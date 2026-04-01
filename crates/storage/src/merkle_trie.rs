use std::sync::Arc;

use eth_trie::{EthTrie, Trie};
use ethereum_types::H256;

use crate::{KvStore, StorageError};
use crate::trie_adapter::KvStoreTrieDb;

/// Ethereum-compatible Merkle Patricia Trie backed by any [`KvStore`].
///
/// Wraps [`eth_trie::EthTrie`] and maps errors to [`StorageError::Trie`].
/// Produces state roots identical to Ethereum given the same inputs.
pub struct MerkleTrie<S: KvStore> {
    trie: EthTrie<KvStoreTrieDb<S>>,
}

impl<S: KvStore + 'static> MerkleTrie<S> {
    /// Create a new empty trie.
    pub fn new(store: Arc<S>) -> Self {
        let db = Arc::new(KvStoreTrieDb::new(store));
        Self {
            trie: EthTrie::new(db),
        }
    }

    /// Open an existing trie at the given root hash (32 bytes).
    pub fn at_root(store: Arc<S>, root: &[u8; 32]) -> Result<Self, StorageError> {
        let db = Arc::new(KvStoreTrieDb::new(store));
        let base = EthTrie::new(Arc::clone(&db));
        let root_hash = H256::from_slice(root);
        Ok(Self {
            trie: base.at_root(root_hash),
        })
    }

    /// Get a value by key.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.trie
            .get(key)
            .map_err(|e| StorageError::Trie(e.to_string()))
    }

    /// Check if a key exists.
    pub fn contains(&self, key: &[u8]) -> Result<bool, StorageError> {
        self.trie
            .contains(key)
            .map_err(|e| StorageError::Trie(e.to_string()))
    }

    /// Insert or update a key-value pair.
    pub fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.trie
            .insert(key, value)
            .map_err(|e| StorageError::Trie(e.to_string()))
    }

    /// Remove a key from the trie.
    pub fn remove(&mut self, key: &[u8]) -> Result<bool, StorageError> {
        self.trie
            .remove(key)
            .map_err(|e| StorageError::Trie(e.to_string()))
    }

    /// Compute and return the 32-byte trie root hash.
    pub fn root_hash(&mut self) -> Result<[u8; 32], StorageError> {
        let h256 = self
            .trie
            .root_hash()
            .map_err(|e| StorageError::Trie(e.to_string()))?;
        Ok(h256.0)
    }

    /// Generate a Merkle proof for the given key.
    pub fn get_proof(&mut self, key: &[u8]) -> Result<Vec<Vec<u8>>, StorageError> {
        self.trie
            .get_proof(key)
            .map_err(|e| StorageError::Trie(e.to_string()))
    }

    /// Verify a Merkle proof against a root hash.
    pub fn verify_proof(
        store: Arc<S>,
        root_hash: &[u8; 32],
        key: &[u8],
        proof: Vec<Vec<u8>>,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let db = Arc::new(KvStoreTrieDb::new(store));
        let trie = EthTrie::new(db);
        let h256 = H256::from_slice(root_hash);
        trie.verify_proof(h256, key, proof)
            .map_err(|e| StorageError::Trie(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryDb;

    #[test]
    fn empty_trie_root_is_deterministic() {
        let db = Arc::new(MemoryDb::new());
        let mut trie = MerkleTrie::new(db);
        let root = trie.root_hash().unwrap();
        assert_eq!(root.len(), 32);

        let db2 = Arc::new(MemoryDb::new());
        let mut trie2 = MerkleTrie::new(db2);
        assert_eq!(root, trie2.root_hash().unwrap());
    }

    #[test]
    fn insert_and_get() {
        let db = Arc::new(MemoryDb::new());
        let mut trie = MerkleTrie::new(db);

        trie.insert(b"key1", b"value1").unwrap();
        assert_eq!(trie.get(b"key1").unwrap(), Some(b"value1".to_vec()));
        assert_eq!(trie.get(b"key2").unwrap(), None);
    }

    #[test]
    fn insert_changes_root() {
        let db = Arc::new(MemoryDb::new());
        let mut trie = MerkleTrie::new(db);

        let root_empty = trie.root_hash().unwrap();
        trie.insert(b"key", b"value").unwrap();
        let root_with_data = trie.root_hash().unwrap();
        assert_ne!(root_empty, root_with_data);
    }

    #[test]
    fn remove_key() {
        let db = Arc::new(MemoryDb::new());
        let mut trie = MerkleTrie::new(db);

        trie.insert(b"key", b"value").unwrap();
        assert!(trie.contains(b"key").unwrap());

        trie.remove(b"key").unwrap();
        assert!(!trie.contains(b"key").unwrap());
    }

    #[test]
    fn root_hash_deterministic() {
        let db1 = Arc::new(MemoryDb::new());
        let mut trie1 = MerkleTrie::new(db1);
        trie1.insert(b"a", b"1").unwrap();
        trie1.insert(b"b", b"2").unwrap();

        let db2 = Arc::new(MemoryDb::new());
        let mut trie2 = MerkleTrie::new(db2);
        trie2.insert(b"a", b"1").unwrap();
        trie2.insert(b"b", b"2").unwrap();

        assert_eq!(trie1.root_hash().unwrap(), trie2.root_hash().unwrap());
    }

    #[test]
    fn proof_generation_and_verification() {
        let db = Arc::new(MemoryDb::new());
        let mut trie = MerkleTrie::new(Arc::clone(&db));

        trie.insert(b"key1", b"value1").unwrap();
        trie.insert(b"key2", b"value2").unwrap();
        let root = trie.root_hash().unwrap();

        let proof = trie.get_proof(b"key1").unwrap();
        assert!(!proof.is_empty());

        let db2 = Arc::new(MemoryDb::new());
        let verified =
            MerkleTrie::<MemoryDb>::verify_proof(db2, &root, b"key1", proof).unwrap();
        assert_eq!(verified, Some(b"value1".to_vec()));
    }

    #[test]
    fn at_root_reopens_trie() {
        let db = Arc::new(MemoryDb::new());
        let mut trie = MerkleTrie::new(Arc::clone(&db));

        trie.insert(b"persist", b"data").unwrap();
        let root = trie.root_hash().unwrap();

        let trie2 = MerkleTrie::at_root(db, &root).unwrap();
        assert_eq!(trie2.get(b"persist").unwrap(), Some(b"data".to_vec()));
    }
}
