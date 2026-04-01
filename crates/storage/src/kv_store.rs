use crate::StorageError;

/// Operation in a write batch.
#[derive(Debug, Clone)]
pub enum WriteBatchOp {
    Put { key: Vec<u8>, value: Vec<u8> },
    Delete { key: Vec<u8> },
}

/// Batch of write operations.
///
/// Atomicity guarantees depend on the backend implementation:
/// - `RocksDbStore`: fully atomic (all-or-nothing via RocksDB WriteBatch)
/// - `MemoryDb`: best-effort under write lock; not rollback-safe on panic
#[derive(Debug, Clone, Default)]
pub struct WriteBatch {
    ops: Vec<WriteBatchOp>,
}

impl WriteBatch {
    pub fn new() -> Self {
        Self { ops: Vec::new() }
    }

    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.ops.push(WriteBatchOp::Put { key, value });
    }

    pub fn delete(&mut self, key: Vec<u8>) {
        self.ops.push(WriteBatchOp::Delete { key });
    }

    pub fn ops(&self) -> &[WriteBatchOp] {
        &self.ops
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Low-level key-value store trait.
///
/// Each implementation represents a single logical namespace
/// (e.g., one RocksDB column family). This design keeps the trait
/// compatible with `eth_trie::DB` and allows typed stores to compose
/// multiple `KvStore` instances for different data domains.
pub trait KvStore: Send + Sync {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError>;
    fn delete(&self, key: &[u8]) -> Result<(), StorageError>;
    fn flush(&self) -> Result<(), StorageError>;
    fn write_batch(&self, batch: WriteBatch) -> Result<(), StorageError>;

    /// Check if a key exists without reading the full value.
    fn contains(&self, key: &[u8]) -> Result<bool, StorageError> {
        Ok(self.get(key)?.is_some())
    }
}
