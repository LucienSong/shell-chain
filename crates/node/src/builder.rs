//! Ergonomic node builder for assembling shell-chain components.

use std::sync::Arc;

use parking_lot::RwLock;

use shell_consensus::PoaEngine;
use shell_mempool::TxPool;
use shell_storage::{ChainStore, KvStore, MemoryDb, WorldState};

use crate::config::NodeConfig;
use crate::node::Node;

/// Builder for constructing a `Node` with all required components.
///
/// # Example (dev mode with in-memory storage)
/// ```ignore
/// let node = NodeBuilder::new(NodeConfig::dev(authority))
///     .with_memory_storage()
///     .build();
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
    pub fn build(mut self) -> (Node<S>, Arc<S>) {
        let store = self.store.take().expect("store must be set");

        let chain_store = Arc::new(ChainStore::new(store.clone()));

        // Resume from existing chain state if available.
        let world_state = match chain_store.get_head_block() {
            Ok(Some(head)) => {
                match WorldState::at_root(store.clone(), &head.header.state_root) {
                    Ok(ws) => Arc::new(RwLock::new(ws)),
                    Err(_) => Arc::new(RwLock::new(WorldState::new(store.clone()))),
                }
            }
            _ => Arc::new(RwLock::new(WorldState::new(store.clone()))),
        };

        let consensus = Arc::new(PoaEngine::new(self.config.consensus.clone()));
        let tx_pool = Arc::new(TxPool::new(self.config.mempool.clone()));

        let node = Node::new(
            self.config,
            store.clone(),
            chain_store,
            world_state,
            tx_pool,
            consensus,
        );

        (node, store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::Address;

    #[test]
    fn build_dev_node() {
        let authority = Address::from_public_key(b"test-authority");
        let config = NodeConfig::dev(authority);
        let (node, _store) = NodeBuilder::new_dev(config).build();

        assert_eq!(node.config.chain_id, 1337);
        assert_eq!(node.tx_pool.len(), 0);
    }
}
