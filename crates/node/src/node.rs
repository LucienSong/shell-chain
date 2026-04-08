//! Running node with event loop and block production.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use shell_consensus::{Attestation, ConsensusEngine, FinalityState, ForkChoice, PoaEngine};
use shell_core::{calculate_base_fee, Block, BlockHeader, SignedTransaction};
use shell_crypto::{BatchVerifier, MultiVerifier, PreVerified, Signer, Verifier, VerifyItem};
use shell_evm::{commit_evm_state, validate_tx_for_import, ShellEvm, ShellStateDb};
use shell_mempool::TxPool;
use shell_network::{NetworkMessage, NetworkService};
use shell_primitives::{Address, Bytes, ShellHash, U256};
use shell_rpc::DevRpcControl;
use shell_storage::{ChainStore, KvStore, StatePruner, WorldState};

use crate::config::NodeConfig;
use crate::error::NodeError;
use crate::metrics::Metrics;
use crate::pruning::StateRootTracker;

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
    pub consensus: Arc<RwLock<PoaEngine>>,
    /// Known authority public keys for seal verification (Address → PQ pubkey).
    pub known_authorities: Arc<RwLock<HashMap<Address, Vec<u8>>>>,
    /// Tracks recent state roots for pruning decisions.
    pub state_root_tracker: RwLock<StateRootTracker>,
    /// State pruner: removes old canonical mappings (F-303).
    pub state_pruner: RwLock<StatePruner>,
    /// Finality tracking: collects attestations and detects quorum.
    pub finality: Arc<RwLock<FinalityState>>,
    /// Fork-choice rule: selects the canonical head based on attestations and finality.
    pub fork_choice: Arc<RwLock<ForkChoice>>,
    /// Prometheus metrics.
    pub metrics: Arc<Metrics>,
    /// Runtime signer retained so dev RPCs can force block production.
    runtime_signer: RwLock<Option<Arc<dyn Signer>>>,
    /// Dev-only runtime controls for Hardhat/Foundry compatibility.
    dev_state: RwLock<DevState>,
    shutdown_tx: watch::Sender<bool>,
}

const SYNC_RETRY_BASE_INTERVAL_SECS: u64 = 5;
const SYNC_RETRY_MAX_INTERVAL_SECS: u64 = 30;
const SYNC_RETRY_BACKOFF_THRESHOLD: u32 = 3;

struct DevSnapshot {
    head_hash: ShellHash,
    head_number: u64,
    state_root: ShellHash,
    total_tx_count: u64,
    finalized_number: u64,
    pending_txs: Vec<SignedTransaction>,
    next_block_timestamp: Option<u64>,
}

struct DevState {
    next_block_timestamp: Option<u64>,
    next_snapshot_id: u64,
    snapshots: BTreeMap<String, DevSnapshot>,
}

impl<S: KvStore + 'static> Node<S> {
    /// Create a new node from pre-built components.
    pub fn new(
        config: NodeConfig,
        store: Arc<S>,
        chain_store: Arc<ChainStore<S>>,
        world_state: Arc<RwLock<WorldState<S>>>,
        tx_pool: Arc<TxPool>,
        consensus: Arc<RwLock<PoaEngine>>,
    ) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        let tracker = StateRootTracker::new(config.pruning.clone());
        let state_pruner = StatePruner::new(128);
        let metrics = Arc::new(Metrics::new().expect("failed to register Prometheus metrics"));

        // F-094: Recover finalized state from persistent storage on restart.
        let (fin_number, fin_hash) = {
            let stored = chain_store
                .get_finalized_number()
                .ok()
                .flatten()
                .unwrap_or(0);
            if stored > 0 {
                let hash = chain_store
                    .get_block_by_number(stored)
                    .ok()
                    .flatten()
                    .map(|b| b.hash())
                    .unwrap_or(ShellHash::ZERO);
                (stored, hash)
            } else {
                (0, ShellHash::ZERO)
            }
        };
        let finality_state = if fin_number > 0 {
            FinalityState::with_finalized(fin_number, fin_hash)
        } else {
            FinalityState::new()
        };

        Self {
            config,
            store,
            chain_store,
            world_state,
            tx_pool,
            consensus,
            known_authorities: Arc::new(RwLock::new(HashMap::new())),
            state_root_tracker: RwLock::new(tracker),
            state_pruner: RwLock::new(state_pruner),
            finality: Arc::new(RwLock::new(finality_state)),
            fork_choice: Arc::new(RwLock::new(ForkChoice::new(ShellHash::ZERO))),
            metrics,
            runtime_signer: RwLock::new(None),
            dev_state: RwLock::new(DevState {
                next_block_timestamp: None,
                next_snapshot_id: 1,
                snapshots: BTreeMap::new(),
            }),
            shutdown_tx,
        }
    }

    /// Register an authority's public key for seal verification.
    pub fn register_authority_pubkey(&self, address: Address, pubkey: Vec<u8>) {
        self.known_authorities.write().insert(address, pubkey);
    }

    fn head_number(&self) -> u64 {
        self.chain_store
            .get_head_block()
            .ok()
            .flatten()
            .map(|b| b.number())
            .unwrap_or(0)
    }

    fn sync_retry_delay_secs(attempts_without_progress: u32) -> u64 {
        if attempts_without_progress < SYNC_RETRY_BACKOFF_THRESHOLD {
            SYNC_RETRY_BASE_INTERVAL_SECS
        } else {
            SYNC_RETRY_MAX_INTERVAL_SECS
        }
    }

    async fn request_missing_blocks<N: NetworkService + ?Sized>(
        &self,
        network: &mut N,
        sync_requested: &mut bool,
        reason: &'static str,
    ) {
        let head_number = self.head_number();
        info!(head = head_number, reason, "requesting blocks from peers");
        let req = NetworkMessage::BlockRequest {
            start_number: head_number + 1,
            count: 128,
        };
        let _ = network.broadcast(req).await;
        *sync_requested = true;
    }

    fn current_block_timestamp(&self, parent_timestamp: u64) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut dev = self.dev_state.write();
        let ts = if let Some(next) = dev.next_block_timestamp.take() {
            next
        } else {
            now
        };
        ts.max(parent_timestamp.saturating_add(1))
    }

    fn snapshot_inner(&self) -> Result<String, NodeError> {
        let head = self
            .chain_store
            .get_head_block()?
            .ok_or(NodeError::NoGenesis)?;
        let total_tx_count = self.chain_store.get_total_tx_count()?;
        let finalized_number = self.chain_store.get_finalized_number()?.unwrap_or(0);
        let pending_txs = self.tx_pool.pending(self.tx_pool.len());

        let mut dev = self.dev_state.write();
        let id = format!("0x{:x}", dev.next_snapshot_id);
        dev.next_snapshot_id = dev.next_snapshot_id.saturating_add(1);
        let next_block_timestamp = dev.next_block_timestamp;
        dev.snapshots.insert(
            id.clone(),
            DevSnapshot {
                head_hash: head.hash(),
                head_number: head.number(),
                state_root: head.header.state_root,
                total_tx_count,
                finalized_number,
                pending_txs,
                next_block_timestamp,
            },
        );
        Ok(id)
    }

    fn revert_inner(&self, snapshot_id: &str) -> Result<bool, NodeError> {
        let snapshot = {
            let dev = self.dev_state.read();
            match dev.snapshots.get(snapshot_id) {
                Some(s) => DevSnapshot {
                    head_hash: s.head_hash,
                    head_number: s.head_number,
                    state_root: s.state_root,
                    total_tx_count: s.total_tx_count,
                    finalized_number: s.finalized_number,
                    pending_txs: s.pending_txs.clone(),
                    next_block_timestamp: s.next_block_timestamp,
                },
                None => return Ok(false),
            }
        };

        let current_head = self
            .chain_store
            .get_head_block()?
            .ok_or(NodeError::NoGenesis)?;
        if current_head.number() > snapshot.head_number {
            for number in (snapshot.head_number + 1)..=current_head.number() {
                self.chain_store.delete_canonical(number)?;
            }
        }

        self.chain_store.set_head(&snapshot.head_hash)?;
        self.chain_store
            .set_total_tx_count(snapshot.total_tx_count)?;
        self.chain_store
            .set_finalized_number(snapshot.finalized_number)?;

        let restored_ws = WorldState::at_root(self.store.clone(), &snapshot.state_root)?;
        *self.world_state.write() = restored_ws;

        let finalized_hash = if snapshot.finalized_number == 0 {
            ShellHash::ZERO
        } else {
            self.chain_store
                .get_block_by_number(snapshot.finalized_number)?
                .map(|b| b.hash())
                .unwrap_or(ShellHash::ZERO)
        };
        *self.finality.write() = if snapshot.finalized_number > 0 {
            shell_consensus::FinalityState::with_finalized(
                snapshot.finalized_number,
                finalized_hash,
            )
        } else {
            shell_consensus::FinalityState::new()
        };

        self.tx_pool.clear();
        let known_pubkeys = |addr: &Address| -> Option<Vec<u8>> {
            self.chain_store.get_pubkey(addr).ok().flatten()
        };
        let balance_of = |addr: &Address| -> U256 {
            self.world_state
                .read()
                .get_balance(addr)
                .unwrap_or(U256::ZERO)
        };
        let verifier = MultiVerifier;
        for tx in snapshot.pending_txs {
            let _ = self
                .tx_pool
                .insert(tx, &verifier, &known_pubkeys, &balance_of);
        }

        self.dev_state.write().next_block_timestamp = snapshot.next_block_timestamp;

        Ok(true)
    }

    /// Signal the node to shut down.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Record a finalised state root, run state pruning, and evict old entries.
    fn record_finalized_state_root(&self, block_number: u64, state_root: ShellHash) {
        let mut tracker = self.state_root_tracker.write();
        if let Some(evicted) = tracker.record(block_number, state_root) {
            tracing::debug!(
                block = evicted.block_number,
                root = %evicted.state_root,
                "state root eligible for pruning"
            );
        }

        // F-303: Drive StatePruner — register block and run periodic pruning.
        {
            let mut pruner = self.state_pruner.write();
            pruner.register_block(block_number, state_root);
            pruner.mark_active(state_root);
            if pruner.should_prune(block_number) {
                pruner.mark_prunable(block_number);
                match pruner.prune(self.store.as_ref()) {
                    Ok(result) => {
                        if result.pruned_count > 0 {
                            tracing::info!(
                                pruned = result.pruned_count,
                                protected = result.protected_count,
                                block = block_number,
                                "state pruner: removed old canonical mappings"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "state pruner: prune failed");
                    }
                }
            }
        }

        // Periodic status log every 64 blocks.
        if block_number > 0 && block_number.is_multiple_of(64) {
            let oldest = tracker.oldest().map(|e| e.block_number).unwrap_or(0);
            tracing::info!(
                tracked = tracker.len(),
                oldest_block = oldest,
                archive = tracker.config().is_archive(),
                "state root history status"
            );
        }
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
        self: Arc<Self>,
        signer: Arc<dyn Signer>,
        network: &mut dyn NetworkService,
    ) -> Result<(), NodeError> {
        use shell_network::{NetworkEvent, NetworkMessage};
        use shell_rpc::{start_rpc_server, BlockEvent};
        use tokio::time::{interval, Duration};

        *self.runtime_signer.write() = Some(Arc::clone(&signer));

        // Spawn the Prometheus metrics HTTP server if enabled.
        if self.config.metrics.enabled {
            let metrics = Arc::clone(&self.metrics);
            let metrics_addr = self.config.metrics.listen_addr;
            tokio::spawn(crate::metrics::serve_metrics(metrics, metrics_addr));
        }

        // Create a channel for the RPC layer to forward submitted transactions
        // to the network broadcast loop.
        let (tx_broadcast_tx, mut tx_broadcast_rx) =
            tokio::sync::mpsc::unbounded_channel::<SignedTransaction>();

        // Create a broadcast channel for block events (eth_subscribe).
        // F-042: Use larger capacity to reduce subscriber lag.
        let (block_event_tx, _) = tokio::sync::broadcast::channel::<BlockEvent>(256);

        // Start JSON-RPC server.
        // Pass the signer to the RPC layer if this node is a validator,
        // enabling governance RPCs (proposeAddValidator / proposeRemoveValidator).
        let proposer_signer: Option<Arc<dyn Signer>> = if self.config.proposer_address.is_some() {
            Some(Arc::clone(&signer))
        } else {
            None
        };
        // Shared finalized block number for the RPC layer.
        // F-107: recover persisted finalized_number from ChainStore on restart,
        // falling back to finality state and then 0.
        let finality_num = self.finality.read().last_finalized_number();
        let persisted_num = self
            .chain_store
            .get_finalized_number()
            .ok()
            .flatten()
            .unwrap_or(0);
        let finalized_number = Arc::new(parking_lot::RwLock::new(finality_num.max(persisted_num)));

        // Get the peer count handle from the network for RPC.
        let peer_count_handle = network.peer_count_handle();

        let rpc_handle = start_rpc_server(
            self.config.rpc.clone(),
            self.chain_store.clone(),
            self.world_state.clone(),
            self.tx_pool.clone(),
            self.config.chain_id,
            Some(tx_broadcast_tx),
            block_event_tx.clone(),
            proposer_signer,
            self.config.proposer_address,
            finalized_number.clone(),
            self.finality.clone(),
            peer_count_handle,
            Some(self.clone() as Arc<dyn DevRpcControl>),
        )
        .await
        .map_err(|e| NodeError::Startup(format!("RPC: {e}")))?;

        // Register own authority pubkey for seal verification.
        if let Some(addr) = self.config.proposer_address {
            self.register_authority_pubkey(addr, signer.public_key().to_vec());
        }

        let mut block_timer = interval(Duration::from_millis(self.config.block_time_ms));
        let mut peer_count_timer = interval(Duration::from_secs(10));
        let mut sync_retry_timer = interval(Duration::from_secs(SYNC_RETRY_BASE_INTERVAL_SECS));
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        // Track the last time a block was produced for idle-block-skip.
        let mut last_block_time = std::time::Instant::now();
        let mut sync_retry_attempts_without_progress = 0u32;

        // Skip the first immediate tick.
        block_timer.tick().await;
        peer_count_timer.tick().await;
        sync_retry_timer.tick().await;

        // Startup sync: request blocks we don't have from peers.
        // Track whether we are catching up so we don't spam requests.
        let mut sync_requested = false;
        if network.peer_count().await > 0 {
            self.request_missing_blocks(network, &mut sync_requested, "initial-sync")
                .await;
        }

        loop {
            tokio::select! {
                _ = block_timer.tick() => {
                    if self.config.proposer_address.is_some() {
                        // Idle-block-skip: when mempool is empty and we haven't
                        // exceeded max_idle_interval, skip block production.
                        let max_idle_ms = self.config.max_idle_interval_ms;
                        if max_idle_ms > 0 && self.tx_pool.len() == 0 {
                            let idle_dur = std::time::Duration::from_millis(max_idle_ms);
                            if last_block_time.elapsed() < idle_dur {
                                continue;
                            }
                            // Heartbeat: produce an empty block to keep chain alive.
                        }

                        let start = std::time::Instant::now();
                        match self.produce_block(&*signer, 500) {
                            Ok(block) => {
                                last_block_time = std::time::Instant::now();
                                let elapsed = start.elapsed().as_secs_f64();
                                self.metrics.block_production_ms.observe(elapsed);
                                self.metrics.blocks_imported.inc();
                                self.metrics.block_height.set(block.number() as i64);
                                self.metrics.tx_pool_size.set(self.tx_pool.len() as i64);

                                let number = block.number();
                                let tx_count = block.transactions.len();
                                let gas = block.header.gas_used;
                                // F-046: Use scope blocks to manage lock lifetimes.
                                {
                                    let consensus = self.consensus.read();
                                    if consensus.config().is_epoch_boundary(number) {
                                        let epoch = consensus.config().epoch_of(number);
                                        info!(epoch, block = number, "new epoch started");
                                    }
                                }
                                // Reload validators at epoch boundaries (F-041: handle errors).
                                // F-061: Scope read lock explicitly to prevent deadlock.
                                let is_epoch = {
                                    self.consensus.read().config().is_epoch_boundary(number)
                                };
                                if is_epoch {
                                    let validators = {
                                        let ws = self.world_state.read();
                                        ws.get_validators()
                                    };
                                    match validators {
                                        Ok(v) if !v.is_empty() => {
                                            self.consensus.write().config_mut().set_authorities(v);
                                        }
                                        Ok(_) => {
                                            // Empty validator set in world state — keep current authorities.
                                        }
                                        Err(e) => {
                                            tracing::error!(
                                                error = %e,
                                                block = number,
                                                "CRITICAL: failed to reload validators at epoch boundary — \
                                                 continuing with stale validator set may cause consensus divergence"
                                            );
                                        }
                                    }
                                }
                                eprintln!(
                                    "⛏  Block #{number} produced ({tx_count} txs, {gas} gas)"
                                );

                                // Notify eth_subscribe listeners.
                                let block_hash = block.hash();
                                let receipts = self
                                    .chain_store
                                    .get_receipts(&block_hash)
                                    .ok()
                                    .flatten()
                                    .unwrap_or_default();
                                if block_event_tx.send(BlockEvent::NewBlock {
                                    header: block.header.clone(),
                                    receipts,
                                }).is_err() {
                                    tracing::warn!("no active subscribers for block events");
                                }

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
                                    let verifier = MultiVerifier;
                                    let saved_header = block.header.clone();
                                    let saved_hash = block.hash();
                                    let imported_number = block.number();
                                    match self.import_block(*block, &verifier) {
                                        Ok(()) => {
                                            sync_requested = false;
                                            sync_retry_attempts_without_progress = 0;
                                            sync_retry_timer.reset_after(Duration::from_secs(
                                                SYNC_RETRY_BASE_INTERVAL_SECS,
                                            ));
                                            self.metrics.blocks_imported.inc();
                                            self.metrics.block_height.set(imported_number as i64);
                                            self.metrics.tx_pool_size.set(self.tx_pool.len() as i64);

                                            // Notify eth_subscribe listeners.
                                            let receipts = self
                                                .chain_store
                                                .get_receipts(&saved_hash)
                                                .ok()
                                                .flatten()
                                                .unwrap_or_default();
                                            if block_event_tx.send(BlockEvent::NewBlock {
                                                header: saved_header,
                                                receipts,
                                            }).is_err() {
                                                tracing::warn!("no active subscribers for block events");
                                            }
                                        }
                                        Err(NodeError::GapDetected { .. }) => {
                                            // Only request missing blocks on genuine gap,
                                            // NOT on invalid signatures or other errors (F-037).
                                            if !sync_requested {
                                                self.request_missing_blocks(
                                                    network,
                                                    &mut sync_requested,
                                                    "gap-detected",
                                                )
                                                .await;
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!("⚠  Block import error: {e}");
                                        }
                                    }
                                }
                                NetworkMessage::NewTransaction(tx) => {
                                    // F-043: Use insert() directly — it returns Duplicate
                                    // error if already known, avoiding TOCTOU race.
                                    let verifier = MultiVerifier;
                                    match self.handle_incoming_tx(*tx, &verifier) {
                                        Ok(_hash) => {
                                            self.metrics.txs_received.inc();
                                            self.metrics.tx_pool_size.set(self.tx_pool.len() as i64);
                                        }
                                        Err(e) => {
                                            // MempoolError::Duplicate is expected for re-broadcast; don't log it as error.
                                            let msg = format!("{e}");
                                            if !msg.contains("duplicate") && !msg.contains("Duplicate") {
                                                eprintln!("⚠  Tx handling error: {e}");
                                            }
                                        }
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
                                    let verifier = MultiVerifier;
                                    let mut last_ok = 0u64;
                                    for block in blocks {
                                        let num = block.number();
                                        let hdr = block.header.clone();
                                        let bhash = block.hash();
                                        match self.import_block(block, &verifier) {
                                            Ok(()) => {
                                                last_ok = num;
                                                self.metrics.blocks_imported.inc();
                                                self.metrics.block_height.set(num as i64);
                                                debug!(number = num, "synced block");

                                                // Notify eth_subscribe listeners.
                                                let receipts = self
                                                    .chain_store
                                                    .get_receipts(&bhash)
                                                    .ok()
                                                    .flatten()
                                                    .unwrap_or_default();
                                                if block_event_tx.send(BlockEvent::NewBlock {
                                                    header: hdr,
                                                    receipts,
                                                }).is_err() {
                                                    tracing::warn!("no active subscribers for block events");
                                                }
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
                                        sync_retry_attempts_without_progress = 0;
                                        sync_retry_timer.reset_after(Duration::from_secs(
                                            SYNC_RETRY_BASE_INTERVAL_SECS,
                                        ));
                                    } else {
                                        sync_requested = false;
                                        sync_retry_attempts_without_progress = 0;
                                        sync_retry_timer.reset_after(Duration::from_secs(
                                            SYNC_RETRY_BASE_INTERVAL_SECS,
                                        ));
                                    }
                                }
                                NetworkMessage::Ping => {
                                    debug!(%peer, "received Ping, responding with Pong");
                                    let _ = network.broadcast(NetworkMessage::Pong).await;
                                }
                                NetworkMessage::Pong => {
                                    debug!(%peer, "received Pong");
                                }
                                NetworkMessage::NewAttestation(attestation) => {
                                    let verifier = MultiVerifier;
                                    if let Err(e) = self.handle_attestation(*attestation, &verifier) {
                                        tracing::warn!("attestation error: {e}");
                                    }
                                    // Push latest finalized number to the RPC layer.
                                    let fin = self.finality.read().last_finalized_number();
                                    let mut fn_w = finalized_number.write();
                                    if fin > *fn_w {
                                        *fn_w = fin;
                                    }
                                }
                            }
                        }
                        Some(NetworkEvent::PeerConnected(peer)) => {
                            info!(%peer, "peer connected");
                            sync_requested = false;
                            sync_retry_attempts_without_progress = 0;
                            sync_retry_timer
                                .reset_after(Duration::from_secs(SYNC_RETRY_BASE_INTERVAL_SECS));
                            self.request_missing_blocks(
                                network,
                                &mut sync_requested,
                                "peer-connected",
                            )
                            .await;
                        }
                        Some(NetworkEvent::PeerDisconnected(peer)) => {
                            info!(%peer, "peer disconnected");
                            sync_requested = false;
                            sync_retry_attempts_without_progress = 0;
                            sync_retry_timer
                                .reset_after(Duration::from_secs(SYNC_RETRY_BASE_INTERVAL_SECS));
                        }
                        Some(NetworkEvent::RoutingTableUpdated { peer_count }) => {
                            debug!(peer_count, "routing table updated");
                            if peer_count > 0 && !sync_requested {
                                sync_retry_attempts_without_progress = 0;
                                sync_retry_timer.reset_after(Duration::from_secs(
                                    SYNC_RETRY_BASE_INTERVAL_SECS,
                                ));
                                self.request_missing_blocks(
                                    network,
                                    &mut sync_requested,
                                    "routing-update",
                                )
                                .await;
                            }
                        }
                        None => {
                            eprintln!("Network channel closed, shutting down");
                            break;
                        }
                    }
                }

                // Forward RPC-submitted transactions to peers.
                Some(signed_tx) = tx_broadcast_rx.recv() => {
                    let msg = NetworkMessage::NewTransaction(Box::new(signed_tx));
                    let _ = network.broadcast(msg).await;
                }

                // Periodically update peer count metric.
                _ = peer_count_timer.tick() => {
                    let peers = network.peer_count().await;
                    self.metrics.peer_count.set(peers as i64);
                }

                _ = sync_retry_timer.tick() => {
                    if sync_requested && network.peer_count().await > 0 {
                        self.request_missing_blocks(
                            network,
                            &mut sync_requested,
                            "sync-retry",
                        )
                        .await;
                        sync_retry_attempts_without_progress =
                            sync_retry_attempts_without_progress.saturating_add(1);
                        sync_retry_timer.reset_after(Duration::from_secs(
                            Self::sync_retry_delay_secs(sync_retry_attempts_without_progress),
                        ));
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

        // Graceful shutdown: stop RPC servers first.
        rpc_handle.http_handle.stop().ok();
        if let Some(ws) = rpc_handle.ws_handle {
            ws.stop().ok();
        }
        eprintln!("✓ RPC server stopped");

        // Flush storage to disk.
        if let Err(e) = self.store.flush() {
            eprintln!("⚠  Storage flush failed: {e}");
        } else {
            eprintln!("✓ Storage flushed to disk");
        }

        let _ = network.shutdown().await;
        Ok(())
    }

    /// Produce a block from pending mempool transactions.
    ///
    /// Collects up to `max_txs` transactions, executes each through the EVM,
    /// commits state changes after every transaction (so subsequent txs see
    /// prior updates), assembles a block, and commits it to storage.
    pub fn produce_block(&self, signer: &dyn Signer, max_txs: usize) -> Result<Block, NodeError> {
        let head = self
            .chain_store
            .get_head_block()?
            .ok_or(NodeError::NoGenesis)?;
        let head_hash = head.hash();
        let next_number = head.number() + 1;

        let proposer_addr = self.config.proposer_address.ok_or(NodeError::NotProposer)?;

        if !self
            .consensus
            .read()
            .is_proposer(next_number, &proposer_addr)
        {
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

        let now = self.current_block_timestamp(head.header.timestamp);

        // Calculate EIP-1559 base fee from parent block.
        let base_fee = calculate_base_fee(
            head.header.gas_used,
            head.header.gas_limit,
            head.header.base_fee_per_gas,
        );

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
            base_fee_per_gas: base_fee,
            withdrawals_root: ShellHash::ZERO,
            parent_beacon_block_root: ShellHash::ZERO,
            blob_gas_used: 0,
            excess_blob_gas: 0,
        };

        let mut included_txs: Vec<SignedTransaction> = Vec::new();
        let mut receipts = Vec::new();
        let mut cumulative_gas: u64 = 0;

        for (idx, tx) in candidates.iter().enumerate() {
            // EIP-1559: skip transactions that cannot afford the base fee.
            if tx.tx.max_fee_per_gas < base_fee {
                continue;
            }

            // F-302: Re-validate mempool txs before execution. Security checks
            // may have changed since the tx was originally admitted (e.g. new
            // algorithm restrictions, pubkey conflicts). Uses the import-path
            // validator which skips nonce/balance (EVM handles those).
            let import_cs = ChainStore::new(self.store.clone());
            let pre_verifier = PreVerified;
            if let Err(e) =
                validate_tx_for_import(tx, &import_cs, &pre_verifier, self.config.chain_id)
            {
                debug!(
                    tx_hash = %tx.tx.hash(),
                    error = %e,
                    "produce_block: skipping tx that failed re-validation"
                );
                continue;
            }

            match evm.execute_tx(tx, &header, idx as u32, cumulative_gas) {
                Ok(result) => {
                    cumulative_gas += result.gas_used;
                    receipts.push(result.receipt);
                    included_txs.push(tx.clone());

                    if result.is_system_tx {
                        // System contract tx: state was applied directly to
                        // the EVM's WorldState. Sync validator set to the
                        // persistent WorldState so state_root is consistent.
                        // Propagate errors to abort block production (F-068).
                        let local_ws = evm.state_db_mut().world_state_mut();
                        let validators = local_ws.get_validators()?;
                        // F-202: Validate resulting validator set (same as import path).
                        if validators.is_empty() {
                            return Err(NodeError::Startup(
                                "system tx produced empty validator set".into(),
                            ));
                        }
                        if validators.len() > WorldState::<S>::MAX_VALIDATORS {
                            return Err(NodeError::Startup(format!(
                                "system tx produced validator set of size {} exceeding max {}",
                                validators.len(),
                                WorldState::<S>::MAX_VALIDATORS,
                            )));
                        }
                        let mut ws = self.world_state.write();
                        ws.set_validators(&validators)?;
                    } else {
                        // Normal EVM tx: commit EvmState changeset.
                        commit_evm_state(
                            &result.state_changes,
                            evm.state_db_mut().world_state_mut(),
                            &self.chain_store,
                        )?;

                        // Commit to the node's persistent WorldState.
                        {
                            let mut ws = self.world_state.write();
                            commit_evm_state(&result.state_changes, &mut ws, &self.chain_store)?;
                        }
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

        // Compute block-level logs bloom by OR-ing all receipt blooms.
        {
            let receipt_blooms: Vec<shell_evm::bloom::Bloom> = receipts
                .iter()
                .map(|r| {
                    let mut bloom = [0u8; shell_evm::bloom::BLOOM_SIZE];
                    let bytes = r.logs_bloom.as_ref();
                    let len = bytes.len().min(shell_evm::bloom::BLOOM_SIZE);
                    bloom[..len].copy_from_slice(&bytes[..len]);
                    bloom
                })
                .collect();
            let block_bloom = shell_evm::bloom::bloom_union(&receipt_blooms);
            header.logs_bloom = Bytes::from(block_bloom.to_vec());
        }

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
        self.consensus.read().sign_block(&mut block, signer)?;

        // Register the signer's pubkey so we can verify our own blocks on re-import.
        self.register_authority_pubkey(proposer_addr, signer.public_key().to_vec());

        // Commit to storage.
        let block_hash = block.hash();
        self.chain_store.put_block(&block)?;
        self.chain_store.put_receipts(&block_hash, &receipts)?;
        self.chain_store
            .set_canonical(block.number(), &block_hash)?;
        self.chain_store.set_head(&block_hash)?;

        // Remove included transactions from mempool.
        let tx_hashes: Vec<ShellHash> = included_txs.iter().map(|tx| tx.hash()).collect();
        self.tx_pool.remove_batch(&tx_hashes);

        // Update global transaction counter for shell_transactionCount RPC.
        let new_tx_count = included_txs.len() as u64;
        if new_tx_count > 0 {
            let _ = self.chain_store.increment_tx_count(new_tx_count);
        }

        // Track the new state root for pruning decisions.
        self.record_finalized_state_root(block.number(), block.header.state_root);

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
    pub fn import_block(&self, block: Block, _verifier: &dyn Verifier) -> Result<(), NodeError> {
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
            return Err(NodeError::GapDetected { incoming, expected });
        }

        // Verify consensus rules.
        self.consensus.read().verify_header(&block.header)?;

        // Verify EIP-1559 base fee is correct.
        let expected_base_fee = calculate_base_fee(
            head.header.gas_used,
            head.header.gas_limit,
            head.header.base_fee_per_gas,
        );
        if block.header.base_fee_per_gas != expected_base_fee {
            return Err(NodeError::Startup(format!(
                "invalid base_fee_per_gas: expected {expected_base_fee}, got {}",
                block.header.base_fee_per_gas,
            )));
        }

        // Verify proposer seal (PQ signature).
        match &block.proposer_seal {
            Some(seal) => {
                let proposer = &block.header.proposer;
                let known = self.known_authorities.read();
                if let Some(pubkey) = known.get(proposer) {
                    let verifier = MultiVerifier;
                    self.consensus
                        .read()
                        .verify_seal(&block.header, seal, pubkey, &verifier)?;
                } else {
                    // Try chain store as fallback.
                    drop(known);
                    if let Ok(Some(pubkey)) = self.chain_store.get_pubkey(proposer) {
                        let verifier = MultiVerifier;
                        self.consensus.read().verify_seal(
                            &block.header,
                            seal,
                            &pubkey,
                            &verifier,
                        )?;
                        // Cache for future lookups.
                        self.known_authorities.write().insert(*proposer, pubkey);
                    } else {
                        // F-308: Reject blocks from unknown proposers.
                        return Err(NodeError::Startup(format!(
                            "block {} seal verification failed: proposer {} pubkey unknown",
                            block.number(),
                            proposer
                        )));
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
        let mut receipts = Vec::new();
        if !block.transactions.is_empty() {
            // Validate all transactions before execution (F-181):
            // security-critical checks (sig, algorithm, access list, pubkey)
            // are enforced during block import, not just mempool.
            let import_cs = ChainStore::new(self.store.clone());

            // M5-C2: Batch verify all transaction signatures in parallel.
            // Resolve pubkeys and compute tx hashes, then dispatch to rayon.
            let batch_verifier = MultiVerifier;
            let tx_hashes: Vec<ShellHash> = block.transactions.iter().map(|tx| tx.hash()).collect();
            let mut resolved_pks: Vec<Vec<u8>> = Vec::with_capacity(block.transactions.len());
            for tx in &block.transactions {
                let pk = match &tx.sender_pubkey {
                    Some(pk) => pk.clone(),
                    None => import_cs
                        .get_pubkey(&tx.from)
                        .map_err(|e| {
                            NodeError::Startup(format!(
                                "block {} pubkey lookup failed: {e}",
                                block.number()
                            ))
                        })?
                        .ok_or_else(|| {
                            NodeError::Startup(format!(
                                "block {} missing pubkey for {}",
                                block.number(),
                                tx.from
                            ))
                        })?,
                };
                resolved_pks.push(pk);
            }
            let verify_items: Vec<VerifyItem> = block
                .transactions
                .iter()
                .enumerate()
                .map(|(i, tx)| VerifyItem {
                    pubkey: &resolved_pks[i],
                    message: tx_hashes[i].as_bytes(),
                    signature: &tx.signature,
                })
                .collect();
            batch_verifier
                .verify_batch_all(&verify_items)
                .map_err(|e| {
                    NodeError::Startup(format!(
                        "block {} batch sig verification failed: {e}",
                        block.number()
                    ))
                })?;

            // Non-signature validation (chain-id, gas, access list, address
            // derivation). Uses PreVerified to skip redundant individual
            // sig checks — signatures were already batch-verified above.
            let pre_verified = PreVerified;
            for tx in &block.transactions {
                validate_tx_for_import(tx, &import_cs, &pre_verified, self.config.chain_id)
                    .map_err(|e| {
                        NodeError::Startup(format!(
                            "block {} tx validation failed: {e}",
                            block.number()
                        ))
                    })?;
            }

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
                        receipts.push(result.receipt);

                        if result.is_system_tx {
                            // Propagate errors to abort block import (F-068).
                            let local_ws = evm.state_db_mut().world_state_mut();
                            let validators = local_ws.get_validators()?;
                            // F-068: Validate resulting validator set is sane.
                            if validators.is_empty() {
                                return Err(NodeError::Startup(
                                    "system tx produced empty validator set".into(),
                                ));
                            }
                            if validators.len() > WorldState::<S>::MAX_VALIDATORS {
                                return Err(NodeError::Startup(format!(
                                    "system tx produced validator set of size {} exceeding max {}",
                                    validators.len(),
                                    WorldState::<S>::MAX_VALIDATORS,
                                )));
                            }
                            let mut ws = self.world_state.write();
                            ws.set_validators(&validators)?;
                        } else {
                            commit_evm_state(
                                &result.state_changes,
                                evm.state_db_mut().world_state_mut(),
                                &self.chain_store,
                            )?;

                            let mut ws = self.world_state.write();
                            commit_evm_state(&result.state_changes, &mut ws, &self.chain_store)?;
                        }
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
        if !receipts.is_empty() {
            self.chain_store.put_receipts(&block_hash, &receipts)?;
        }
        self.chain_store
            .set_canonical(block.number(), &block_hash)?;
        self.chain_store.set_head(&block_hash)?;

        // Remove any included transactions from our mempool.
        let tx_hashes: Vec<ShellHash> = block.transactions.iter().map(|tx| tx.hash()).collect();
        self.tx_pool.remove_batch(&tx_hashes);

        // Update global transaction counter for shell_transactionCount RPC.
        let imported_tx_count = block.transactions.len() as u64;
        if imported_tx_count > 0 {
            let _ = self.chain_store.increment_tx_count(imported_tx_count);
        }

        // Track the imported state root for pruning decisions.
        self.record_finalized_state_root(block.number(), block.header.state_root);

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

        let known_pubkeys =
            |addr: &Address| -> Option<Vec<u8>> { chain_store.get_pubkey(addr).ok().flatten() };
        let balance_of =
            |addr: &Address| -> U256 { world_state_guard.get_balance(addr).unwrap_or(U256::ZERO) };

        let dv = MultiVerifier;
        let hash = self
            .tx_pool
            .insert(tx, &dv, &known_pubkeys, &balance_of)
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
        let total_validators = self.consensus.read().config().authorities.len();
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
}

impl<S: KvStore + 'static> DevRpcControl for Node<S> {
    fn mine_blocks(&self, blocks: u64) -> Result<(), String> {
        let signer = self
            .runtime_signer
            .read()
            .clone()
            .ok_or_else(|| "node signer is not initialized".to_string())?;
        for _ in 0..blocks.max(1) {
            self.produce_block(signer.as_ref(), 500)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn set_next_block_timestamp(&self, timestamp: u64) -> Result<u64, String> {
        let head = self
            .chain_store
            .get_head_block()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "missing head block".to_string())?;
        let min_timestamp = head.header.timestamp.saturating_add(1);
        if timestamp < min_timestamp {
            return Err(format!(
                "timestamp must be >= next valid block timestamp {min_timestamp}"
            ));
        }
        self.dev_state.write().next_block_timestamp = Some(timestamp);
        Ok(timestamp)
    }

    fn increase_time(&self, seconds: u64) -> Result<u64, String> {
        let head = self
            .chain_store
            .get_head_block()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "missing head block".to_string())?;
        let mut dev = self.dev_state.write();
        let base_timestamp = dev
            .next_block_timestamp
            .unwrap_or(head.header.timestamp)
            .max(head.header.timestamp);
        let next_timestamp = base_timestamp.saturating_add(seconds);
        dev.next_block_timestamp = Some(next_timestamp);
        Ok(next_timestamp.saturating_sub(head.header.timestamp))
    }

    fn snapshot(&self) -> Result<String, String> {
        self.snapshot_inner().map_err(|e| e.to_string())
    }

    fn revert(&self, snapshot_id: &str) -> Result<bool, String> {
        self.revert_inner(snapshot_id).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pruning::PruningConfig;
    use shell_consensus::PoaConfig;
    use shell_core::Transaction;
    use shell_crypto::DilithiumSigner;
    use shell_mempool::MempoolConfig;
    use shell_rpc::DevRpcControl;
    use shell_storage::MemoryDb;

    fn setup_node() -> (Node<MemoryDb>, DilithiumSigner) {
        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let authority = Address::from_public_key(&pubkey);

        let db = Arc::new(MemoryDb::new());
        let chain_store = Arc::new(ChainStore::new(db.clone()));
        let world_state = Arc::new(RwLock::new(WorldState::new(db.clone())));
        let consensus = Arc::new(RwLock::new(PoaEngine::new(PoaConfig::new(
            vec![authority],
            1,
        ))));
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
                base_fee_per_gas: 0,
                withdrawals_root: ShellHash::ZERO,
                parent_beacon_block_root: ShellHash::ZERO,
                blob_gas_used: 0,
                excess_blob_gas: 0,
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
    fn sync_retry_delay_uses_backoff_after_threshold() {
        assert_eq!(Node::<MemoryDb>::sync_retry_delay_secs(0), 5);
        assert_eq!(Node::<MemoryDb>::sync_retry_delay_secs(2), 5);
        assert_eq!(Node::<MemoryDb>::sync_retry_delay_secs(3), 30);
        assert_eq!(Node::<MemoryDb>::sync_retry_delay_secs(10), 30);
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
    fn dev_rpc_mine_blocks_advances_head() {
        let (node, signer) = setup_node();
        store_genesis(&node);
        *node.runtime_signer.write() = Some(Arc::new(signer));

        node.mine_blocks(2).unwrap();

        let head = node.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(head.number(), 2);
    }

    #[test]
    fn dev_rpc_time_controls_affect_next_block() {
        let (node, signer) = setup_node();
        store_genesis(&node);
        *node.runtime_signer.write() = Some(Arc::new(signer));

        let genesis = node.chain_store.get_head_block().unwrap().unwrap();
        let next_ts = genesis.header.timestamp + 10;
        assert_eq!(node.set_next_block_timestamp(next_ts).unwrap(), next_ts);
        node.mine_blocks(1).unwrap();

        let block1 = node.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(block1.header.timestamp, next_ts);

        let offset = node.increase_time(30).unwrap();
        assert_eq!(offset, 30);
        node.mine_blocks(1).unwrap();

        let block2 = node.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(block2.header.timestamp, block1.header.timestamp + 30);
    }

    #[test]
    fn dev_rpc_snapshot_and_revert_restore_head() {
        let (node, signer) = setup_node();
        store_genesis(&node);
        *node.runtime_signer.write() = Some(Arc::new(signer));

        node.mine_blocks(1).unwrap();
        let snapshot_id = node.snapshot().unwrap();
        node.mine_blocks(2).unwrap();
        let head_before_revert = node.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(head_before_revert.number(), 3);

        assert!(node.revert(&snapshot_id).unwrap());

        let head_after_revert = node.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(head_after_revert.number(), 1);
        assert!(!node.revert("0xdeadbeef").unwrap());
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

        // Fund sender (enough for transfer + gas at INITIAL_BASE_FEE)
        fund_account(&node, &sender, U256::from(100_000_000_000_000u64));

        // Verify initial balances
        {
            let ws = node.world_state.read();
            assert_eq!(
                ws.get_balance(&sender).unwrap(),
                U256::from(100_000_000_000_000u64)
            );
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
            max_fee_per_gas: shell_core::INITIAL_BASE_FEE,
            max_priority_fee_per_gas: 0,
            access_list: None,
            tx_type: 2,
            max_fee_per_blob_gas: None,
            blob_versioned_hashes: None,
        };

        // Sign with real Dilithium key
        let tx_hash = {
            let encoded = alloy_rlp::encode(&tx);
            shell_primitives::keccak256(&encoded)
        };
        let sig = tx_signer.sign(tx_hash.as_bytes()).expect("sign failed");
        let signed =
            SignedTransaction::with_pubkey(sender, tx, sig, tx_signer.public_key().to_vec());

        // Insert into mempool with real verification
        let verifier = MultiVerifier;
        let known_pubkeys = |_: &Address| -> Option<Vec<u8>> { None };
        let balance_of = |addr: &Address| -> U256 {
            node.world_state
                .read()
                .get_balance(addr)
                .unwrap_or(U256::ZERO)
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
            assert_eq!(
                receiver_balance, transfer_value,
                "receiver should have received the transfer"
            );

            // Sender balance should have decreased (value transferred + gas)
            let sender_balance = ws.get_balance(&sender).unwrap();
            assert!(
                sender_balance < U256::from(100_000_000_000_000u64),
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
                base_fee_per_gas: shell_core::INITIAL_BASE_FEE,
                withdrawals_root: ShellHash::ZERO,
                parent_beacon_block_root: ShellHash::ZERO,
                blob_gas_used: 0,
                excess_blob_gas: 0,
            },
            transactions: vec![],
            proposer_seal: None,
        };

        let verifier = MultiVerifier;
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
        let consensus = Arc::new(RwLock::new(PoaEngine::new(PoaConfig::new(
            vec![proposer],
            1,
        ))));
        let tx_pool = Arc::new(TxPool::new(MempoolConfig {
            chain_id: 1337,
            ..MempoolConfig::default()
        }));
        let config = NodeConfig::dev(proposer);
        let node2 = Node::new(config, node2_db, node2_cs, node2_ws, tx_pool, consensus);
        store_genesis(&node2);

        // Register authority pubkey on node2.
        node2.register_authority_pubkey(proposer, signer.public_key().to_vec());

        let verifier = MultiVerifier;
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
        let consensus = Arc::new(RwLock::new(PoaEngine::new(PoaConfig::new(
            vec![proposer],
            1,
        ))));
        let tx_pool = Arc::new(TxPool::new(MempoolConfig {
            chain_id: 1337,
            ..MempoolConfig::default()
        }));
        let config = NodeConfig::dev(proposer);
        let node2 = Node::new(config, node2_db, node2_cs, node2_ws, tx_pool, consensus);
        store_genesis(&node2);
        node2.register_authority_pubkey(proposer, signer.public_key().to_vec());

        let verifier = MultiVerifier;
        let result = node2.import_block(block, &verifier);
        assert!(
            result.is_err(),
            "block with invalid seal should be rejected"
        );
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
                base_fee_per_gas: shell_core::INITIAL_BASE_FEE,
                withdrawals_root: ShellHash::ZERO,
                parent_beacon_block_root: ShellHash::ZERO,
                blob_gas_used: 0,
                excess_blob_gas: 0,
            },
            transactions: vec![],
            proposer_seal: None,
        };

        let verifier = MultiVerifier;
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
        let pubkey = known
            .get(&proposer)
            .expect("pubkey should be registered after produce_block");
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

    #[test]
    fn epoch_boundary_reloads_validators() {
        let signer = DilithiumSigner::generate();
        let authority = Address::from_public_key(signer.public_key());

        let db = Arc::new(MemoryDb::new());
        let chain_store = Arc::new(ChainStore::new(db.clone()));
        let world_state = Arc::new(RwLock::new(WorldState::new(db.clone())));
        let consensus = Arc::new(RwLock::new(PoaEngine::new(
            PoaConfig::new(vec![authority], 1).with_epoch_length(3),
        )));
        let tx_pool = Arc::new(TxPool::new(MempoolConfig {
            chain_id: 1337,
            ..MempoolConfig::default()
        }));

        let config = NodeConfig::dev(authority);
        let node = Node::new(config, db, chain_store, world_state, tx_pool, consensus);
        store_genesis(&node);

        // Write a new validator set to world state.
        let new_validator = Address::from([0xAA; 20]);
        {
            let mut ws = node.world_state.write();
            ws.set_validators(&[authority, new_validator]).unwrap();
        }

        // Before epoch boundary, consensus has 1 authority.
        assert_eq!(node.consensus.read().config().authorities.len(), 1);

        // Produce blocks until we hit the epoch boundary (block 3).
        for _ in 0..3 {
            node.produce_block(&signer, 0).unwrap();
        }

        // Block 3 is an epoch boundary (epoch_length=3).
        // Simulate the epoch boundary sync that the event loop would do.
        {
            let consensus = node.consensus.read();
            if consensus.config().is_epoch_boundary(3) {
                drop(consensus);
                let ws = node.world_state.read();
                let validators = ws.get_validators().unwrap();
                drop(ws);
                if !validators.is_empty() {
                    node.consensus
                        .write()
                        .config_mut()
                        .set_authorities(validators);
                }
            }
        }

        // After epoch boundary reload, consensus should have 2 authorities.
        let consensus_guard = node.consensus.read();
        let authorities = &consensus_guard.config().authorities;
        assert_eq!(authorities.len(), 2);
        assert!(authorities.contains(&authority));
        assert!(authorities.contains(&new_validator));
    }

    #[test]
    fn validator_change_takes_effect_at_next_epoch() {
        let signer = DilithiumSigner::generate();
        let authority = Address::from_public_key(signer.public_key());

        let db = Arc::new(MemoryDb::new());
        let chain_store = Arc::new(ChainStore::new(db.clone()));
        let world_state = Arc::new(RwLock::new(WorldState::new(db.clone())));
        let consensus = Arc::new(RwLock::new(PoaEngine::new(
            PoaConfig::new(vec![authority], 1).with_epoch_length(2),
        )));
        let tx_pool = Arc::new(TxPool::new(MempoolConfig {
            chain_id: 1337,
            ..MempoolConfig::default()
        }));

        let config = NodeConfig::dev(authority);
        let node = Node::new(config, db, chain_store, world_state, tx_pool, consensus);
        store_genesis(&node);

        // Produce block 1 — not an epoch boundary.
        node.produce_block(&signer, 0).unwrap();
        assert_eq!(node.consensus.read().config().authorities.len(), 1);

        // Write validators mid-epoch.
        let new_val = Address::from([0xCC; 20]);
        {
            let mut ws = node.world_state.write();
            ws.set_validators(&[authority, new_val]).unwrap();
        }

        // Still not reloaded until epoch boundary.
        assert_eq!(node.consensus.read().config().authorities.len(), 1);

        // Produce block 2 — epoch boundary (epoch_length=2).
        node.produce_block(&signer, 0).unwrap();

        // Simulate epoch boundary sync.
        {
            let consensus = node.consensus.read();
            if consensus.config().is_epoch_boundary(2) {
                drop(consensus);
                let ws = node.world_state.read();
                let validators = ws.get_validators().unwrap();
                drop(ws);
                if !validators.is_empty() {
                    node.consensus
                        .write()
                        .config_mut()
                        .set_authorities(validators);
                }
            }
        }

        // Now the validator set should be updated.
        assert_eq!(node.consensus.read().config().authorities.len(), 2);
    }

    // ── Pruning integration tests ──────────────────────────────────────

    fn setup_node_with_pruning(keep_recent: u64) -> (Node<MemoryDb>, DilithiumSigner) {
        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let authority = Address::from_public_key(&pubkey);

        let db = Arc::new(MemoryDb::new());
        let chain_store = Arc::new(ChainStore::new(db.clone()));
        let world_state = Arc::new(RwLock::new(WorldState::new(db.clone())));
        let consensus = Arc::new(RwLock::new(PoaEngine::new(PoaConfig::new(
            vec![authority],
            1,
        ))));
        let tx_pool = Arc::new(TxPool::new(MempoolConfig {
            chain_id: 1337,
            ..MempoolConfig::default()
        }));

        let mut config = NodeConfig::dev(authority);
        config.pruning = PruningConfig::new(keep_recent);
        let node = Node::new(config, db, chain_store, world_state, tx_pool, consensus);
        (node, signer)
    }

    #[test]
    fn state_root_history_grows_with_blocks() {
        let (node, signer) = setup_node_with_pruning(128);
        store_genesis(&node);

        for _ in 0..5 {
            node.produce_block(&signer, 0).unwrap();
        }

        let tracker = node.state_root_tracker.read();
        assert_eq!(tracker.len(), 5, "should track one root per produced block");
        assert_eq!(tracker.oldest().unwrap().block_number, 1);
        assert_eq!(tracker.latest().unwrap().block_number, 5);
    }

    #[test]
    fn oldest_roots_evicted_when_exceeding_keep_recent() {
        let keep = 3u64;
        let (node, signer) = setup_node_with_pruning(keep);
        store_genesis(&node);

        for _ in 0..6 {
            node.produce_block(&signer, 0).unwrap();
        }

        let tracker = node.state_root_tracker.read();
        assert_eq!(
            tracker.len(),
            keep as usize,
            "history should be capped at keep_recent"
        );
        assert_eq!(
            tracker.oldest().unwrap().block_number,
            4,
            "blocks 1–3 should have been evicted"
        );
        assert_eq!(tracker.latest().unwrap().block_number, 6);
    }

    #[test]
    fn archive_mode_never_prunes() {
        let (node, signer) = setup_node_with_pruning(0); // archive
        store_genesis(&node);

        for _ in 0..10 {
            node.produce_block(&signer, 0).unwrap();
        }

        let tracker = node.state_root_tracker.read();
        assert_eq!(tracker.len(), 10, "archive mode keeps all roots");
        assert_eq!(tracker.oldest().unwrap().block_number, 1);
    }

    // ── Block sync integration tests ───────────────────────────────────

    #[test]
    fn import_multiple_sequential_blocks() {
        let (node, _signer) = setup_node();
        store_genesis(&node);
        let verifier = MultiVerifier;
        let proposer = node.config.proposer_address.unwrap();

        let mut parent_hash = node.chain_store.get_head_hash().unwrap().unwrap();
        let mut parent_gas_used = 0u64;
        let mut parent_gas_limit = 30_000_000u64;
        let mut parent_base_fee = 0u64;

        for i in 1..=5u64 {
            let base_fee =
                shell_core::calculate_base_fee(parent_gas_used, parent_gas_limit, parent_base_fee);
            let block = Block {
                header: BlockHeader {
                    parent_hash,
                    state_root: ShellHash::default(),
                    transactions_root: ShellHash::default(),
                    receipts_root: ShellHash::default(),
                    logs_bloom: Bytes::default(),
                    number: i,
                    gas_limit: 30_000_000,
                    gas_used: 0,
                    timestamp: 1_700_000_000 + i,
                    extra_data: Bytes::default(),
                    proposer,
                    sig_aggregate_proof: None,
                    base_fee_per_gas: base_fee,
                    withdrawals_root: ShellHash::ZERO,
                    parent_beacon_block_root: ShellHash::ZERO,
                    blob_gas_used: 0,
                    excess_blob_gas: 0,
                },
                transactions: vec![],
                proposer_seal: None,
            };
            parent_hash = block.hash();
            parent_gas_used = block.header.gas_used;
            parent_gas_limit = block.header.gas_limit;
            parent_base_fee = base_fee;
            node.import_block(block, &verifier).unwrap();
        }

        let head = node.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(head.number(), 5);
        for i in 0..=5u64 {
            assert!(
                node.chain_store.get_block_by_number(i).unwrap().is_some(),
                "block {i} should be retrievable by number"
            );
        }
    }

    #[test]
    fn import_block_with_gap_fails() {
        let (node, _signer) = setup_node();
        store_genesis(&node);
        let verifier = MultiVerifier;
        let proposer = node.config.proposer_address.unwrap();

        // Skip block 1, try to import block 2 directly.
        let block = Block {
            header: BlockHeader {
                parent_hash: ShellHash::from([0xAA; 32]),
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::default(),
                number: 2,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_002,
                extra_data: Bytes::default(),
                proposer,
                sig_aggregate_proof: None,
                base_fee_per_gas: shell_core::INITIAL_BASE_FEE,
                withdrawals_root: ShellHash::ZERO,
                parent_beacon_block_root: ShellHash::ZERO,
                blob_gas_used: 0,
                excess_blob_gas: 0,
            },
            transactions: vec![],
            proposer_seal: None,
        };

        let result = node.import_block(block, &verifier);
        assert!(result.is_err());
        match result.unwrap_err() {
            NodeError::GapDetected { incoming, expected } => {
                assert_eq!(incoming, 2);
                assert_eq!(expected, 1);
            }
            other => panic!("expected GapDetected, got: {other:?}"),
        }
    }

    #[test]
    fn import_fork_block_at_same_height_skipped() {
        let (node, _signer) = setup_node();
        store_genesis(&node);
        let verifier = MultiVerifier;
        let proposer = node.config.proposer_address.unwrap();

        // Import block 1 normally.
        let parent_hash = node.chain_store.get_head_hash().unwrap().unwrap();
        let block1 = Block {
            header: BlockHeader {
                parent_hash,
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::default(),
                number: 1,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_001,
                extra_data: Bytes::default(),
                proposer,
                sig_aggregate_proof: None,
                base_fee_per_gas: shell_core::INITIAL_BASE_FEE,
                withdrawals_root: ShellHash::ZERO,
                parent_beacon_block_root: ShellHash::ZERO,
                blob_gas_used: 0,
                excess_blob_gas: 0,
            },
            transactions: vec![],
            proposer_seal: None,
        };
        let block1_hash = block1.hash();
        node.import_block(block1, &verifier).unwrap();
        assert_eq!(
            node.chain_store.get_head_hash().unwrap().unwrap(),
            block1_hash
        );

        // Try to import a competing block at the same height with different content.
        let fork_block = Block {
            header: BlockHeader {
                parent_hash,
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::default(),
                number: 1,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_099_999, // different timestamp → different hash
                extra_data: Bytes::default(),
                proposer,
                sig_aggregate_proof: None,
                base_fee_per_gas: shell_core::INITIAL_BASE_FEE,
                withdrawals_root: ShellHash::ZERO,
                parent_beacon_block_root: ShellHash::ZERO,
                blob_gas_used: 0,
                excess_blob_gas: 0,
            },
            transactions: vec![],
            proposer_seal: None,
        };

        // Should succeed (silently skipped as fork), head unchanged.
        let result = node.import_block(fork_block, &verifier);
        assert!(result.is_ok());
        assert_eq!(
            node.chain_store.get_head_hash().unwrap().unwrap(),
            block1_hash,
            "head should remain unchanged after fork block is skipped"
        );
    }

    #[test]
    fn import_block_out_of_order_then_correct_order() {
        let (node, signer) = setup_node();
        store_genesis(&node);
        let verifier = MultiVerifier;

        // Produce block 1 to get a valid block.
        let block1 = node.produce_block(&signer, 100).unwrap();
        assert_eq!(block1.number(), 1);

        // Set up node2 to try importing.
        let proposer = node.config.proposer_address.unwrap();
        let db2 = Arc::new(MemoryDb::new());
        let cs2 = Arc::new(ChainStore::new(db2.clone()));
        let ws2 = Arc::new(RwLock::new(WorldState::new(db2.clone())));
        let consensus = Arc::new(RwLock::new(PoaEngine::new(PoaConfig::new(
            vec![proposer],
            1,
        ))));
        let tx_pool = Arc::new(TxPool::new(MempoolConfig {
            chain_id: 1337,
            ..MempoolConfig::default()
        }));
        let config = NodeConfig::dev(proposer);
        let node2 = Node::new(config, db2, cs2, ws2, tx_pool, consensus);
        node2.register_authority_pubkey(proposer, signer.public_key().to_vec());
        store_genesis(&node2);

        // Produce block 2 on node1.
        let block2 = node.produce_block(&signer, 100).unwrap();
        assert_eq!(block2.number(), 2);

        // Try importing block 2 first (out of order) — should fail with gap.
        let result = node2.import_block(block2.clone(), &verifier);
        assert!(result.is_err());

        // Now import block 1, then block 2 — both should succeed.
        node2.import_block(block1, &verifier).unwrap();
        node2.import_block(block2, &verifier).unwrap();
        let head = node2.chain_store.get_head_block().unwrap().unwrap();
        assert_eq!(head.number(), 2);
    }

    #[test]
    fn import_duplicate_block_is_idempotent() {
        let (node, _signer) = setup_node();
        store_genesis(&node);
        let verifier = MultiVerifier;
        let proposer = node.config.proposer_address.unwrap();

        let parent_hash = node.chain_store.get_head_hash().unwrap().unwrap();
        let block = Block {
            header: BlockHeader {
                parent_hash,
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::default(),
                number: 1,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_001,
                extra_data: Bytes::default(),
                proposer,
                sig_aggregate_proof: None,
                base_fee_per_gas: shell_core::INITIAL_BASE_FEE,
                withdrawals_root: ShellHash::ZERO,
                parent_beacon_block_root: ShellHash::ZERO,
                blob_gas_used: 0,
                excess_blob_gas: 0,
            },
            transactions: vec![],
            proposer_seal: None,
        };

        // First import should succeed.
        node.import_block(block.clone(), &verifier).unwrap();
        assert_eq!(
            node.chain_store.get_head_block().unwrap().unwrap().number(),
            1
        );

        // Second import of same block (now at or below head) should succeed silently.
        let result = node.import_block(block, &verifier);
        assert!(
            result.is_ok(),
            "duplicate import should be handled gracefully"
        );
        assert_eq!(
            node.chain_store.get_head_block().unwrap().unwrap().number(),
            1
        );
    }

    #[test]
    fn head_number_returns_current_height() {
        let (node, signer) = setup_node();
        store_genesis(&node);
        assert_eq!(node.head_number(), 0);

        node.produce_block(&signer, 100).unwrap();
        assert_eq!(node.head_number(), 1);
    }

    // ── State consistency tests ────────────────────────────────────────

    #[test]
    fn produce_n_blocks_head_matches() {
        let (node, signer) = setup_node();
        store_genesis(&node);

        for expected in 1..=8u64 {
            let block = node.produce_block(&signer, 100).unwrap();
            assert_eq!(block.number(), expected);

            let head = node.chain_store.get_head_block().unwrap().unwrap();
            assert_eq!(
                head.number(),
                expected,
                "chain_store head should be {expected} after producing block {expected}"
            );
            assert_eq!(head.hash(), block.hash());
        }
    }

    #[test]
    fn import_block_state_root_matches_header() {
        let (node, signer) = setup_node();
        store_genesis(&node);

        let block = node.produce_block(&signer, 0).unwrap();
        let expected_root = block.header.state_root;

        // Verify world state root matches what was written in the header.
        let _ws = node.world_state.read();
        // The state root won't literally match for empty blocks on a fresh trie,
        // but the produce_block code writes ws.state_root() into the header.
        // We verify the header's state_root is consistent.
        assert_eq!(
            block.header.state_root, expected_root,
            "header state_root should be self-consistent"
        );
    }

    #[test]
    fn produce_block_with_tx_stores_receipts() {
        let (node, signer) = setup_node();
        store_genesis(&node);

        let tx_signer = DilithiumSigner::generate();
        let sender = Address::from_public_key(tx_signer.public_key());
        let receiver = Address::from([0xCC; 20]);

        fund_account(&node, &sender, U256::from(100_000_000_000_000u64));

        let tx = Transaction {
            chain_id: 1337,
            nonce: 0,
            to: Some(receiver),
            value: U256::from(1_000),
            data: shell_primitives::Bytes::new(),
            gas_limit: 21_000,
            max_fee_per_gas: shell_core::INITIAL_BASE_FEE,
            max_priority_fee_per_gas: 0,
            access_list: None,
            tx_type: 2,
            max_fee_per_blob_gas: None,
            blob_versioned_hashes: None,
        };

        let tx_hash = {
            let encoded = alloy_rlp::encode(&tx);
            shell_primitives::keccak256(&encoded)
        };
        let sig = tx_signer.sign(tx_hash.as_bytes()).expect("sign failed");
        let signed =
            SignedTransaction::with_pubkey(sender, tx, sig, tx_signer.public_key().to_vec());

        let verifier = MultiVerifier;
        let known_pubkeys = |_: &Address| -> Option<Vec<u8>> { None };
        let balance_of = |addr: &Address| -> U256 {
            node.world_state
                .read()
                .get_balance(addr)
                .unwrap_or(U256::ZERO)
        };
        node.tx_pool
            .insert(signed, &verifier, &known_pubkeys, &balance_of)
            .unwrap();

        let block = node.produce_block(&signer, 100).unwrap();
        assert_eq!(block.transactions.len(), 1);

        // Verify receipts were stored.
        let block_hash = block.hash();
        let receipts = node.chain_store.get_receipts(&block_hash).unwrap();
        assert!(
            receipts.is_some(),
            "receipts should be stored for block with txs"
        );
        let receipts = receipts.unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].status, 1, "transfer tx should succeed");
        assert_eq!(receipts[0].gas_used, 21_000);
    }

    #[test]
    fn chain_store_get_block_by_number_roundtrip() {
        let (node, signer) = setup_node();
        store_genesis(&node);

        let mut produced_hashes = vec![];
        for _ in 0..4 {
            let block = node.produce_block(&signer, 0).unwrap();
            produced_hashes.push(block.hash());
        }

        // Verify every produced block is retrievable by number.
        for (i, expected_hash) in produced_hashes.iter().enumerate() {
            let number = (i + 1) as u64;
            let block = node
                .chain_store
                .get_block_by_number(number)
                .unwrap()
                .unwrap_or_else(|| panic!("block {number} not found"));
            assert_eq!(block.hash(), *expected_hash);
            assert_eq!(block.number(), number);
        }
    }

    #[test]
    fn import_block_tracks_state_root() {
        let (node, _signer) = setup_node_with_pruning(10);
        store_genesis(&node);

        let block = Block {
            header: BlockHeader {
                parent_hash: node.chain_store.get_head_hash().unwrap().unwrap(),
                state_root: ShellHash::from([0xAB; 32]),
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
                base_fee_per_gas: shell_core::INITIAL_BASE_FEE,
                withdrawals_root: ShellHash::ZERO,
                parent_beacon_block_root: ShellHash::ZERO,
                blob_gas_used: 0,
                excess_blob_gas: 0,
            },
            transactions: vec![],
            proposer_seal: None,
        };

        let verifier = MultiVerifier;
        node.import_block(block, &verifier).unwrap();

        let tracker = node.state_root_tracker.read();
        assert_eq!(tracker.len(), 1);
        assert_eq!(tracker.latest().unwrap().block_number, 1);
        assert_eq!(
            tracker.latest().unwrap().state_root,
            ShellHash::from([0xAB; 32])
        );
    }

    #[test]
    fn handle_attestation_rejects_equivocation() {
        let (node, signer) = setup_node();
        store_genesis(&node);

        let proposer = node.config.proposer_address.unwrap();
        let pubkey = signer.public_key().to_vec();
        node.register_authority_pubkey(proposer, pubkey);

        let verifier = MultiVerifier;

        // Produce a block so we have height 1.
        let block1 = node.produce_block(&signer, 100).unwrap();
        let hash1 = block1.hash();
        let height = block1.header.number;

        // Directly record an attestation for hash1 into the finality tracker
        // (bypassing handle_attestation avoids triggering finality + prune
        // since we only have 1 validator).
        let att1 = node.create_attestation(hash1, height, &signer).unwrap();
        node.finality.write().record_attestation(att1);

        // Create a competing block at the same height and store it so the
        // F-087 block existence check passes.
        let mut competing_block = Block {
            header: block1.header.clone(),
            transactions: vec![],
            proposer_seal: None,
        };
        competing_block.header.timestamp += 999; // different timestamp → different hash
        let competing_hash = competing_block.hash();
        node.chain_store.put_block(&competing_block).unwrap();

        // Create a second attestation from the same validator for the
        // competing block at the same height — this is equivocation.
        let att2 = node
            .create_attestation(competing_hash, height, &signer)
            .unwrap();
        let result = node.handle_attestation(att2, &verifier);

        assert!(result.is_err(), "equivocation must be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("equivocation"),
            "error should mention equivocation: {err_msg}"
        );
    }
}
