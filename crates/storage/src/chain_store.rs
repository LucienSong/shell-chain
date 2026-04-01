use std::sync::Arc;

use serde::{Deserialize, Serialize};
use shell_core::{Block, BlockHeader, TransactionReceipt};
use shell_primitives::{Address, ShellHash};

use crate::{KvStore, StorageError};

/// Persistent chain configuration (written once at genesis).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChainConfig {
    pub chain_id: u64,
    pub genesis_hash: ShellHash,
}

/// Key prefixes for chain store data. All keys are prefixed to avoid
/// collisions when sharing a single [`KvStore`] instance.
mod prefix {
    pub const HEADER_BY_HASH: &[u8] = b"h/";
    pub const BODY_BY_HASH: &[u8] = b"b/";
    pub const HASH_BY_NUMBER: &[u8] = b"n/";
    pub const RECEIPTS_BY_HASH: &[u8] = b"r/";
    pub const TX_INDEX: &[u8] = b"t/";
    pub const HEAD_BLOCK: &[u8] = b"HEAD";
    pub const CHAIN_CONFIG: &[u8] = b"CFG";
    pub const CODE_BY_HASH: &[u8] = b"c/";
    pub const PUBKEY_BY_ADDR: &[u8] = b"pk/";
}

/// Block/receipt/transaction-index storage.
///
/// Provides chain-level data access: store and retrieve blocks by number or
/// hash, store transaction receipts, and maintain a transaction → block index.
pub struct ChainStore<S: KvStore> {
    store: Arc<S>,
}

impl<S: KvStore> ChainStore<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    // ── Key helpers ────────────────────────────────────────────

    fn header_key(hash: &ShellHash) -> Vec<u8> {
        [prefix::HEADER_BY_HASH, hash.as_bytes()].concat()
    }

    fn body_key(hash: &ShellHash) -> Vec<u8> {
        [prefix::BODY_BY_HASH, hash.as_bytes()].concat()
    }

    fn number_key(number: u64) -> Vec<u8> {
        [prefix::HASH_BY_NUMBER, &number.to_be_bytes()].concat()
    }

    fn receipts_key(block_hash: &ShellHash) -> Vec<u8> {
        [prefix::RECEIPTS_BY_HASH, block_hash.as_bytes()].concat()
    }

    fn tx_index_key(tx_hash: &ShellHash) -> Vec<u8> {
        [prefix::TX_INDEX, tx_hash.as_bytes()].concat()
    }

    fn code_key(code_hash: &ShellHash) -> Vec<u8> {
        [prefix::CODE_BY_HASH, code_hash.as_bytes()].concat()
    }

    fn pubkey_key(address: &Address) -> Vec<u8> {
        [prefix::PUBKEY_BY_ADDR, address.as_ref()].concat()
    }

    // ── Block operations ───────────────────────────────────────

    /// Store a block (header + body) and update the transaction index.
    ///
    /// Does **not** update HEAD or the canonical number→hash index.
    /// Callers must explicitly call [`set_canonical`] and [`set_head`]
    /// to mark a block as part of the canonical chain.
    pub fn put_block(&self, block: &Block) -> Result<(), StorageError> {
        let block_hash = block.hash();

        // Store header (JSON for now; RLP later if perf-critical)
        let header_bytes = serde_json::to_vec(&block.header)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.store.put(&Self::header_key(&block_hash), &header_bytes)?;

        // Store body (full block JSON)
        let body_bytes = serde_json::to_vec(block)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.store.put(&Self::body_key(&block_hash), &body_bytes)?;

        // Transaction → (block_hash, tx_index) mapping
        for (i, tx) in block.transactions.iter().enumerate() {
            let tx_hash = tx.hash();
            let mut index_value = block_hash.as_bytes().to_vec();
            index_value.extend_from_slice(&(i as u32).to_be_bytes());
            self.store.put(&Self::tx_index_key(&tx_hash), &index_value)?;
        }

        Ok(())
    }

    /// Mark a block number → hash mapping in the canonical chain index.
    pub fn set_canonical(&self, number: u64, hash: &ShellHash) -> Result<(), StorageError> {
        self.store.put(&Self::number_key(number), hash.as_bytes())
    }

    /// Set the HEAD pointer to the given block hash.
    pub fn set_head(&self, hash: &ShellHash) -> Result<(), StorageError> {
        self.store.put(prefix::HEAD_BLOCK, hash.as_bytes())
    }

    /// Get a block by its hash.
    pub fn get_block_by_hash(&self, hash: &ShellHash) -> Result<Option<Block>, StorageError> {
        match self.store.get(&Self::body_key(hash))? {
            Some(data) => {
                let block: Block = serde_json::from_slice(&data)
                    .map_err(|e| StorageError::Codec(e.to_string()))?;
                Ok(Some(block))
            }
            None => Ok(None),
        }
    }

    /// Get a block by its number.
    pub fn get_block_by_number(&self, number: u64) -> Result<Option<Block>, StorageError> {
        let hash_bytes = match self.store.get(&Self::number_key(number))? {
            Some(b) => b,
            None => return Ok(None),
        };
        let hash = ShellHash::try_from_slice(&hash_bytes)
            .map_err(|e| StorageError::Codec(e.to_string()))?;
        self.get_block_by_hash(&hash)
    }

    /// Get a block header by hash.
    pub fn get_header_by_hash(
        &self,
        hash: &ShellHash,
    ) -> Result<Option<BlockHeader>, StorageError> {
        match self.store.get(&Self::header_key(hash))? {
            Some(data) => {
                let header: BlockHeader = serde_json::from_slice(&data)
                    .map_err(|e| StorageError::Codec(e.to_string()))?;
                Ok(Some(header))
            }
            None => Ok(None),
        }
    }

    /// Get the HEAD (latest) block hash.
    pub fn get_head_hash(&self) -> Result<Option<ShellHash>, StorageError> {
        match self.store.get(prefix::HEAD_BLOCK)? {
            Some(data) => {
                let hash = ShellHash::try_from_slice(&data)
                    .map_err(|e| StorageError::Codec(e.to_string()))?;
                Ok(Some(hash))
            }
            None => Ok(None),
        }
    }

    /// Get the HEAD (latest) block.
    pub fn get_head_block(&self) -> Result<Option<Block>, StorageError> {
        match self.get_head_hash()? {
            Some(hash) => self.get_block_by_hash(&hash),
            None => Ok(None),
        }
    }

    // ── Receipt operations ─────────────────────────────────────

    /// Store receipts for a block.
    pub fn put_receipts(
        &self,
        block_hash: &ShellHash,
        receipts: &[TransactionReceipt],
    ) -> Result<(), StorageError> {
        let data = serde_json::to_vec(receipts)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.store.put(&Self::receipts_key(block_hash), &data)
    }

    /// Get receipts for a block.
    pub fn get_receipts(
        &self,
        block_hash: &ShellHash,
    ) -> Result<Option<Vec<TransactionReceipt>>, StorageError> {
        match self.store.get(&Self::receipts_key(block_hash))? {
            Some(data) => {
                let receipts: Vec<TransactionReceipt> = serde_json::from_slice(&data)
                    .map_err(|e| StorageError::Codec(e.to_string()))?;
                Ok(Some(receipts))
            }
            None => Ok(None),
        }
    }

    // ── Transaction index ──────────────────────────────────────

    /// Look up which block contains a given transaction.
    /// Returns (block_hash, tx_index_in_block).
    pub fn get_tx_location(
        &self,
        tx_hash: &ShellHash,
    ) -> Result<Option<(ShellHash, u32)>, StorageError> {
        match self.store.get(&Self::tx_index_key(tx_hash))? {
            Some(data) => {
                if data.len() != 36 {
                    return Err(StorageError::Codec("invalid tx index entry".into()));
                }
                let block_hash = ShellHash::try_from_slice(&data[..32])
                    .map_err(|e| StorageError::Codec(e.to_string()))?;
                let tx_idx = u32::from_be_bytes(data[32..36].try_into().unwrap());
                Ok(Some((block_hash, tx_idx)))
            }
            None => Ok(None),
        }
    }

    // ── Chain config ───────────────────────────────────────────

    /// Persist the chain configuration (chain_id + genesis hash).
    /// Should be called exactly once after genesis initialization.
    pub fn put_chain_config(&self, config: &ChainConfig) -> Result<(), StorageError> {
        let data = serde_json::to_vec(config)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.store.put(prefix::CHAIN_CONFIG, &data)
    }

    /// Retrieve the persisted chain configuration.
    pub fn get_chain_config(&self) -> Result<Option<ChainConfig>, StorageError> {
        match self.store.get(prefix::CHAIN_CONFIG)? {
            Some(data) => {
                let config: ChainConfig = serde_json::from_slice(&data)
                    .map_err(|e| StorageError::Codec(e.to_string()))?;
                Ok(Some(config))
            }
            None => Ok(None),
        }
    }

    // ── Contract code storage ──────────────────────────────────

    /// Store contract bytecode keyed by its hash.
    ///
    /// The caller is responsible for computing `keccak256(code)` and passing
    /// it as `code_hash`. The code can later be retrieved by hash via
    /// [`get_code`].
    pub fn put_code(&self, code_hash: &ShellHash, code: &[u8]) -> Result<(), StorageError> {
        self.store.put(&Self::code_key(code_hash), code)
    }

    /// Retrieve contract bytecode by its hash.
    pub fn get_code(&self, code_hash: &ShellHash) -> Result<Option<Vec<u8>>, StorageError> {
        self.store.get(&Self::code_key(code_hash))
    }

    // ── PQ public key registry ─────────────────────────────────

    /// Register a PQ public key for an address.
    ///
    /// Called on the first transaction from this address (the Hybrid
    /// registration model). Subsequent transactions skip pubkey transfer
    /// and read from this registry.
    pub fn put_pubkey(&self, address: &Address, pubkey: &[u8]) -> Result<(), StorageError> {
        self.store.put(&Self::pubkey_key(address), pubkey)
    }

    /// Retrieve the registered PQ public key for an address.
    pub fn get_pubkey(&self, address: &Address) -> Result<Option<Vec<u8>>, StorageError> {
        self.store.get(&Self::pubkey_key(address))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryDb;
    use shell_primitives::{Address, Bytes};

    fn empty_block(number: u64) -> Block {
        Block {
            header: BlockHeader {
                parent_hash: ShellHash::ZERO,
                state_root: ShellHash::ZERO,
                transactions_root: ShellHash::ZERO,
                receipts_root: ShellHash::ZERO,
                logs_bloom: Bytes::new(),
                number,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1700000000 + number,
                extra_data: Bytes::new(),
                proposer: Address::ZERO,
                sig_aggregate_proof: None,
            },
            transactions: vec![],
            proposer_seal: None,
        }
    }

    /// Helper: put block + set canonical + set head (mimics old behavior).
    fn put_canonical(cs: &ChainStore<MemoryDb>, block: &Block) {
        let hash = block.hash();
        cs.put_block(block).unwrap();
        cs.set_canonical(block.number(), &hash).unwrap();
        cs.set_head(&hash).unwrap();
    }

    #[test]
    fn put_and_get_by_hash() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let block = empty_block(0);
        let hash = block.hash();

        cs.put_block(&block).unwrap();
        let loaded = cs.get_block_by_hash(&hash).unwrap().unwrap();
        assert_eq!(loaded.header, block.header);
    }

    #[test]
    fn put_block_does_not_set_head() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let block = empty_block(0);

        cs.put_block(&block).unwrap();
        // HEAD should still be None — put_block no longer sets it
        assert!(cs.get_head_hash().unwrap().is_none());
    }

    #[test]
    fn put_block_does_not_set_canonical() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let block = empty_block(42);

        cs.put_block(&block).unwrap();
        // Number→hash should not be set
        assert!(cs.get_block_by_number(42).unwrap().is_none());
    }

    #[test]
    fn set_canonical_and_get_by_number() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let block = empty_block(42);
        let hash = block.hash();

        cs.put_block(&block).unwrap();
        cs.set_canonical(42, &hash).unwrap();
        let loaded = cs.get_block_by_number(42).unwrap().unwrap();
        assert_eq!(loaded.number(), 42);
    }

    #[test]
    fn set_head_and_get_head() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let block = empty_block(0);
        let hash = block.hash();

        cs.put_block(&block).unwrap();
        cs.set_head(&hash).unwrap();
        assert_eq!(cs.get_head_hash().unwrap().unwrap(), hash);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        assert!(cs.get_block_by_number(999).unwrap().is_none());
        assert!(cs.get_block_by_hash(&ShellHash::ZERO).unwrap().is_none());
    }

    #[test]
    fn head_block_tracking() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);

        assert!(cs.get_head_hash().unwrap().is_none());

        let b0 = empty_block(0);
        put_canonical(&cs, &b0);
        assert_eq!(cs.get_head_hash().unwrap().unwrap(), b0.hash());

        let mut b1 = empty_block(1);
        b1.header.parent_hash = b0.hash();
        put_canonical(&cs, &b1);
        assert_eq!(cs.get_head_hash().unwrap().unwrap(), b1.hash());
    }

    #[test]
    fn header_retrieval() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let block = empty_block(5);
        let hash = block.hash();

        cs.put_block(&block).unwrap();
        let header = cs.get_header_by_hash(&hash).unwrap().unwrap();
        assert_eq!(header.number, 5);
    }

    #[test]
    fn receipt_storage() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let block = empty_block(0);
        let hash = block.hash();
        cs.put_block(&block).unwrap();

        let receipts = vec![TransactionReceipt {
            tx_hash: ShellHash::ZERO,
            block_number: 0,
            tx_index: 0,
            status: 1,
            gas_used: 21000,
            cumulative_gas_used: 21000,
            contract_address: None,
            logs_bloom: Bytes::new(),
            logs: vec![],
        }];

        cs.put_receipts(&hash, &receipts).unwrap();
        let loaded = cs.get_receipts(&hash).unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].status, 1);
    }

    #[test]
    fn multiple_blocks_chain() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);

        let b0 = empty_block(0);
        put_canonical(&cs, &b0);

        let mut b1 = empty_block(1);
        b1.header.parent_hash = b0.hash();
        put_canonical(&cs, &b1);

        let mut b2 = empty_block(2);
        b2.header.parent_hash = b1.hash();
        put_canonical(&cs, &b2);

        // All blocks retrievable
        assert!(cs.get_block_by_number(0).unwrap().is_some());
        assert!(cs.get_block_by_number(1).unwrap().is_some());
        assert!(cs.get_block_by_number(2).unwrap().is_some());
        assert_eq!(cs.get_head_hash().unwrap().unwrap(), b2.hash());
    }

    #[test]
    fn chain_config_roundtrip() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);

        assert!(cs.get_chain_config().unwrap().is_none());

        let config = ChainConfig {
            chain_id: 1337,
            genesis_hash: ShellHash::ZERO,
        };
        cs.put_chain_config(&config).unwrap();

        let loaded = cs.get_chain_config().unwrap().unwrap();
        assert_eq!(loaded.chain_id, 1337);
        assert_eq!(loaded.genesis_hash, ShellHash::ZERO);
    }

    #[test]
    fn code_storage_roundtrip() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let code = b"\x60\x80\x60\x40\x52"; // PUSH1 0x80 PUSH1 0x40 MSTORE
        let code_hash = shell_primitives::keccak256(code);

        assert!(cs.get_code(&code_hash).unwrap().is_none());

        cs.put_code(&code_hash, code).unwrap();
        let loaded = cs.get_code(&code_hash).unwrap().unwrap();
        assert_eq!(loaded, code);
    }

    #[test]
    fn pubkey_registry_roundtrip() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let addr = Address::ZERO;
        let fake_pubkey = vec![0xAA; 1952]; // Dilithium3 pubkey size

        assert!(cs.get_pubkey(&addr).unwrap().is_none());

        cs.put_pubkey(&addr, &fake_pubkey).unwrap();
        let loaded = cs.get_pubkey(&addr).unwrap().unwrap();
        assert_eq!(loaded.len(), 1952);
        assert_eq!(loaded, fake_pubkey);
    }

    #[test]
    fn pubkey_overwrite() {
        let store = Arc::new(MemoryDb::new());
        let cs = ChainStore::new(store);
        let addr = Address::ZERO;

        cs.put_pubkey(&addr, &[1; 100]).unwrap();
        cs.put_pubkey(&addr, &[2; 200]).unwrap();

        let loaded = cs.get_pubkey(&addr).unwrap().unwrap();
        assert_eq!(loaded, vec![2; 200]);
    }
}
