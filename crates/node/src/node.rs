//! Running node with event loop and block production.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use shell_consensus::{ConsensusEngine, PoaEngine};
use shell_core::{Block, BlockHeader, SignedTransaction};
use shell_crypto::{DilithiumVerifier, Signer, Verifier};
use shell_evm::{commit_evm_state, ShellEvm, ShellStateDb};
use shell_mempool::TxPool;
use shell_network::NetworkService;
use shell_primitives::{Address, Bytes, ShellHash, U256};
use shell_storage::{ChainStore, KvStore, WorldState};

use crate::config::NodeConfig;
use crate::error::NodeError;

/// A running shell-chain node.
///
/// Orchestrates storage, consensus, EVM, mempool, network, and RPC
/// into a unified event loop with optional block production.
pub struct Node<S: KvStore + 'static> {
    pub config: NodeConfig,
    pub store: Arc<S>,
    pub chain_store: Arc<ChainStore<S>>,
    pub world_state: Arc<RwLock<WorldState<S>>>,
    pub tx_pool: Arc<TxPool>,
    pub consensus: Arc<PoaEngine>,
    /// Known authority public keys for seal verification (Address → PQ pubkey).
    pub known_authorities: Arc<RwLock<HashMap<Address, Vec<u8>>>>,
    shutdown_tx: watch::Sender<bool>,
}

impl<S: KvStore + 'static> Node<S> {
    /// Create a new node from pre-built components.
    pub fn new(
        config: NodeConfig,
        store: Arc<S>,
        chain_store: Arc<ChainStore<S>>,
        world_state: Arc<RwLock<WorldState<S>>>,
        tx_pool: Arc<TxPool>,
        consensus: Arc<PoaEngine>,
    ) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            config,
            store,
            chain_store,
            world_state,
            tx_pool,
            consensus,
            known_authorities: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx,
        }
    }

    /// Register an authority's public key for seal verification.
    pub fn register_authority_pubkey(&self, address: Address, pubkey: Vec<u8>) {
        self.known_authorities.write().insert(address, pubkey);
    }

    /// Signal the node to shut down.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Get a shutdown receiver for external coordination.
    pub fn subscribe_shutdown(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    /// Run the async event loop.
    ///
    /// Drives block production, network event handling, and RPC serving:
    /// - **Block production**: on a timer, if this node is the current proposer,
    ///   produce a block from pending mempool txs and broadcast it.
    /// - **Network events**: import blocks and transactions from peers.
    /// - **RPC server**: serves JSON-RPC on the configured address.
    /// - **Shutdown**: stops on `shutdown()` call or Ctrl-C.
    pub async fn run(
        &self,
        signer: Arc<dyn Signer>,
        network: &mut dyn NetworkService,
    ) -> Result<(), NodeError> {
        use shell_network::{NetworkEvent, NetworkMessage};
        use shell_rpc::start_rpc_server;
        use tokio::time::{interval, Duration};

        // Start JSON-RPC server.
        let (_rpc_addr, _rpc_handle) = start_rpc_server(
            self.config.rpc.clone(),
            self.chain_store.clone(),
            self.world_state.clone(),
            self.tx_pool.clone(),
            self.config.chain_id,
        )
        .await
        .map_err(|e| NodeError::Startup(format!("RPC: {e}")))?;

        // Register own authority pubkey for seal verification.
        if let Some(addr) = self.config.proposer_address {
            self.register_authority_pubkey(addr, signer.public_key().to_vec());
        }

        let mut block_timer = interval(Duration::from_millis(self.config.block_time_ms));
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        // Skip the first immediate tick.
        block_timer.tick().await;

        // Startup sync: request blocks we don't have from peers.
        // Track whether we are catching up so we don't spam requests.
        let mut sync_requested = false;
        if network.peer_count().await > 0 {
            let head_number = self
                .chain_store
                .get_head_block()
                .ok()
                .flatten()
                .map(|b| b.number())
                .unwrap_or(0);
            info!(
                head = head_number,
                "requesting blocks from peers for initial sync"
            );
            let req = NetworkMessage::BlockRequest {
                start_number: head_number + 1,
                count: 128,
            };
            let _ = network.broadcast(req).await;
            sync_requested = true;
        }

        loop {
            tokio::select! {
                _ = block_timer.tick() => {
                    if self.config.proposer_address.is_some() {
                        match self.produce_block(&*signer, 500) {
                            Ok(block) => {
                                let number = block.number();
                                let tx_count = block.transactions.len();
                                let gas = block.header.gas_used;
                                eprintln!(
                                    "⛏  Block #{number} produced ({tx_count} txs, {gas} gas)"
                                );
                                let msg = NetworkMessage::NewBlock(Box::new(block));
                                let _ = network.broadcast(msg).await;
                            }
                            Err(NodeError::NotProposer) => {
                                // Not our turn to propose; silently skip.
                            }
                            Err(e) => {
                                eprintln!("⚠  Block production error: {e}");
                            }
                        }
                    }
                }

                event = network.next_event() => {
                    match event {
                        Some(NetworkEvent::MessageReceived { peer, message }) => {
                            match message {
                                NetworkMessage::NewBlock(block) => {
                                    let verifier = DilithiumVerifier;
                                    match self.import_block(*block, &verifier) {
                                        Ok(()) => {
                                            sync_requested = false;
                                        }
                                        Err(NodeError::GapDetected { .. }) => {
                                            // Only request missing blocks on genuine gap,
                                            // NOT on invalid signatures or other errors (F-037).
                                            let head_num = self
                                                .chain_store
                                                .get_head_block()
                                                .ok()
                                                .flatten()
                                                .map(|b| b.number())
                                                .unwrap_or(0);
                                            if !sync_requested {
                                                info!(
                                                    head = head_num,
                                                    "requesting missing blocks for sync"
                                                );
                                                let req = NetworkMessage::BlockRequest {
                                                    start_number: head_num + 1,
                                                    count: 128,
                                                };
                                                let _ = network.broadcast(req).await;
                                                sync_requested = true;
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!("⚠  Block import error: {e}");
                                        }
                                    }
                                }
                                NetworkMessage::NewTransaction(tx) => {
                                    let verifier = DilithiumVerifier;
                                    match self.handle_incoming_tx(*tx, &verifier) {
                                        Ok(_hash) => {}
                                        Err(e) => eprintln!("⚠  Tx handling error: {e}"),
                                    }
                                }
                                NetworkMessage::BlockRequest { start_number, count } => {
                                    const MAX_BLOCK_RESPONSE: u64 = 128;
                                    let safe_count = count.min(MAX_BLOCK_RESPONSE);
                                    debug!(
                                        %peer,
                                        start_number,
                                        count,
                                        safe_count,
                                        "received BlockRequest"
                                    );
                                    let mut blocks = Vec::new();
                                    for n in start_number..start_number.saturating_add(safe_count) {
                                        match self.chain_store.get_block_by_number(n) {
                                            Ok(Some(block)) => blocks.push(block),
                                            _ => break,
                                        }
                                    }
                                    if !blocks.is_empty() {
                                        info!(
                                            count = blocks.len(),
                                            from = start_number,
                                            "responding with blocks"
                                        );
                                        let resp = NetworkMessage::BlockResponse { blocks };
                                        let _ = network.broadcast(resp).await;
                                    }
                                }
                                NetworkMessage::BlockResponse { blocks } => {
                                    info!(
                                        count = blocks.len(),
                                        "received BlockResponse, importing blocks"
                                    );
                                    let verifier = DilithiumVerifier;
                                    let mut last_ok = 0u64;
                                    for block in blocks {
                                        let num = block.number();
                                        match self.import_block(block, &verifier) {
                                            Ok(()) => {
                                                last_ok = num;
                                                debug!(number = num, "synced block");
                                            }
                                            Err(e) => {
                                                warn!(
                                                    number = num,
                                                    error = %e,
                                                    "block sync import failed"
                                                );
                                                break;
                                            }
                                        }
                                    }
                                    // Request next batch if we imported blocks
                                    // (there may be more to catch up on).
                                    if last_ok > 0 {
                                        let req = NetworkMessage::BlockRequest {
                                            start_number: last_ok + 1,
                                            count: 128,
                                        };
                                        let _ = network.broadcast(req).await;
                                        sync_requested = true;
                                    } else {
                                        sync_requested = false;
                                    }
                                }
                                NetworkMessage::Ping => {
                                    debug!(%peer, "received Ping, responding with Pong");
                                    let _ = network.broadcast(NetworkMessage::Pong).await;
                                }
                                NetworkMessage::Pong => {
                                    debug!(%peer, "received Pong");
                                }
                            }
                        }
                        Some(_) => {} // PeerConnected / PeerDisconnected
                        None => {
                            eprintln!("Network channel closed, shutting down");
                            break;
                        }
                    }
                }

                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        eprintln!("Shutdown signal received");
                        break;
                    }
                }
            }
        }

        let _ = network.shutdown().await;
        Ok(())
    }

    /// Produce a block from pending mempool transactions.
    ///
    /// Collects up to `max_txs` transactions, executes each through the EVM,
    /// commits state changes after every transaction (so subsequent txs see
    /// prior updates), assembles a block, and commits it to storage.
    pub fn produce_block(
        &self,
        signer: &dyn Signer,
        max_txs: usize,
    ) -> Result<Block, NodeError> {
        let head = self
            .chain_store
            .get_head_block()?
            .ok_or(NodeError::NoGenesis)?;
        let head_hash = head.hash();
        let next_number = head.number() + 1;

        let proposer_addr = self
            .config
            .proposer_address
            .ok_or(NodeError::NotProposer)?;

        if !self.consensus.is_proposer(next_number, &proposer_addr) {
            return Err(NodeError::NotProposer);
        }

        // Collect pending transactions from mempool.
        let candidates = self.tx_pool.pending(max_txs);

        // Create an isolated EVM instance at the current state root.
        let current_root = {
            let mut ws = self.world_state.write();
            ws.state_root()?
        };
        let ws = WorldState::at_root(self.store.clone(), &current_root)?;
        let cs = ChainStore::new(self.store.clone());
        let state_db = ShellStateDb::new(ws, cs);
        let mut evm = ShellEvm::new(state_db, self.config.chain_id);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Build a preliminary header for EVM context.
        let mut header = BlockHeader {
            parent_hash: head_hash,
            state_root: ShellHash::default(),
            transactions_root: ShellHash::default(),
            receipts_root: ShellHash::default(),
            logs_bloom: Bytes::default(),
            number: next_number,
            gas_limit: head.header.gas_limit,
            gas_used: 0,
            timestamp: now,
            extra_data: Bytes::default(),
            proposer: proposer_addr,
            sig_aggregate_proof: None,
        };

        let mut included_txs: Vec<SignedTransaction> = Vec::new();
        let mut receipts = Vec::new();
        let mut cumulative_gas: u64 = 0;

        for (idx, tx) in candidates.iter().enumerate() {
            match evm.execute_tx(tx, &header, idx as u32, cumulative_gas) {
                Ok(result) => {
                    cumulative_gas += result.gas_used;
                    receipts.push(result.receipt);
                    included_txs.push(tx.clone());

                    // Commit state changes to the EVM's WorldState so the
                    // next transaction sees updated balances/nonces.
                    commit_evm_state(
                        &result.state_changes,
                        evm.state_db_mut().world_state_mut(),
                        &self.chain_store,
                    )?;

                    // Commit to the node's persistent WorldState.
                    {
                        let mut ws = self.world_state.write();
                        commit_evm_state(
                            &result.state_changes,
                            &mut ws,
                            &self.chain_store,
                        )?;
                    }
                }
                Err(_) => {
                    // Skip failed transactions.
                    continue;
                }
            }

            if cumulative_gas >= header.gas_limit {
                break;
            }
        }

        header.gas_used = cumulative_gas;

        // Compute state root from the updated world state.
        {
            let mut ws = self.world_state.write();
            header.state_root = ws.state_root().unwrap_or_default();
        }

        let mut block = Block {
            header,
            transactions: included_txs.clone(),
            proposer_seal: None,
        };

        // Sign the block with the proposer's key.
        self.consensus.sign_block(&mut block, signer)?;

        // Register the signer's pubkey so we can verify our own blocks on re-import.
        self.register_authority_pubkey(proposer_addr, signer.public_key().to_vec());

        // Commit to storage.
        let block_hash = block.hash();
        self.chain_store.put_block(&block)?;
        self.chain_store
            .put_receipts(&block_hash, &receipts)?;
        self.chain_store
            .set_canonical(block.number(), &block_hash)?;
        self.chain_store.set_head(&block_hash)?;

        // Remove included transactions from mempool.
        let tx_hashes: Vec<ShellHash> = included_txs.iter().map(|tx| tx.hash()).collect();
        self.tx_pool.remove_batch(&tx_hashes);

        Ok(block)
    }

    /// Import and validate a block received from the network.
    ///
    /// Re-executes all transactions through the EVM and commits state
    /// changes to WorldState, then stores the block.
    ///
    /// Fork detection: if the incoming block is at the same height as
    /// the current head but with a different hash, it is treated as a
    /// potential fork and skipped. If there is a gap (block number is
    /// more than one ahead of head), missing blocks are requested.
    pub fn import_block(
        &self,
        block: Block,
        _verifier: &dyn Verifier,
    ) -> Result<(), NodeError> {
        let head = self
            .chain_store
            .get_head_block()?
            .ok_or(NodeError::NoGenesis)?;

        let expected = head.number() + 1;
        let incoming = block.number();

        // Fork detection: same height, different hash.
        if incoming == head.number() && block.hash() != head.hash() {
            warn!(
                number = incoming,
                local_hash = %head.hash(),
                remote_hash = %block.hash(),
                "potential fork detected at same height, skipping import"
            );
            return Ok(());
        }

        // Duplicate of current head — already have it.
        if incoming <= head.number() {
            debug!(
                incoming,
                head = head.number(),
                "ignoring block at or below current head"
            );
            return Ok(());
        }

        // Gap detection: block is too far ahead.
        if incoming > expected {
            warn!(
                incoming,
                expected,
                gap = incoming - expected,
                "block too far ahead, missing blocks need to be requested"
            );
            return Err(NodeError::GapDetected {
                incoming,
                expected,
            });
        }

        // Verify consensus rules.
        self.consensus.verify_header(&block.header)?;

        // Verify proposer seal (PQ signature).
        match &block.proposer_seal {
            Some(seal) => {
                let proposer = &block.header.proposer;
                let known = self.known_authorities.read();
                if let Some(pubkey) = known.get(proposer) {
                    let verifier = DilithiumVerifier;
                    self.consensus.verify_seal(
                        &block.header,
                        seal,
                        pubkey,
                        &verifier,
                    )?;
                } else {
                    // Try chain store as fallback.
                    drop(known);
                    if let Ok(Some(pubkey)) = self.chain_store.get_pubkey(proposer) {
                        let verifier = DilithiumVerifier;
                        self.consensus.verify_seal(
                            &block.header,
                            seal,
                            &pubkey,
                            &verifier,
                        )?;
                        // Cache for future lookups.
                        self.known_authorities
                            .write()
                            .insert(*proposer, pubkey);
                    } else {
                        warn!(
                            proposer = %proposer,
                            block = block.number(),
                            "seal present but proposer pubkey unknown, skipping verification (M1b)"
                        );
                    }
                }
            }
            None => {
                warn!(
                    block = block.number(),
                    proposer = %block.header.proposer,
                    "imported block has no proposer seal (M1b: allowed, will be strict in M2)"
                );
            }
        }

        // Re-execute transactions and commit state changes.
        if !block.transactions.is_empty() {
            let current_root = {
                let mut ws = self.world_state.write();
                ws.state_root()?
            };
            let ws = WorldState::at_root(self.store.clone(), &current_root)?;
            let cs = ChainStore::new(self.store.clone());
            let state_db = ShellStateDb::new(ws, cs);
            let mut evm = ShellEvm::new(state_db, self.config.chain_id);
            let mut cumulative_gas: u64 = 0;

            for (idx, tx) in block.transactions.iter().enumerate() {
                match evm.execute_tx(tx, &block.header, idx as u32, cumulative_gas) {
                    Ok(result) => {
                        cumulative_gas += result.gas_used;

                        commit_evm_state(
                            &result.state_changes,
                            evm.state_db_mut().world_state_mut(),
                            &self.chain_store,
                        )?;

                        let mut ws = self.world_state.write();
                        commit_evm_state(
                            &result.state_changes,
                            &mut ws,
                            &self.chain_store,
                        )?;
                    }
                    Err(e) => {
                        return Err(NodeError::Startup(format!(
                            "tx {} re-execution failed: {e}",
                            idx
                        )));
                    }
                }
            }
        }

        // Commit to storage.
        let block_hash = block.hash();
        self.chain_store.put_block(&block)?;
        self.chain_store
            .set_canonical(block.number(), &block_hash)?;
        self.chain_store.set_head(&block_hash)?;

        // Remove any included transactions from our mempool.
        let tx_hashes: Vec<ShellHash> =
            block.transactions.iter().map(|tx| tx.hash()).collect();
        self.tx_pool.remove_batch(&tx_hashes);

        Ok(())
    }

    /// Handle a transaction received from the network.
    pub fn handle_incoming_tx(
        &self,
        tx: SignedTransaction,
        _verifier: &dyn Verifier,
    ) -> Result<ShellHash, NodeError> {
        let chain_store = &self.chain_store;
        let world_state_guard = self.world_state.read();

        let known_pubkeys = |addr: &Address| -> Option<Vec<u8>> {
            chain_store.get_pubkey(addr).ok().flatten()
        };
        let balance_of = |addr: &Address| -> U256 {
            world_state_guard.get_balance(addr).unwrap_or(U256::ZERO)
        };

        let dv = DilithiumVerifier;
        let hash = self
            .tx_pool
            .insert(tx, &dv, &known_pubkeys, &balance_of)
            .map_err(|e| NodeError::Startup(e.to_string()))?;

        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_consensus::PoaConfig;
    use shell_core::Transaction;
    use shell_crypto::DilithiumSigner;
    use shell_mempool::MempoolConfig;
    use shell_storage::MemoryDb;

    fn setup_node() -> (Node<MemoryDb>, DilithiumSigner) {
        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let authority = Address::from_public_key(&pubkey);

        let db = Arc::new(MemoryDb::new());
        let chain_store = Arc::new(ChainStore::new(db.clone()));
        let world_state = Arc::new(RwLock::new(WorldState::new(db.clone())));
        let consensus = Arc::new(PoaEngine::new(PoaConfig::new(vec![authority], 1)));
        let tx_pool = Arc::new(TxPool::new(MempoolConfig {
            chain_id: 1337,
            ..MempoolConfig::default()
        }));

        let config = NodeConfig::dev(authority);
        let node = Node::new(config, db, chain_store, world_state, tx_pool, consensus);
        (node, signer)
    }

    fn store_genesis(node: &Node<MemoryDb>) {
        let genesis = Block {
            header: BlockHeader {
                parent_hash: ShellHash::default(),
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::default(),
                number: 0,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_000,
                extra_data: Bytes::default(),
                proposer: node.config.proposer_address.unwrap(),
                sig_aggregate_proof: None,
            },
            transactions: vec![],
            proposer_seal: None,
        };
        let hash = genesis.hash();
        node.chain_store.put_block(&genesis).unwrap();
        node.chain_store.set_canonical(0, &hash).unwrap();
        node.chain_store.set_head(&hash).unwrap();
    }

    fn fund_account(node: &Node<MemoryDb>, addr: &Address, balance: U256) {
        let account = shell_core::Account {
            pq_pubkey_hash: ShellHash::default(),
            nonce: 0,
            balance,
            validation_code_hash: None,
            code_hash: None,
            storage_root: ShellHash::default(),
        };
        let mut ws = node.world_state.write();
        ws.set_account(addr, &account).unwrap();
    }

    #[test]
    fn node_creation() {
        let (node, _signer) = setup_node();
        assert_eq!(node.config.chain_id, 1337);
        assert!(node.config.proposer_address.is_some());
    }

    #[test]
    fn produce_empty_block() {
        let (node, signer) = setup_node();
        store_genesis(&node);

        let block = node.produce_block(&signer, 100).unwrap();
        assert_eq!(block.number(), 1);
        assert!(block.transactions.is_empty());
        assert!(block.proposer_seal.is_some());
    }

    #[test]
    fn produce_block_commits_state() {
        let (node, signer) = setup_node();
        store_genesis(&node);

        // Create sender and receiver
        let tx_signer = DilithiumSigner::generate();
        let sender = Address::from_public_key(tx_signer.public_key());
        let receiver = Address::from([0xBB; 20]);
        let transfer_value = U256::from(1_000_000);

        // Fund sender
        fund_account(&node, &sender, U256::from(10_000_000_000u64));

        // Verify initial balances
        {
            let ws = node.world_state.read();
            assert_eq!(ws.get_balance(&sender).unwrap(), U256::from(10_000_000_000u64));
            assert_eq!(ws.get_balance(&receiver).unwrap(), U256::ZERO);
        }

        // Create and submit a transfer transaction
        let tx = Transaction {
            chain_id: 1337,
            nonce: 0,
            to: Some(receiver),
            value: transfer_value,
            data: shell_primitives::Bytes::new(),
            gas_limit: 21_000,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
        };

        // Sign with real Dilithium key
        let tx_hash = {
            let encoded = alloy_rlp::encode(&tx);
            shell_primitives::keccak256(&encoded)
        };
        let sig = tx_signer.sign(tx_hash.as_bytes()).expect("sign failed");
        let signed = SignedTransaction::with_pubkey(
            sender,
            tx,
            sig,
            tx_signer.public_key().to_vec(),
        );

        // Insert into mempool with real verification
        let verifier = DilithiumVerifier;
        let known_pubkeys = |_: &Address| -> Option<Vec<u8>> { None };
        let balance_of = |addr: &Address| -> U256 {
            node.world_state.read().get_balance(addr).unwrap_or(U256::ZERO)
        };
        node.tx_pool
            .insert(signed, &verifier, &known_pubkeys, &balance_of)
            .unwrap();

        // Produce block with the transfer
        let block = node.produce_block(&signer, 100).unwrap();
        assert_eq!(block.number(), 1);
        assert_eq!(block.transactions.len(), 1);

        // Verify state was committed: receiver got funds
        {
            let ws = node.world_state.read();
            let receiver_balance = ws.get_balance(&receiver).unwrap();
            assert_eq!(receiver_balance, transfer_value, "receiver should have received the transfer");

            // Sender balance should have decreased (value transferred + gas)
            let sender_balance = ws.get_balance(&sender).unwrap();
            assert!(
                sender_balance < U256::from(10_000_000_000u64),
                "sender balance should decrease after transfer"
            );
        }

        // State root should be non-default (state was modified)
        assert_ne!(
            block.header.state_root,
            ShellHash::default(),
            "state root should reflect committed state"
        );
    }

    #[test]
    fn import_block() {
        let (node, _signer) = setup_node();
        store_genesis(&node);

        let block = Block {
            header: BlockHeader {
                parent_hash: node.chain_store.get_head_hash().unwrap().unwrap(),
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::default(),
                number: 1,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_001,
                extra_data: Bytes::default(),
                proposer: node.config.proposer_address.unwrap(),
                sig_aggregate_proof: None,
            },
            transactions: vec![],
            proposer_seal: None,
        };

        let verifier = DilithiumVerifier;
        node.import_block(block, &verifier).unwrap();

        let head = node.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(head.number(), 1);
    }

    #[test]
    fn import_block_with_valid_seal() {
        let (node, signer) = setup_node();
        store_genesis(&node);
        let proposer = node.config.proposer_address.unwrap();

        // Register authority pubkey so seal verification runs.
        node.register_authority_pubkey(proposer, signer.public_key().to_vec());

        // Produce a properly signed block and re-import it on a fresh node.
        let block = node.produce_block(&signer, 100).unwrap();
        assert!(block.proposer_seal.is_some());

        // Set up a second node sharing storage to import the block.
        let node2_db = Arc::new(MemoryDb::new());
        let node2_cs = Arc::new(ChainStore::new(node2_db.clone()));
        let node2_ws = Arc::new(RwLock::new(WorldState::new(node2_db.clone())));
        let consensus = Arc::new(PoaEngine::new(PoaConfig::new(vec![proposer], 1)));
        let tx_pool = Arc::new(TxPool::new(MempoolConfig {
            chain_id: 1337,
            ..MempoolConfig::default()
        }));
        let config = NodeConfig::dev(proposer);
        let node2 = Node::new(config, node2_db, node2_cs, node2_ws, tx_pool, consensus);
        store_genesis(&node2);

        // Register authority pubkey on node2.
        node2.register_authority_pubkey(proposer, signer.public_key().to_vec());

        let verifier = DilithiumVerifier;
        node2.import_block(block, &verifier).unwrap();

        let head = node2.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(head.number(), 1);
    }

    #[test]
    fn import_block_with_invalid_seal_rejected() {
        let (node, signer) = setup_node();
        store_genesis(&node);
        let proposer = node.config.proposer_address.unwrap();

        // Register authority pubkey.
        node.register_authority_pubkey(proposer, signer.public_key().to_vec());

        let mut block = node.produce_block(&signer, 100).unwrap();

        // Corrupt the seal.
        if let Some(ref mut seal) = block.proposer_seal {
            seal.data[0] ^= 0xFF;
        }

        // Set up a second node to import the corrupted block.
        let node2_db = Arc::new(MemoryDb::new());
        let node2_cs = Arc::new(ChainStore::new(node2_db.clone()));
        let node2_ws = Arc::new(RwLock::new(WorldState::new(node2_db.clone())));
        let consensus = Arc::new(PoaEngine::new(PoaConfig::new(vec![proposer], 1)));
        let tx_pool = Arc::new(TxPool::new(MempoolConfig {
            chain_id: 1337,
            ..MempoolConfig::default()
        }));
        let config = NodeConfig::dev(proposer);
        let node2 = Node::new(config, node2_db, node2_cs, node2_ws, tx_pool, consensus);
        store_genesis(&node2);
        node2.register_authority_pubkey(proposer, signer.public_key().to_vec());

        let verifier = DilithiumVerifier;
        let result = node2.import_block(block, &verifier);
        assert!(result.is_err(), "block with invalid seal should be rejected");
    }

    #[test]
    fn import_block_without_seal_allowed_m1b() {
        // In M1b, blocks without a seal are allowed with a warning.
        let (node, _signer) = setup_node();
        store_genesis(&node);

        let block = Block {
            header: BlockHeader {
                parent_hash: node.chain_store.get_head_hash().unwrap().unwrap(),
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::default(),
                number: 1,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_001,
                extra_data: Bytes::default(),
                proposer: node.config.proposer_address.unwrap(),
                sig_aggregate_proof: None,
            },
            transactions: vec![],
            proposer_seal: None,
        };

        let verifier = DilithiumVerifier;
        // Should succeed despite missing seal (M1b tolerance).
        node.import_block(block, &verifier).unwrap();
        let head = node.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(head.number(), 1);
    }

    #[test]
    fn produce_block_registers_authority_pubkey() {
        let (node, signer) = setup_node();
        store_genesis(&node);

        let proposer = node.config.proposer_address.unwrap();
        assert!(node.known_authorities.read().get(&proposer).is_none());

        node.produce_block(&signer, 100).unwrap();

        let known = node.known_authorities.read();
        let pubkey = known.get(&proposer).expect("pubkey should be registered after produce_block");
        assert_eq!(pubkey, signer.public_key());
    }

    #[test]
    fn shutdown_signal() {
        let (node, _signer) = setup_node();
        let rx = node.shutdown_tx.subscribe();
        assert!(!*rx.borrow());

        node.shutdown();
        assert!(*rx.borrow());
    }

    #[tokio::test]
    async fn event_loop_produces_blocks() {
        use shell_network::{NetworkBus, NetworkConfig};
        use std::time::Duration;

        let (node, signer) = setup_node();
        store_genesis(&node);

        let bus = NetworkBus::new(64);
        let mut network = bus.join(&NetworkConfig::default());

        let node = Arc::new(node);
        let node_clone = node.clone();
        let signer = Arc::new(signer) as Arc<dyn Signer>;

        // Spawn the event loop in a background task.
        let handle = tokio::spawn(async move {
            // Use a very short block time for testing.
            // We can't mutate config directly, so we test with the default.
            node_clone.run(signer, &mut network).await
        });

        // Wait for at least 3 blocks to be produced (~6s with 2s block_time).
        tokio::time::sleep(Duration::from_secs(7)).await;

        // Shut down the node.
        node.shutdown();
        let result = handle.await.expect("task panicked");
        assert!(result.is_ok(), "run() returned error: {:?}", result.err());

        // Verify blocks were produced.
        let head = node.chain_store.get_head_block().unwrap().unwrap();
        assert!(
            head.number() >= 3,
            "expected at least 3 blocks, got {}",
            head.number()
        );
    }
}
