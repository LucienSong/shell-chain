use shell_core::{Account, Block, BlockHeader};
use shell_primitives::{keccak256, Address, Bytes, ShellHash};
use shell_storage::{ChainConfig, ChainStore, KvStore, StorageError, WorldState};

use crate::{AllocEntry, ConsensusConfig, GenesisConfig, GenesisError};

/// Initialize world state from genesis allocations and produce the genesis block.
///
/// Persists the genesis block into `chain_store` and writes chain configuration
/// (chain_id + genesis_hash) so that later boot-up can verify chain identity.
pub fn initialize_genesis<S: KvStore + 'static>(
    config: &GenesisConfig,
    store: std::sync::Arc<S>,
) -> Result<Block, GenesisError> {
    let mut world_state = WorldState::new(std::sync::Arc::clone(&store));

    // Apply allocations
    for (address, entry) in &config.alloc {
        apply_alloc(&mut world_state, address, entry)
            .map_err(|e| GenesisError::StateInit(e.to_string()))?;
    }

    // Compute state root
    let state_root = world_state
        .state_root()
        .map_err(|e| GenesisError::StateInit(e.to_string()))?;

    // Build genesis header
    let proposer = match &config.consensus {
        ConsensusConfig::PoA { authorities, .. } => {
            authorities.first().copied().unwrap_or(Address::ZERO)
        }
    };

    let header = BlockHeader {
        parent_hash: ShellHash::ZERO,
        state_root,
        transactions_root: ShellHash::ZERO,
        receipts_root: ShellHash::ZERO,
        logs_bloom: Bytes::new(),
        number: 0,
        gas_limit: config.gas_limit,
        gas_used: 0,
        timestamp: config.timestamp,
        extra_data: Bytes::copy_from_slice(config.extra_data.as_bytes()),
        proposer,
        sig_aggregate_proof: None,
    };

    let block = Block {
        header,
        transactions: vec![],
        proposer_seal: None,
    };

    // F-012 / F-013: Persist genesis block + canonical mapping + chain config
    let chain_store = ChainStore::new(std::sync::Arc::clone(&store));
    let genesis_hash = block.hash();

    chain_store
        .put_block(&block)
        .map_err(|e| GenesisError::StateInit(e.to_string()))?;
    chain_store
        .set_canonical(0, &genesis_hash)
        .map_err(|e| GenesisError::StateInit(e.to_string()))?;
    chain_store
        .set_head(&genesis_hash)
        .map_err(|e| GenesisError::StateInit(e.to_string()))?;
    chain_store
        .put_chain_config(&ChainConfig {
            chain_id: config.chain_id,
            genesis_hash,
        })
        .map_err(|e| GenesisError::StateInit(e.to_string()))?;

    Ok(block)
}

fn apply_alloc<S: KvStore + 'static>(
    world_state: &mut WorldState<S>,
    address: &Address,
    entry: &AllocEntry,
) -> Result<(), StorageError> {
    // Create account with the allocated balance
    let mut account = Account::new_eoa(ShellHash::ZERO, entry.balance);
    account.nonce = entry.nonce;

    // Set code hash if code is provided
    if let Some(ref code_hex) = entry.code {
        let code = hex::decode(code_hex.trim_start_matches("0x"))
            .map_err(|e| StorageError::Codec(e.to_string()))?;
        account.code_hash = Some(keccak256(&code));
    }

    world_state.set_account(address, &account)?;

    // Apply initial storage entries
    if let Some(ref storage) = entry.storage {
        for (key, value) in storage {
            world_state.set_storage(address, key, value)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::U256;
    use shell_storage::{ChainStore, MemoryDb};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_genesis() -> GenesisConfig {
        let mut alloc = HashMap::new();
        let addr1 = Address::ZERO;
        alloc.insert(
            addr1,
            AllocEntry {
                balance: U256::from(1_000_000u64),
                nonce: 0,
                code: None,
                storage: None,
            },
        );

        GenesisConfig {
            chain_id: 1337,
            chain_name: "test-chain".to_string(),
            timestamp: 1700000000,
            gas_limit: 30_000_000,
            extra_data: "genesis".to_string(),
            consensus: ConsensusConfig::PoA {
                authorities: vec![addr1],
                block_time_secs: 1,
                epoch_length: 0,
            },
            alloc,
        }
    }

    #[test]
    fn genesis_block_is_block_zero() {
        let config = test_genesis();
        let store = Arc::new(MemoryDb::new());
        let block = initialize_genesis(&config, store).unwrap();

        assert_eq!(block.number(), 0);
        assert!(block.header.is_genesis());
        assert!(block.transactions.is_empty());
        assert!(block.proposer_seal.is_none());
    }

    #[test]
    fn genesis_state_root_is_nonzero() {
        let config = test_genesis();
        let store = Arc::new(MemoryDb::new());
        let block = initialize_genesis(&config, store).unwrap();

        assert_ne!(block.header.state_root, ShellHash::ZERO);
    }

    #[test]
    fn genesis_allocations_applied() {
        let config = test_genesis();
        let store = Arc::new(MemoryDb::new());
        let block = initialize_genesis(&config, Arc::clone(&store)).unwrap();

        // Re-open world state at the genesis state root
        let ws = WorldState::at_root(store, &block.header.state_root).unwrap();
        let balance = ws.get_balance(&Address::ZERO).unwrap();
        assert_eq!(balance, U256::from(1_000_000u64));
    }

    #[test]
    fn genesis_deterministic() {
        let config = test_genesis();

        let store1 = Arc::new(MemoryDb::new());
        let block1 = initialize_genesis(&config, store1).unwrap();

        let store2 = Arc::new(MemoryDb::new());
        let block2 = initialize_genesis(&config, store2).unwrap();

        assert_eq!(block1.hash(), block2.hash());
        assert_eq!(block1.header.state_root, block2.header.state_root);
    }

    #[test]
    fn genesis_with_contract_code() {
        let mut config = test_genesis();
        let contract_addr = Address::from_public_key(keccak256(b"contract").as_bytes());
        config.alloc.insert(
            contract_addr,
            AllocEntry {
                balance: U256::ZERO,
                nonce: 1,
                code: Some("0x6080604052".to_string()),
                storage: None,
            },
        );

        let store = Arc::new(MemoryDb::new());
        let block = initialize_genesis(&config, Arc::clone(&store)).unwrap();

        let ws = WorldState::at_root(store, &block.header.state_root).unwrap();
        let acct = ws.get_account(&contract_addr).unwrap().unwrap();
        assert!(acct.is_contract());
        assert_eq!(acct.nonce, 1);
    }

    #[test]
    fn genesis_with_storage() {
        let mut config = test_genesis();
        let addr = Address::from_public_key(keccak256(b"storage-test").as_bytes());

        let slot = keccak256(b"slot-0");
        let value = keccak256(b"value-0");
        let mut storage = HashMap::new();
        storage.insert(slot, value);

        config.alloc.insert(
            addr,
            AllocEntry {
                balance: U256::from(100u64),
                nonce: 0,
                code: None,
                storage: Some(storage),
            },
        );

        let store = Arc::new(MemoryDb::new());
        let block = initialize_genesis(&config, Arc::clone(&store)).unwrap();

        let ws = WorldState::at_root(store, &block.header.state_root).unwrap();
        let stored = ws.get_storage(&addr, &slot).unwrap();
        assert_eq!(stored, value);
    }

    #[test]
    fn genesis_extra_data() {
        let config = test_genesis();
        let store = Arc::new(MemoryDb::new());
        let block = initialize_genesis(&config, store).unwrap();

        assert_eq!(block.header.extra_data.as_ref(), b"genesis");
    }

    #[test]
    fn genesis_persists_chain_config() {
        let config = test_genesis();
        let store = Arc::new(MemoryDb::new());
        let block = initialize_genesis(&config, Arc::clone(&store)).unwrap();

        let chain_store = ChainStore::new(store);
        let chain_cfg = chain_store.get_chain_config().unwrap().unwrap();
        assert_eq!(chain_cfg.chain_id, 1337);
        assert_eq!(chain_cfg.genesis_hash, block.hash());
    }

    #[test]
    fn genesis_sets_head_and_canonical() {
        let config = test_genesis();
        let store = Arc::new(MemoryDb::new());
        let block = initialize_genesis(&config, Arc::clone(&store)).unwrap();

        let chain_store = ChainStore::new(store);
        assert_eq!(chain_store.get_head_hash().unwrap().unwrap(), block.hash());
        let loaded = chain_store.get_block_by_number(0).unwrap().unwrap();
        assert_eq!(loaded.hash(), block.hash());
    }
}
