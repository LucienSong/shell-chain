use std::sync::Arc;

use crate::{KvStore, StorageError};

/// Adapter that bridges [`KvStore`] to [`eth_trie::DB`].
///
/// This allows any `KvStore` implementation (e.g., `MemoryDb`, future `RocksColumn`)
/// to serve as the backing store for an Ethereum-compatible Merkle Patricia Trie.
pub struct KvStoreTrieDb<S> {
    inner: Arc<S>,
}

impl<S> KvStoreTrieDb<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { inner: store }
    }
}

impl<S: KvStore> eth_trie::DB for KvStoreTrieDb<S> {
    type Error = StorageError;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        self.inner.get(key)
    }

    fn insert(&self, key: &[u8], value: Vec<u8>) -> Result<(), Self::Error> {
        self.inner.put(key, &value)
    }

    fn remove(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.inner.delete(key)
    }

    fn flush(&self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}
