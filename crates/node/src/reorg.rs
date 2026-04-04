//! Chain reorganization engine.
//!
//! When a competing fork becomes the preferred chain, the reorg engine:
//! 1. Finds the common ancestor of current and target chains
//! 2. Rolls back state to the ancestor's state root
//! 3. Collects transactions from rolled-back blocks for mempool re-insertion
//! 4. Re-applies blocks on the new canonical chain
//! 5. Updates canonical chain pointers and head

use std::sync::Arc;

use parking_lot::RwLock;
use shell_primitives::ShellHash;
use shell_storage::{ChainStore, KvStore, WorldState};
use tracing::info;

use crate::error::NodeError;

/// Result of a chain reorganization.
#[derive(Debug)]
pub struct ReorgResult {
    /// Common ancestor block number.
    pub ancestor_number: u64,
    /// Common ancestor block hash.
    pub ancestor_hash: ShellHash,
    /// Number of blocks rolled back from the old chain.
    pub rolled_back: usize,
    /// Number of blocks applied from the new chain.
    pub applied: usize,
    /// Transactions from rolled-back blocks that should be re-added to mempool.
    pub reverted_txs: Vec<shell_core::SignedTransaction>,
    /// New head block hash after reorg.
    pub new_head: ShellHash,
}

/// Executes chain reorganizations.
pub struct ReorgEngine;

impl ReorgEngine {
    /// Execute a chain reorganization from the current head to a target fork.
    ///
    /// # Arguments
    /// * `chain_store` – block and canonical-mapping storage
    /// * `world_state` – current EVM world state (will be replaced)
    /// * `store` – underlying KV store used to reconstruct world state at a prior root
    /// * `ancestor_hash` – hash of the common ancestor block
    /// * `ancestor_number` – height of the common ancestor
    /// * `old_chain` – hashes of blocks to roll back, oldest first
    /// * `new_chain` – hashes of blocks to apply, oldest first
    /// * `finalized_number` – latest finalized block height (reorg cannot go past this)
    pub fn execute<S: KvStore>(
        chain_store: &Arc<ChainStore<S>>,
        world_state: &Arc<RwLock<WorldState<S>>>,
        store: &Arc<S>,
        ancestor_hash: ShellHash,
        ancestor_number: u64,
        old_chain: &[ShellHash],
        new_chain: &[ShellHash],
        finalized_number: u64,
    ) -> Result<ReorgResult, NodeError> {
        // Safety: cannot reorg past the finalized block
        if ancestor_number < finalized_number {
            return Err(NodeError::Startup(format!(
                "cannot reorg past finalized block {}: ancestor is at {}",
                finalized_number, ancestor_number
            )));
        }

        info!(
            ancestor = ancestor_number,
            rollback = old_chain.len(),
            apply = new_chain.len(),
            "starting chain reorganization"
        );

        // Step 1: Collect transactions from blocks being rolled back (newest first)
        let mut reverted_txs = Vec::new();
        for hash in old_chain.iter().rev() {
            if let Ok(Some(block)) = chain_store.get_block_by_hash(hash) {
                reverted_txs.extend(block.transactions.clone());
            }
        }

        // Step 2: Restore world state to the ancestor's state root
        let ancestor_block = chain_store
            .get_block_by_hash(&ancestor_hash)?
            .ok_or_else(|| {
                NodeError::Startup(format!("ancestor block not found: {:?}", ancestor_hash))
            })?;

        let new_ws =
            WorldState::at_root(Arc::clone(store), &ancestor_block.header.state_root)?;
        *world_state.write() = new_ws;

        info!(
            state_root = ?ancestor_block.header.state_root,
            "restored world state to ancestor"
        );

        // Step 3: Apply new chain blocks and update canonical mappings
        let mut applied = 0;
        let mut new_head = ancestor_hash;
        for hash in new_chain {
            let block =
                chain_store
                    .get_block_by_hash(hash)?
                    .ok_or_else(|| {
                        NodeError::Startup(format!("new chain block not found: {:?}", hash))
                    })?;

            chain_store.set_canonical(block.number(), hash)?;
            new_head = *hash;
            applied += 1;
        }

        // Step 4: Update head pointer
        chain_store.set_head(&new_head)?;

        // Step 5: Remove transactions that already exist in the new chain
        let new_chain_tx_hashes: std::collections::HashSet<ShellHash> = new_chain
            .iter()
            .filter_map(|h| chain_store.get_block_by_hash(h).ok().flatten())
            .flat_map(|b| b.transactions.iter().map(|tx| tx.hash()).collect::<Vec<_>>())
            .collect();

        reverted_txs.retain(|tx| !new_chain_tx_hashes.contains(&tx.hash()));

        let result = ReorgResult {
            ancestor_number,
            ancestor_hash,
            rolled_back: old_chain.len(),
            applied,
            reverted_txs,
            new_head,
        };

        info!(
            rolled_back = result.rolled_back,
            applied = result.applied,
            reverted_txs = result.reverted_txs.len(),
            new_head = ?result.new_head,
            "chain reorganization complete"
        );

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_core::{Block, BlockHeader, SignedTransaction, Transaction};
    use shell_crypto::{PQSignature, SignatureType};
    use shell_primitives::{Address, Bytes, U256};
    use shell_storage::MemoryDb;

    fn make_hash(n: u8) -> ShellHash {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        ShellHash::from(bytes)
    }

    fn make_block(number: u64, parent_hash: ShellHash, state_root: ShellHash) -> Block {
        let header = BlockHeader {
            parent_hash,
            state_root,
            transactions_root: ShellHash::default(),
            receipts_root: ShellHash::default(),
            logs_bloom: Bytes::default(),
            number,
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1_000_000 + number,
            extra_data: Bytes::default(),
            proposer: Address::from_public_key(b"test-proposer"),
            sig_aggregate_proof: None,
            base_fee_per_gas: 0,
        };
        Block {
            header,
            transactions: vec![],
            proposer_seal: None,
        }
    }

    /// Create a store + chain store + world state, returning the persisted empty
    /// state root so test blocks can reference it.
    fn setup_chain() -> (
        Arc<MemoryDb>,
        Arc<ChainStore<MemoryDb>>,
        Arc<RwLock<WorldState<MemoryDb>>>,
        ShellHash,
    ) {
        let store = Arc::new(MemoryDb::new());
        let chain_store = Arc::new(ChainStore::new(store.clone()));
        let mut ws = WorldState::new(store.clone());
        let empty_root = ws.state_root().unwrap();
        let world_state = Arc::new(RwLock::new(ws));
        (store, chain_store, world_state, empty_root)
    }

    fn make_tx() -> SignedTransaction {
        SignedTransaction::new(
            Address::from_public_key(b"sender"),
            Transaction {
                chain_id: 1,
                nonce: 0,
                max_fee_per_gas: 1_000_000_000,
                max_priority_fee_per_gas: 100_000,
                gas_limit: 21_000,
                to: None,
                value: U256::ZERO,
                data: Bytes::default(),
            },
            PQSignature::new(SignatureType::Dilithium3, vec![1, 2, 3]),
        )
    }

    #[test]
    fn test_reorg_past_finalized_rejected() {
        let (store, chain_store, world_state, _root) = setup_chain();
        let result = ReorgEngine::execute(
            &chain_store,
            &world_state,
            &store,
            make_hash(0),
            5,  // ancestor at 5
            &[],
            &[],
            10, // finalized at 10 — ancestor < finalized
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot reorg past finalized"));
    }

    #[test]
    fn test_empty_reorg() {
        let (store, chain_store, world_state, root) = setup_chain();

        let ancestor = make_block(5, make_hash(0), root);
        chain_store.put_block(&ancestor).unwrap();
        let ancestor_hash = ancestor.hash();

        let result = ReorgEngine::execute(
            &chain_store,
            &world_state,
            &store,
            ancestor_hash,
            5,
            &[], // nothing to roll back
            &[], // nothing to apply
            0,   // no finalized
        )
        .unwrap();

        assert_eq!(result.rolled_back, 0);
        assert_eq!(result.applied, 0);
        assert_eq!(result.reverted_txs.len(), 0);
    }

    #[test]
    fn test_reorg_collects_reverted_txs() {
        let (store, chain_store, world_state, root) = setup_chain();

        let ancestor = make_block(5, make_hash(0), root);
        chain_store.put_block(&ancestor).unwrap();
        let ancestor_hash = ancestor.hash();

        let mut old_block = make_block(6, ancestor_hash, root);
        old_block.transactions.push(make_tx());
        chain_store.put_block(&old_block).unwrap();
        let old_hash = old_block.hash();

        let result = ReorgEngine::execute(
            &chain_store,
            &world_state,
            &store,
            ancestor_hash,
            5,
            &[old_hash], // roll back this block
            &[],         // no new blocks
            0,
        )
        .unwrap();

        assert_eq!(result.rolled_back, 1);
        assert_eq!(result.reverted_txs.len(), 1);
    }

    #[test]
    fn test_reorg_applies_new_chain() {
        let (store, chain_store, world_state, root) = setup_chain();

        let ancestor = make_block(5, make_hash(0), root);
        chain_store.put_block(&ancestor).unwrap();
        let ancestor_hash = ancestor.hash();

        let new_block_6 = make_block(6, ancestor_hash, root);
        chain_store.put_block(&new_block_6).unwrap();
        let new_hash_6 = new_block_6.hash();

        let new_block_7 = make_block(7, new_hash_6, root);
        chain_store.put_block(&new_block_7).unwrap();
        let new_hash_7 = new_block_7.hash();

        let result = ReorgEngine::execute(
            &chain_store,
            &world_state,
            &store,
            ancestor_hash,
            5,
            &[],                       // nothing to roll back
            &[new_hash_6, new_hash_7], // apply these
            0,
        )
        .unwrap();

        assert_eq!(result.applied, 2);
        assert_eq!(result.new_head, new_hash_7);
    }

    #[test]
    fn test_reorg_filters_duplicate_txs() {
        let (store, chain_store, world_state, root) = setup_chain();

        let ancestor = make_block(5, make_hash(0), root);
        chain_store.put_block(&ancestor).unwrap();
        let ancestor_hash = ancestor.hash();

        let tx = make_tx();

        let mut old_block = make_block(6, ancestor_hash, root);
        old_block.transactions.push(tx.clone());
        chain_store.put_block(&old_block).unwrap();
        let old_hash = old_block.hash();

        let mut new_block = make_block(6, ancestor_hash, root);
        new_block.header.timestamp += 1; // different block, same tx
        new_block.transactions.push(tx);
        chain_store.put_block(&new_block).unwrap();
        let new_hash = new_block.hash();

        let result = ReorgEngine::execute(
            &chain_store,
            &world_state,
            &store,
            ancestor_hash,
            5,
            &[old_hash],
            &[new_hash],
            0,
        )
        .unwrap();

        // TX exists in new chain, so it should be filtered from reverted
        assert_eq!(result.reverted_txs.len(), 0);
    }
}
