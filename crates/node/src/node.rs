//! Running node with event loop and block production.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use tokio::sync::watch;

use shell_consensus::{ConsensusEngine, PoaEngine};
use shell_core::{Block, BlockHeader, SignedTransaction};
use shell_crypto::{DilithiumVerifier, Signer, Verifier};
use shell_evm::{ShellEvm, ShellStateDb};
use shell_mempool::TxPool;
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
    pub chain_store: Arc<ChainStore<S>>,
    pub world_state: Arc<RwLock<WorldState<S>>>,
    pub tx_pool: Arc<TxPool>,
    pub consensus: Arc<PoaEngine>,
    shutdown_tx: watch::Sender<bool>,
}

impl<S: KvStore + 'static> Node<S> {
    /// Create a new node from pre-built components.
    pub fn new(
        config: NodeConfig,
        chain_store: Arc<ChainStore<S>>,
        world_state: Arc<RwLock<WorldState<S>>>,
        tx_pool: Arc<TxPool>,
        consensus: Arc<PoaEngine>,
    ) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            config,
            chain_store,
            world_state,
            tx_pool,
            consensus,
            shutdown_tx,
        }
    }

    /// Signal the node to shut down.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Produce a block from pending mempool transactions.
    ///
    /// Collects up to `max_txs` transactions, executes each through the EVM,
    /// assembles a block, and commits it to storage. Returns the new block.
    pub fn produce_block(
        &self,
        state_store: Arc<S>,
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

        // Create an isolated EVM instance backed by the same store.
        let ws = WorldState::new(state_store.clone());
        let cs = ChainStore::new(state_store);
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
    pub fn import_block(
        &self,
        block: Block,
        _verifier: &dyn Verifier,
    ) -> Result<(), NodeError> {
        let head = self
            .chain_store
            .get_head_block()?
            .ok_or(NodeError::NoGenesis)?;

        // Basic ordering check.
        if block.number() != head.number() + 1 {
            return Err(NodeError::Startup(format!(
                "block {} does not follow head {}",
                block.number(),
                head.number()
            )));
        }

        // Verify consensus rules.
        self.consensus.verify_header(&block.header)?;

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
    use shell_crypto::DilithiumSigner;
    use shell_mempool::MempoolConfig;
    use shell_storage::MemoryDb;

    fn setup_node() -> (Node<MemoryDb>, Arc<MemoryDb>) {
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
        let node = Node::new(config, chain_store, world_state, tx_pool, consensus);
        (node, db)
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

    #[test]
    fn node_creation() {
        let (node, _) = setup_node();
        assert_eq!(node.config.chain_id, 1337);
        assert!(node.config.proposer_address.is_some());
    }

    #[test]
    fn produce_empty_block() {
        let (node, db) = setup_node();
        store_genesis(&node);

        let signer = DilithiumSigner::generate();
        let block = node.produce_block(db, &signer, 100).unwrap();
        assert_eq!(block.number(), 1);
        assert!(block.transactions.is_empty());
        assert!(block.proposer_seal.is_some());
    }

    #[test]
    fn import_block() {
        let (node, _) = setup_node();
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
    fn shutdown_signal() {
        let (node, _) = setup_node();
        let rx = node.shutdown_tx.subscribe();
        assert!(!*rx.borrow());

        node.shutdown();
        assert!(*rx.borrow());
    }
}
