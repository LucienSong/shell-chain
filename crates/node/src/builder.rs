//! Ergonomic node builder for assembling shell-chain components.

use std::sync::Arc;

use parking_lot::RwLock;

use crate::config::ConsensusEngineConfig;
use shell_consensus::{ConsensusEngine, PoaEngine, WPoaEngine};
use shell_mempool::TxPool;
use shell_storage::{ChainStore, KvStore, MemoryDb, WorldState};

use crate::config::NodeConfig;
use crate::error::NodeError;
use crate::node::Node;

/// Builder for constructing a `Node` with all required components.
///
/// # Example (dev mode with in-memory storage)
/// ```ignore
/// let node = NodeBuilder::new(NodeConfig::dev(authority))
///     .with_memory_storage()
///     .build()?;
/// ```
pub struct NodeBuilder<S: KvStore + 'static> {
    config: NodeConfig,
    store: Option<Arc<S>>,
}

impl NodeBuilder<MemoryDb> {
    /// Create a builder for an in-memory dev node.
    pub fn new_dev(config: NodeConfig) -> NodeBuilder<MemoryDb> {
        let db = Arc::new(MemoryDb::new());
        NodeBuilder {
            config,
            store: Some(db),
        }
    }
}

impl<S: KvStore + 'static> NodeBuilder<S> {
    /// Create a builder with a custom KvStore backend.
    pub fn new(config: NodeConfig, store: Arc<S>) -> Self {
        Self {
            config,
            store: Some(store),
        }
    }

    /// Build the node, wiring all components together.
    ///
    /// Automatically detects whether the chain has been initialized:
    /// if a head block exists, WorldState resumes from its state root;
    /// otherwise, WorldState starts empty (pre-genesis).
    ///
    /// Returns an error rather than starting with empty state when persisted
    /// chain data cannot be read or validated.
    pub fn build(mut self) -> Result<(Node<S>, Arc<S>), NodeError> {
        let store = self.store.take().expect("store must be set");

        let chain_store = Arc::new(ChainStore::new(store.clone()));

        let cache_mb = self.config.state_cache_size_mb;

        // Resume from existing chain state if available.
        let world_state = match chain_store.get_head_block()? {
            Some(head) => {
                let state_root = head.header.state_root;
                let block_number = head.number();
                let mut ws = WorldState::at_root_with_cache_mb(store.clone(), &state_root, cache_mb)
                    .map_err(|error| {
                        NodeError::Startup(format!(
                            "failed to open world state at head #{block_number} ({state_root}): {error}"
                        ))
                    })?;
                ws.validate().map_err(|error| {
                    NodeError::Startup(format!(
                        "world state validation failed at head #{block_number} ({state_root}): {error}"
                    ))
                })?;
                Arc::new(RwLock::new(ws))
            }
            None => Arc::new(RwLock::new(WorldState::new_with_cache_mb(
                store.clone(),
                cache_mb,
            ))),
        };

        let consensus: Arc<RwLock<dyn ConsensusEngine>> = match &self.config.consensus {
            ConsensusEngineConfig::Poa(poa_cfg) => {
                Arc::new(RwLock::new(PoaEngine::new(poa_cfg.clone())))
            }
            ConsensusEngineConfig::WPoa(wpoa_cfg) => Arc::new(RwLock::new(WPoaEngine::new(
                wpoa_cfg.clone(),
                Arc::new(shell_crypto::MultiVerifier),
            ))),
        };
        let tx_pool = Arc::new(TxPool::new(self.config.mempool.clone()));

        let node = Node::new(
            self.config,
            store.clone(),
            chain_store,
            world_state,
            tx_pool,
            consensus,
        );

        Ok((node, store))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_core::{Block, BlockHeader};
    use shell_primitives::{Address, ShellHash};
    use shell_storage::{StorageError, WriteBatch};

    struct FailingReadStore;

    impl KvStore for FailingReadStore {
        fn get(&self, _key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
            Err(StorageError::Database("injected read failure".into()))
        }

        fn put(&self, _key: &[u8], _value: &[u8]) -> Result<(), StorageError> {
            Ok(())
        }

        fn delete(&self, _key: &[u8]) -> Result<(), StorageError> {
            Ok(())
        }

        fn flush(&self) -> Result<(), StorageError> {
            Ok(())
        }

        fn write_batch(&self, _batch: WriteBatch) -> Result<(), StorageError> {
            Ok(())
        }

        fn scan_prefix(&self, _prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StorageError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn build_dev_node() {
        let authority = Address::from_public_key(b"test-authority", 0);
        let config = NodeConfig::dev(authority);
        let (node, _store) = NodeBuilder::new_dev(config).build().unwrap();

        assert_eq!(node.config.chain_id, 1337);
        assert_eq!(node.tx_pool.len(), 0);
    }

    #[test]
    fn build_propagates_head_read_failure() {
        let authority = Address::from_public_key(b"test-authority", 0);
        let config = NodeConfig::dev(authority);

        let result = NodeBuilder::new(config, Arc::new(FailingReadStore)).build();
        let err = match result {
            Ok(_) => panic!("node build unexpectedly succeeded"),
            Err(err) => err,
        };

        assert!(matches!(err, NodeError::Storage(StorageError::Database(_))));
    }

    #[test]
    fn build_rejects_missing_head_state_root() {
        let authority = Address::from_public_key(b"test-authority", 0);
        let config = NodeConfig::dev(authority);
        let store = Arc::new(MemoryDb::new());
        let chain_store = ChainStore::new(store.clone());
        let block = Block {
            header: BlockHeader {
                state_root: ShellHash::from([0x42; 32]),
                ..BlockHeader::default()
            },
            transactions: Vec::new(),
            system_transactions: Vec::new(),
            proposer_seal: None,
        };
        let block_hash = block.hash();
        chain_store.put_block(&block).unwrap();
        chain_store.set_head(&block_hash).unwrap();

        let result = NodeBuilder::new(config, store).build();
        let err = match result {
            Ok(_) => panic!("node build unexpectedly succeeded"),
            Err(err) => err,
        };

        assert!(matches!(
            err,
            NodeError::Startup(message) if message.contains("world state validation failed")
        ));
    }
}
