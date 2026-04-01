//! RocksDB-backed implementation of [`KvStore`].
//!
//! [`RocksDbStore`] wraps a single RocksDB column family, exposing it through
//! the [`KvStore`] trait. Multiple `RocksDbStore` instances can share the same
//! underlying `rocksdb::DB` via `Arc`, each targeting a different column family.
//!
//! # Column Families
//!
//! Shell-chain uses 4 column families:
//! - **`state`**: account trie nodes (WorldState)
//! - **`chain`**: block headers, bodies, canonical index (ChainStore)
//! - **`receipts`**: transaction receipts
//! - **`index`**: secondary indexes (tx-hash → block, etc.)
//!
//! # Usage
//!
//! ```ignore
//! use shell_storage::{RocksDbStore, RocksDbConfig};
//!
//! let stores = RocksDbStore::open_all("/tmp/shell-chain-db", None)?;
//! let state_store = &stores.state;
//! let chain_store = &stores.chain;
//! ```

use std::path::Path;
use std::sync::Arc;

use rocksdb::{
    BlockBasedOptions, BoundColumnFamily, Cache, ColumnFamilyDescriptor,
    DBCompactionStyle, DBWithThreadMode, MultiThreaded, Options, WriteBatch as RocksWriteBatch,
};

use crate::{KvStore, StorageError, WriteBatch, WriteBatchOp};

/// Column family names used by shell-chain.
pub const CF_STATE: &str = "state";
pub const CF_CHAIN: &str = "chain";
pub const CF_RECEIPTS: &str = "receipts";
pub const CF_INDEX: &str = "index";

/// All column family names.
const ALL_CFS: &[&str] = &[CF_STATE, CF_CHAIN, CF_RECEIPTS, CF_INDEX];

type RocksDb = DBWithThreadMode<MultiThreaded>;

/// Tuning configuration for the RocksDB engine.
///
/// Pass to [`RocksDbStore::open_all`] to override defaults. All fields have
/// sensible defaults via [`RocksDbConfig::default()`] that are suitable for
/// development and light workloads. For production nodes, tune based on
/// available RAM and disk characteristics.
///
/// # Example
///
/// ```ignore
/// let cfg = RocksDbConfig {
///     block_cache_mb: 256,
///     write_buffer_mb: 128,
///     ..Default::default()
/// };
/// let stores = RocksDbStore::open_all("/data/shell-chain", Some(cfg))?;
/// ```
#[derive(Debug, Clone)]
pub struct RocksDbConfig {
    /// LRU block cache size in megabytes. Shared across all column families.
    /// Higher values reduce disk reads for hot data.
    pub block_cache_mb: usize,
    /// Write buffer (memtable) size per column family in megabytes.
    pub write_buffer_mb: usize,
    /// Maximum number of write buffers per column family before stalling.
    pub max_write_buffers: i32,
    /// RocksDB compaction style. `Level` is best for most blockchain workloads.
    pub compaction_style: RocksCompactionStyle,
}

/// Compaction strategy selection (mirrors `rocksdb::DBCompactionStyle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RocksCompactionStyle {
    Level,
    Universal,
    Fifo,
}

impl Default for RocksDbConfig {
    fn default() -> Self {
        Self {
            block_cache_mb: 128,
            write_buffer_mb: 64,
            max_write_buffers: 3,
            compaction_style: RocksCompactionStyle::Level,
        }
    }
}

/// RocksDB-backed KvStore targeting a single column family.
///
/// Multiple instances can share the same `Arc<RocksDb>`, each operating
/// on a different column family. All operations are thread-safe.
#[derive(Clone)]
pub struct RocksDbStore {
    db: Arc<RocksDb>,
    cf_name: &'static str,
}

/// Collection of all RocksDB column family stores.
///
/// Returned by [`RocksDbStore::open_all`]. Each field is a `RocksDbStore`
/// targeting its respective column family, sharing the same underlying DB.
pub struct RocksDbStores {
    pub state: RocksDbStore,
    pub chain: RocksDbStore,
    pub receipts: RocksDbStore,
    pub index: RocksDbStore,
}

impl RocksDbStore {
    /// Open a RocksDB database at the given path with all shell-chain column families.
    ///
    /// Pass `None` for config to use [`RocksDbConfig::default()`].
    /// Creates the database and column families if they don't exist.
    /// Returns a [`RocksDbStores`] struct with one `RocksDbStore` per column family.
    pub fn open_all<P: AsRef<Path>>(
        path: P,
        config: Option<RocksDbConfig>,
    ) -> Result<RocksDbStores, StorageError> {
        let cfg = config.unwrap_or_default();

        // Shared block cache across all CFs
        let cache = Cache::new_lru_cache(cfg.block_cache_mb * 1024 * 1024);
        let mut table_opts = BlockBasedOptions::default();
        table_opts.set_block_cache(&cache);

        let mut db_opts = Options::default();
        db_opts.create_if_missing(true);
        db_opts.create_missing_column_families(true);

        // Build per-CF options with tuning parameters
        let make_cf_opts = || {
            let mut opts = Options::default();
            opts.set_write_buffer_size(cfg.write_buffer_mb * 1024 * 1024);
            opts.set_max_write_buffer_number(cfg.max_write_buffers);
            opts.set_compaction_style(match cfg.compaction_style {
                RocksCompactionStyle::Level => DBCompactionStyle::Level,
                RocksCompactionStyle::Universal => DBCompactionStyle::Universal,
                RocksCompactionStyle::Fifo => DBCompactionStyle::Fifo,
            });
            opts.set_block_based_table_factory(&table_opts);
            opts
        };

        let cf_descriptors: Vec<ColumnFamilyDescriptor> = ALL_CFS
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, make_cf_opts()))
            .collect();

        let db = RocksDb::open_cf_descriptors(&db_opts, path, cf_descriptors)
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let db = Arc::new(db);

        Ok(RocksDbStores {
            state: RocksDbStore { db: db.clone(), cf_name: CF_STATE },
            chain: RocksDbStore { db: db.clone(), cf_name: CF_CHAIN },
            receipts: RocksDbStore { db: db.clone(), cf_name: CF_RECEIPTS },
            index: RocksDbStore { db, cf_name: CF_INDEX },
        })
    }

    /// Get a reference to the column family handle.
    fn cf(&self) -> Arc<BoundColumnFamily<'_>> {
        self.db
            .cf_handle(self.cf_name)
            .expect("column family must exist — opened via open_all")
    }
}

impl KvStore for RocksDbStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        self.db
            .get_cf(&self.cf(), key)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.db
            .put_cf(&self.cf(), key, value)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        self.db
            .delete_cf(&self.cf(), key)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn flush(&self) -> Result<(), StorageError> {
        self.db
            .flush_cf(&self.cf())
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn write_batch(&self, batch: WriteBatch) -> Result<(), StorageError> {
        let cf = self.cf();
        let mut rocks_batch = RocksWriteBatch::default();
        for op in batch.ops() {
            match op {
                WriteBatchOp::Put { key, value } => {
                    rocks_batch.put_cf(&cf, key, value);
                }
                WriteBatchOp::Delete { key } => {
                    rocks_batch.delete_cf(&cf, key);
                }
            }
        }
        self.db
            .write(rocks_batch)
            .map_err(|e| StorageError::Database(e.to_string()))
    }

    fn contains(&self, key: &[u8]) -> Result<bool, StorageError> {
        self.db
            .get_pinned_cf(&self.cf(), key)
            .map(|v| v.is_some())
            .map_err(|e| StorageError::Database(e.to_string()))
    }
}

impl std::fmt::Debug for RocksDbStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RocksDbStore")
            .field("cf_name", &self.cf_name)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_temp() -> (tempfile::TempDir, RocksDbStores) {
        let dir = tempfile::tempdir().unwrap();
        let stores = RocksDbStore::open_all(dir.path(), None).unwrap();
        (dir, stores)
    }

    #[test]
    fn open_and_close() {
        let (_dir, _stores) = open_temp();
        // Database opens and drops without error.
    }

    #[test]
    fn put_get_delete() {
        let (_dir, stores) = open_temp();
        let s = &stores.state;

        assert_eq!(s.get(b"k1").unwrap(), None);

        s.put(b"k1", b"v1").unwrap();
        assert_eq!(s.get(b"k1").unwrap(), Some(b"v1".to_vec()));

        s.delete(b"k1").unwrap();
        assert_eq!(s.get(b"k1").unwrap(), None);
    }

    #[test]
    fn contains_check() {
        let (_dir, stores) = open_temp();
        let s = &stores.chain;

        assert!(!s.contains(b"missing").unwrap());
        s.put(b"present", b"yes").unwrap();
        assert!(s.contains(b"present").unwrap());
    }

    #[test]
    fn write_batch_atomic() {
        let (_dir, stores) = open_temp();
        let s = &stores.receipts;

        s.put(b"to_delete", b"old").unwrap();

        let mut batch = WriteBatch::new();
        batch.put(b"a".to_vec(), b"1".to_vec());
        batch.put(b"b".to_vec(), b"2".to_vec());
        batch.delete(b"to_delete".to_vec());

        s.write_batch(batch).unwrap();

        assert_eq!(s.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(s.get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(s.get(b"to_delete").unwrap(), None);
    }

    #[test]
    fn column_families_are_isolated() {
        let (_dir, stores) = open_temp();

        stores.state.put(b"key", b"state_val").unwrap();
        stores.chain.put(b"key", b"chain_val").unwrap();

        assert_eq!(stores.state.get(b"key").unwrap(), Some(b"state_val".to_vec()));
        assert_eq!(stores.chain.get(b"key").unwrap(), Some(b"chain_val".to_vec()));
        assert_eq!(stores.receipts.get(b"key").unwrap(), None);
        assert_eq!(stores.index.get(b"key").unwrap(), None);
    }

    #[test]
    fn flush_succeeds() {
        let (_dir, stores) = open_temp();
        stores.state.put(b"k", b"v").unwrap();
        stores.state.flush().unwrap();
        assert_eq!(stores.state.get(b"k").unwrap(), Some(b"v".to_vec()));
    }

    #[test]
    fn reopen_persists_data() {
        let dir = tempfile::tempdir().unwrap();

        // Open, write, close
        {
            let stores = RocksDbStore::open_all(dir.path(), None).unwrap();
            stores.state.put(b"persist", b"value").unwrap();
            stores.chain.put(b"block", b"data").unwrap();
        }

        // Reopen and verify
        {
            let stores = RocksDbStore::open_all(dir.path(), None).unwrap();
            assert_eq!(stores.state.get(b"persist").unwrap(), Some(b"value".to_vec()));
            assert_eq!(stores.chain.get(b"block").unwrap(), Some(b"data".to_vec()));
        }
    }

    #[test]
    fn large_value_roundtrip() {
        let (_dir, stores) = open_temp();
        let s = &stores.state;

        // Simulate a Dilithium3 public key (~1952 bytes)
        let large_val = vec![0xABu8; 1952];
        s.put(b"pq_pubkey", &large_val).unwrap();
        assert_eq!(s.get(b"pq_pubkey").unwrap(), Some(large_val));
    }

    #[test]
    fn empty_batch_is_noop() {
        let (_dir, stores) = open_temp();
        let batch = WriteBatch::new();
        stores.state.write_batch(batch).unwrap();
    }

    #[test]
    fn custom_config_opens_successfully() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = RocksDbConfig {
            block_cache_mb: 16,
            write_buffer_mb: 8,
            max_write_buffers: 2,
            compaction_style: RocksCompactionStyle::Universal,
        };
        let stores = RocksDbStore::open_all(dir.path(), Some(cfg)).unwrap();
        stores.state.put(b"k", b"v").unwrap();
        assert_eq!(stores.state.get(b"k").unwrap(), Some(b"v".to_vec()));
    }
}
