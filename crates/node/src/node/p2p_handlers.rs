use super::*;

impl<S: KvStore + 'static> Node<S> {
    /// Handle a transaction received from the network.
    pub fn handle_incoming_tx(
        &self,
        tx: SignedTransaction,
        _verifier: &dyn Verifier,
    ) -> Result<ShellHash, NodeError> {
        let chain_store = &self.chain_store;
        let mut world_state_guard = self.world_state.write();

        let dv = MultiVerifier;
        let hash = self
            .tx_pool
            .insert(tx, &mut world_state_guard, chain_store.as_ref(), &dv)
            .map_err(|e| NodeError::Startup(e.to_string()))?;

        Ok(hash)
    }

    /// Process an incoming attestation from the network.
    pub fn handle_attestation(
        &self,
        attestation: Attestation,
        verifier: &dyn Verifier,
    ) -> Result<(), NodeError> {
        let block_hash = attestation.block_hash;
        let block_number = attestation.block_number;
        let validator = attestation.validator;

        // F-087: Verify the attested block exists in our local chain store.
        // If unknown, log and skip — the block may arrive later via sync.
        match self.chain_store.get_block_by_hash(&block_hash) {
            Ok(Some(_)) => {}
            Ok(None) => {
                tracing::warn!(
                    %block_hash,
                    block_number,
                    %validator,
                    "attestation for unknown block — skipping (may arrive via sync)"
                );
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(
                    %block_hash,
                    error = %e,
                    "failed to check block existence for attestation"
                );
                return Ok(());
            }
        }

        // Verify the attesting validator is a known authority.
        let known = self.known_authorities.read();
        let pubkey = known.get(&validator).ok_or_else(|| {
            NodeError::Startup(format!("unknown attestation validator: {:?}", validator))
        })?;

        // Verify the attestation signature.
        let msg = Attestation::signing_message(&block_hash, block_number);
        let sig = shell_crypto::PQSignature::new(
            shell_crypto::SignatureType::Dilithium3,
            attestation.signature.clone(),
        );
        let valid = verifier
            .verify(pubkey, &msg, &sig)
            .map_err(|_| NodeError::Startup("invalid attestation signature".into()))?;
        if !valid {
            return Err(NodeError::Startup(
                "attestation signature verification failed".into(),
            ));
        }

        // Check for equivocation.
        let mut finality = self.finality.write();
        if let Some(conflicting) =
            finality.detect_equivocation(&block_hash, block_number, &validator)
        {
            tracing::error!(
                %validator,
                %block_hash,
                %conflicting,
                height = block_number,
                "equivocation detected — rejecting attestation"
            );
            return Err(NodeError::Startup(format!(
                "equivocation: validator {validator:?} already attested to {conflicting:?} at height {block_number}"
            )));
        }

        // Record the attestation.
        if !finality.record_attestation(attestation) {
            return Ok(()); // duplicate, already recorded
        }

        // Check if this block reached finality.
        let total_validators = self.consensus.read().poa_config().authorities.len();
        if finality.check_finality(&block_hash, block_number, total_validators) {
            tracing::info!(
                block = block_number,
                hash = %block_hash,
                "block finalized"
            );
            let _ = self.chain_store.set_finalized_number(block_number);
            // F-088: Prune fork choice data for old blocks to prevent unbounded growth.
            let mut fc = self.fork_choice.write();
            fc.mark_finalized(&block_hash);
            fc.prune_below(block_number);
        }

        Ok(())
    }

    /// Create and return an attestation for a block (called after producing/importing a block).
    pub fn create_attestation(
        &self,
        block_hash: ShellHash,
        block_number: u64,
        signer: &dyn Signer,
    ) -> Result<Attestation, NodeError> {
        let proposer_addr = self.config.proposer_address.ok_or(NodeError::NotProposer)?;

        let msg = Attestation::signing_message(&block_hash, block_number);
        let sig = signer
            .sign(&msg)
            .map_err(|e| NodeError::Startup(format!("failed to sign attestation: {e}")))?;

        Ok(Attestation::new(
            block_hash,
            block_number,
            proposer_addr,
            sig.data,
        ))
    }

    /// W.5: Handle an incoming wPoA vote from a peer.
    ///
    /// Reconstructs the PQ signature, validates the voter, records the vote,
    /// and logs when quorum is reached.
    pub fn handle_wpoa_vote(
        &self,
        voter: Address,
        block_hash: ShellHash,
        block_number: u64,
        signature: Vec<u8>,
    ) {
        let sig =
            shell_crypto::PQSignature::new(shell_crypto::SignatureType::Dilithium3, signature);

        // FF.6: Drop votes for blocks that have already been finalized at a different hash
        // (stale or conflicting vote). Penalise the sender.
        {
            let finality = self.finality.read();
            let fin_number = finality.last_finalized_number();
            if fin_number > 0 && block_number <= fin_number {
                // Check if the vote is for the same hash as the finalized block.
                let fin_hash_at_height = self
                    .chain_store
                    .get_block_by_number(block_number)
                    .ok()
                    .flatten()
                    .map(|b| b.hash());
                if fin_hash_at_height.as_ref() != Some(&block_hash) {
                    tracing::warn!(
                        block_number,
                        %block_hash,
                        fin_number,
                        %voter,
                        "FF.6: vote for finalized block with wrong hash — dropping and penalising"
                    );
                    let peer_id =
                        shell_consensus::ScoringPeerId::from(format!("{voter:?}"));
                    self.peer_scorer.lock().record_event(
                        &peer_id,
                        shell_consensus::PeerEvent::InvalidProofPayload,
                    );
                    return;
                }
            }
        }

        let mut guard = self.wpoa_round.lock();
        if let Some(ref mut round) = *guard {
            if round.block_number != block_number {
                tracing::debug!(
                    block_number,
                    expected = round.block_number,
                    "W.5: WPoaVote for unexpected block number, ignoring"
                );
                return;
            }
            let peer_id = shell_consensus::ScoringPeerId::from(format!("{voter:?}"));
            let events = round.on_vote(voter, block_hash, sig);
            for event in events {
                match event {
                    WPoaEvent::BlockCommitted {
                        block_hash,
                        quorum_signatures,
                    } => {
                        tracing::info!(
                            %block_hash,
                            block_number,
                            signers = quorum_signatures.len(),
                            "W.5: block committed with quorum"
                        );
                        // PS.1: reward all quorum signers.
                        {
                            let mut scorer = self.peer_scorer.lock();
                            for signer in quorum_signatures.keys() {
                                let signer_id =
                                    shell_consensus::ScoringPeerId::from(format!("{signer:?}"));
                                scorer.record_event(
                                    &signer_id,
                                    shell_consensus::PeerEvent::ValidProofDelivered,
                                );
                            }
                        }
                        // FF.1 / FF.3: Advance finality and persist.
                        // The round state machine already verified weight-based quorum,
                        // so BlockCommitted IS the finality signal.  Verify the block
                        // is locally canonical before finalizing (safety guard).
                        let locally_canonical = self
                            .chain_store
                            .get_block_by_number(block_number)
                            .ok()
                            .flatten()
                            .map(|b| b.hash() == block_hash)
                            .unwrap_or(false);

                        if locally_canonical {
                            let advanced = self
                                .finality
                                .write()
                                .set_finalized_direct(block_number, block_hash);
                            if advanced {
                                tracing::info!(
                                    block_number,
                                    %block_hash,
                                    "FF: block finalized"
                                );
                                if let Err(e) =
                                    self.chain_store.set_finalized_number(block_number)
                                {
                                    tracing::warn!(
                                        block_number,
                                        error = %e,
                                        "FF: failed to persist finalized number"
                                    );
                                }
                                // FF.2: Store commit certificate sidecar.
                                // Encode quorum_signatures as JSON: {address_hex -> sig_hex}.
                                let cert: std::collections::HashMap<String, String> =
                                    quorum_signatures
                                        .iter()
                                        .map(|(addr, sig)| {
                                            (format!("{addr:?}"), hex::encode(&sig.data))
                                        })
                                        .collect();
                                match serde_json::to_vec(&cert) {
                                    Ok(encoded) => {
                                        if let Err(e) = self
                                            .chain_store
                                            .set_commit_certificate(&block_hash, &encoded)
                                        {
                                            tracing::warn!(
                                                %block_hash,
                                                error = %e,
                                                "FF.2: failed to store commit certificate"
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            %block_hash,
                                            error = %e,
                                            "FF.2: failed to encode commit certificate"
                                        );
                                    }
                                }
                            }
                        } else {
                            tracing::warn!(
                                block_number,
                                %block_hash,
                                "FF: BlockCommitted but block not locally canonical — \
                                 finality deferred until block is imported"
                            );
                        }
                    }
                    WPoaEvent::DuplicateVote { voter } => {
                        tracing::warn!(%voter, "W.5: duplicate vote rejected");
                        // PS.1: penalise duplicate voter.
                        self.peer_scorer
                            .lock()
                            .record_event(&peer_id, shell_consensus::PeerEvent::DuplicateMessage);
                    }
                    WPoaEvent::WrongBlockHash { expected, got } => {
                        tracing::warn!(%expected, %got, "W.5: vote for wrong block hash rejected");
                        // PS.1: penalise invalid payload.
                        self.peer_scorer.lock().record_event(
                            &peer_id,
                            shell_consensus::PeerEvent::InvalidProofPayload,
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    /// PS.2: Flush wPoA peer scorer to the network-level ban list.
    ///
    /// Any peer whose score has fallen below `disconnect_threshold` is
    /// recorded as a violation in the `PeerBanList`. After `ban_threshold`
    /// violations the network layer will refuse connections from that peer.
    /// Called from the event loop after each wPoA vote round completes.
    pub fn flush_scorer_bans(&self) {
        let scorer = self.peer_scorer.lock();
        let to_disconnect = scorer.peers_to_disconnect();
        if to_disconnect.is_empty() {
            return;
        }
        let mut ban_list = self.peer_ban_list.lock();
        for scoring_peer in to_disconnect {
            let net_peer = shell_network::PeerId(scoring_peer.0.clone());
            let was_banned = ban_list.record_violation(&net_peer);
            if was_banned {
                tracing::warn!(
                    peer = %scoring_peer.0,
                    "PS.2: peer score below threshold — recorded ban violation (now banned)"
                );
            } else {
                tracing::debug!(
                    peer = %scoring_peer.0,
                    "PS.2: peer score below threshold — recorded violation"
                );
            }
        }
    }

    /// W.5: Handle an incoming wPoA view-change vote from a peer.
    ///
    /// Records the vote and logs when quorum for the view change is reached.
    pub fn handle_wpoa_view_change(&self, voter: Address, new_view: u64, block_number: u64) {
        let mut guard = self.wpoa_round.lock();
        if let Some(ref mut round) = *guard {
            if round.block_number != block_number {
                return;
            }
            let events = round.on_view_change_vote(voter, new_view);
            for event in events {
                if let WPoaEvent::ViewChangeReady { new_view } = event {
                    tracing::info!(new_view, "W.5: view change ready — advancing round");
                    round.round = new_view;
                }
            }
        }
    }
}
