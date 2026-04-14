//! Filter registry for `eth_newFilter`, `eth_newBlockFilter`, `eth_getFilterChanges`,
//! `eth_getFilterLogs`, and `eth_uninstallFilter` RPCs.
//!
//! Filters are poll-based: clients install a filter, then periodically call
//! `eth_getFilterChanges` to drain accumulated results since the last poll.
//! Filters expire after a configurable TTL (default 5 minutes) of inactivity.

use std::collections::HashMap;
use std::time::Instant;

use parking_lot::RwLock;
use rand::Rng;
use shell_primitives::ShellHash;

use crate::filter::RawLogFilter;
use crate::types::RpcLogWithMeta;

/// Default time-to-live for idle filters (5 minutes).
const DEFAULT_TTL_SECS: u64 = 300;

/// Maximum number of concurrent filters to prevent resource exhaustion.
const MAX_FILTERS: usize = 1024;

/// Types of filters that can be registered.
pub enum FilterKind {
    /// Log filter with criteria (from `eth_newFilter`).
    Log(RawLogFilter),
    /// Block filter — tracks new block hashes (from `eth_newBlockFilter`).
    Block,
}

/// Result returned by `get_filter_changes`.
pub enum FilterChanges {
    /// Accumulated log entries since last poll.
    Logs(Vec<RpcLogWithMeta>),
    /// Accumulated block hashes since last poll.
    BlockHashes(Vec<ShellHash>),
}

/// Internal state for a single registered filter.
pub struct FilterEntry {
    pub kind: FilterKind,
    /// Block number at filter creation (or last successful poll).
    pub last_poll_block: u64,
    /// Last access time for TTL-based expiry.
    pub last_access: Instant,
}

/// Thread-safe registry of active filters.
///
/// Filter IDs are cryptographically random hex strings to prevent enumeration.
pub struct FilterRegistry {
    filters: RwLock<HashMap<String, FilterEntry>>,
    ttl_secs: u64,
}

impl Default for FilterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterRegistry {
    /// Create a new registry with the default TTL.
    pub fn new() -> Self {
        Self {
            filters: RwLock::new(HashMap::new()),
            ttl_secs: DEFAULT_TTL_SECS,
        }
    }

    /// Create a new registry with a custom TTL (seconds).
    #[cfg(test)]
    pub fn with_ttl(ttl_secs: u64) -> Self {
        Self {
            filters: RwLock::new(HashMap::new()),
            ttl_secs,
        }
    }

    /// Install a new filter and return its hex-encoded ID.
    /// Returns `None` if the maximum filter count has been reached.
    pub fn new_filter(&self, kind: FilterKind, current_block: u64) -> Option<String> {
        let mut filters = self.filters.write();
        if filters.len() >= MAX_FILTERS {
            return None;
        }
        // F-126: generate a random 128-bit hex ID to prevent enumeration.
        let id = format!("0x{:032x}", rand::rng().random::<u128>());
        let entry = FilterEntry {
            kind,
            last_poll_block: current_block,
            last_access: Instant::now(),
        };
        filters.insert(id.clone(), entry);
        Some(id)
    }

    /// Get the filter kind and last poll block for a given filter ID.
    /// Returns `None` if the filter does not exist.
    /// Updates the last access timestamp.
    pub fn get_filter_info(&self, id: &str) -> Option<(bool, u64)> {
        let mut filters = self.filters.write();
        let entry = filters.get_mut(id)?;
        entry.last_access = Instant::now();
        let is_log = matches!(entry.kind, FilterKind::Log(_));
        Some((is_log, entry.last_poll_block))
    }

    /// Update the last_poll_block for a filter after a successful poll.
    pub fn update_last_poll(&self, id: &str, new_block: u64) {
        let mut filters = self.filters.write();
        if let Some(entry) = filters.get_mut(id) {
            entry.last_poll_block = new_block;
            entry.last_access = Instant::now();
        }
    }

    /// Check if a filter exists and whether it is a log filter.
    /// Returns the stored `RawLogFilter` for log filters (cloned).
    pub fn get_log_filter(&self, id: &str) -> Option<RawLogFilter> {
        let mut filters = self.filters.write();
        let entry = filters.get_mut(id)?;
        entry.last_access = Instant::now();
        match &entry.kind {
            FilterKind::Log(raw) => Some(raw.clone()),
            _ => None,
        }
    }

    /// Remove a filter. Returns `true` if it existed.
    pub fn uninstall(&self, id: &str) -> bool {
        self.filters.write().remove(id).is_some()
    }

    /// Remove filters that have not been accessed within the TTL window.
    pub fn cleanup_expired(&self) {
        let cutoff = Instant::now() - std::time::Duration::from_secs(self.ttl_secs);
        self.filters
            .write()
            .retain(|_, entry| entry.last_access > cutoff);
    }

    /// Returns the number of active filters.
    pub fn len(&self) -> usize {
        self.filters.read().len()
    }

    /// Returns true if there are no active filters.
    pub fn is_empty(&self) -> bool {
        self.filters.read().is_empty()
    }

    /// Spawn a periodic cleanup task. Call this once at startup.
    pub fn start_cleanup(registry: std::sync::Arc<Self>) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                let before = registry.len();
                registry.cleanup_expired();
                let after = registry.len();
                if before > after {
                    tracing::debug!(
                        removed = before - after,
                        remaining = after,
                        "cleaned up expired filters"
                    );
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_filter_returns_hex_id() {
        let reg = FilterRegistry::new();
        let id = reg.new_filter(FilterKind::Block, 0).unwrap();
        assert!(id.starts_with("0x"));
    }

    #[test]
    fn new_filter_respects_max_limit() {
        let reg = FilterRegistry::new();
        for _ in 0..MAX_FILTERS {
            assert!(reg.new_filter(FilterKind::Block, 0).is_some());
        }
        // One more should fail
        assert!(reg.new_filter(FilterKind::Block, 0).is_none());
    }

    #[test]
    fn filter_ids_are_random_and_unique() {
        let reg = FilterRegistry::new();
        let id1 = reg.new_filter(FilterKind::Block, 0).unwrap();
        let id2 = reg.new_filter(FilterKind::Block, 0).unwrap();
        assert!(id1.starts_with("0x"));
        assert!(id2.starts_with("0x"));
        assert_ne!(id1, id2);
        // Random IDs should be long hex strings (0x + 32 hex chars).
        assert!(id1.len() >= 34);
    }

    #[test]
    fn uninstall_existing_filter() {
        let reg = FilterRegistry::new();
        let id = reg.new_filter(FilterKind::Block, 10).unwrap();
        assert!(reg.uninstall(&id));
        assert!(!reg.uninstall(&id)); // already removed
    }

    #[test]
    fn get_filter_info_returns_correct_data() {
        let reg = FilterRegistry::new();
        let id = reg.new_filter(FilterKind::Block, 42).unwrap();
        let (is_log, last_poll) = reg.get_filter_info(&id).unwrap();
        assert!(!is_log);
        assert_eq!(last_poll, 42);
    }

    #[test]
    fn get_filter_info_for_log_filter() {
        let raw: RawLogFilter = serde_json::from_str(r#"{}"#).unwrap();
        let reg = FilterRegistry::new();
        let id = reg.new_filter(FilterKind::Log(raw), 5).unwrap();
        let (is_log, last_poll) = reg.get_filter_info(&id).unwrap();
        assert!(is_log);
        assert_eq!(last_poll, 5);
    }

    #[test]
    fn get_filter_info_nonexistent() {
        let reg = FilterRegistry::new();
        assert!(reg.get_filter_info("0x999").is_none());
    }

    #[test]
    fn update_last_poll_advances_block() {
        let reg = FilterRegistry::new();
        let id = reg.new_filter(FilterKind::Block, 0).unwrap();
        reg.update_last_poll(&id, 50);
        let (_, last_poll) = reg.get_filter_info(&id).unwrap();
        assert_eq!(last_poll, 50);
    }

    #[test]
    fn get_log_filter_returns_raw_for_log_kind() {
        let raw: RawLogFilter =
            serde_json::from_str(r#"{"fromBlock":"0x1","toBlock":"0x5"}"#).unwrap();
        let reg = FilterRegistry::new();
        let id = reg.new_filter(FilterKind::Log(raw), 1).unwrap();
        let retrieved = reg.get_log_filter(&id);
        assert!(retrieved.is_some());
    }

    #[test]
    fn get_log_filter_returns_none_for_block_kind() {
        let reg = FilterRegistry::new();
        let id = reg.new_filter(FilterKind::Block, 0).unwrap();
        assert!(reg.get_log_filter(&id).is_none());
    }

    #[test]
    fn cleanup_removes_expired_filters() {
        let reg = FilterRegistry::with_ttl(0); // expire immediately
        let id = reg.new_filter(FilterKind::Block, 0).unwrap();
        // Sleep a tiny bit to ensure the filter is past the TTL
        std::thread::sleep(std::time::Duration::from_millis(10));
        reg.cleanup_expired();
        assert_eq!(reg.len(), 0);
        assert!(!reg.uninstall(&id));
    }

    #[test]
    fn cleanup_keeps_fresh_filters() {
        let reg = FilterRegistry::new(); // default 300s TTL
        let _id = reg.new_filter(FilterKind::Block, 0).unwrap();
        reg.cleanup_expired();
        assert_eq!(reg.len(), 1);
    }
}
