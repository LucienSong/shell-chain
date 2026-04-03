//! Bandwidth tracking and rate limiting for P2P connections.
//!
//! Provides a thread-safe [`BandwidthTracker`] that monitors inbound/outbound
//! byte rates per second. When configured limits are exceeded, `record_*`
//! methods return `false` so the caller can log warnings or shed load.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Snapshot of current bandwidth usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandwidthStats {
    /// Inbound bytes recorded in the current one-second window.
    pub inbound_bytes_per_sec: u64,
    /// Outbound bytes recorded in the current one-second window.
    pub outbound_bytes_per_sec: u64,
    /// Cumulative inbound bytes since tracker creation.
    pub total_inbound: u64,
    /// Cumulative outbound bytes since tracker creation.
    pub total_outbound: u64,
}

/// Thread-safe bandwidth tracker with per-second rate limiting.
///
/// Counters (`inbound_bytes` / `outbound_bytes`) accumulate within a
/// one-second window and are reset by [`reset_if_needed`]. A limit of
/// `0` means **unlimited** — `record_*` always returns `true`.
pub struct BandwidthTracker {
    inbound_bytes: Arc<AtomicU64>,
    outbound_bytes: Arc<AtomicU64>,
    total_inbound: Arc<AtomicU64>,
    total_outbound: Arc<AtomicU64>,
    max_inbound: u64,
    max_outbound: u64,
    last_reset: std::sync::Mutex<Instant>,
}

impl BandwidthTracker {
    /// Create a new tracker.
    ///
    /// `max_in` / `max_out` are bytes-per-second limits; pass `0` for unlimited.
    pub fn new(max_in: u64, max_out: u64) -> Self {
        Self {
            inbound_bytes: Arc::new(AtomicU64::new(0)),
            outbound_bytes: Arc::new(AtomicU64::new(0)),
            total_inbound: Arc::new(AtomicU64::new(0)),
            total_outbound: Arc::new(AtomicU64::new(0)),
            max_inbound: max_in,
            max_outbound: max_out,
            last_reset: std::sync::Mutex::new(Instant::now()),
        }
    }

    /// Record `bytes` of inbound traffic. Returns `false` if the configured
    /// per-second inbound limit would be exceeded (the bytes are still counted).
    pub fn record_inbound(&self, bytes: u64) -> bool {
        self.total_inbound.fetch_add(bytes, Ordering::Relaxed);
        let prev = self.inbound_bytes.fetch_add(bytes, Ordering::Relaxed);
        if self.max_inbound == 0 {
            return true;
        }
        prev + bytes <= self.max_inbound
    }

    /// Record `bytes` of outbound traffic. Returns `false` if the configured
    /// per-second outbound limit would be exceeded (the bytes are still counted).
    pub fn record_outbound(&self, bytes: u64) -> bool {
        self.total_outbound.fetch_add(bytes, Ordering::Relaxed);
        let prev = self.outbound_bytes.fetch_add(bytes, Ordering::Relaxed);
        if self.max_outbound == 0 {
            return true;
        }
        prev + bytes <= self.max_outbound
    }

    /// Reset per-second counters if at least one second has elapsed.
    pub fn reset_if_needed(&self) {
        let mut last = self.last_reset.lock().expect("lock poisoned");
        if last.elapsed() >= std::time::Duration::from_secs(1) {
            self.inbound_bytes.store(0, Ordering::Relaxed);
            self.outbound_bytes.store(0, Ordering::Relaxed);
            *last = Instant::now();
        }
    }

    /// Return a snapshot of current usage.
    pub fn stats(&self) -> BandwidthStats {
        BandwidthStats {
            inbound_bytes_per_sec: self.inbound_bytes.load(Ordering::Relaxed),
            outbound_bytes_per_sec: self.outbound_bytes.load(Ordering::Relaxed),
            total_inbound: self.total_inbound.load(Ordering::Relaxed),
            total_outbound: self.total_outbound.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_always_allows() {
        let tracker = BandwidthTracker::new(0, 0);
        assert!(tracker.record_inbound(u64::MAX / 2));
        assert!(tracker.record_outbound(u64::MAX / 2));
    }

    #[test]
    fn inbound_limit_triggers() {
        let tracker = BandwidthTracker::new(100, 0);
        assert!(tracker.record_inbound(50));
        assert!(tracker.record_inbound(50)); // exactly at limit
        assert!(!tracker.record_inbound(1)); // over limit
    }

    #[test]
    fn outbound_limit_triggers() {
        let tracker = BandwidthTracker::new(0, 200);
        assert!(tracker.record_outbound(200));
        assert!(!tracker.record_outbound(1));
    }

    #[test]
    fn reset_clears_window_counters() {
        let tracker = BandwidthTracker::new(100, 100);
        assert!(tracker.record_inbound(100));
        assert!(!tracker.record_inbound(1));

        // Force a reset by backdating last_reset.
        {
            let mut last = tracker.last_reset.lock().unwrap();
            *last = Instant::now() - std::time::Duration::from_secs(2);
        }
        tracker.reset_if_needed();

        // Window counters are zeroed; should be allowed again.
        assert!(tracker.record_inbound(100));
    }

    #[test]
    fn stats_reflect_current_usage() {
        let tracker = BandwidthTracker::new(0, 0);
        tracker.record_inbound(42);
        tracker.record_outbound(84);

        let s = tracker.stats();
        assert_eq!(s.inbound_bytes_per_sec, 42);
        assert_eq!(s.outbound_bytes_per_sec, 84);
        assert_eq!(s.total_inbound, 42);
        assert_eq!(s.total_outbound, 84);
    }

    #[test]
    fn total_counters_survive_reset() {
        let tracker = BandwidthTracker::new(100, 100);
        tracker.record_inbound(50);
        tracker.record_outbound(75);

        // Force reset.
        {
            let mut last = tracker.last_reset.lock().unwrap();
            *last = Instant::now() - std::time::Duration::from_secs(2);
        }
        tracker.reset_if_needed();

        tracker.record_inbound(10);
        tracker.record_outbound(20);

        let s = tracker.stats();
        assert_eq!(s.inbound_bytes_per_sec, 10);
        assert_eq!(s.outbound_bytes_per_sec, 20);
        assert_eq!(s.total_inbound, 60);
        assert_eq!(s.total_outbound, 95);
    }

    #[test]
    fn no_reset_within_one_second() {
        let tracker = BandwidthTracker::new(100, 100);
        tracker.record_inbound(80);
        tracker.reset_if_needed(); // less than 1s elapsed — should be a no-op
        let s = tracker.stats();
        assert_eq!(s.inbound_bytes_per_sec, 80);
    }

    #[test]
    fn default_config_bandwidth_unlimited() {
        use crate::config::NetworkConfig;
        let cfg = NetworkConfig::default();
        assert_eq!(cfg.max_inbound_bandwidth, 0);
        assert_eq!(cfg.max_outbound_bandwidth, 0);
    }
}
