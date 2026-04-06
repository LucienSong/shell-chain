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
    /// per-second inbound limit would be exceeded (bytes NOT counted when over limit).
    pub fn record_inbound(&self, bytes: u64) -> bool {
        // Saturating add for total counter to prevent wrap-around (F-058).
        let _ = self
            .total_inbound
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_add(bytes))
            });
        if self.max_inbound == 0 {
            self.inbound_bytes.fetch_add(bytes, Ordering::Relaxed);
            return true;
        }
        // CAS loop: atomically check-and-increment to prevent race (F-056).
        self.inbound_bytes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                let new = current.saturating_add(bytes);
                if new <= self.max_inbound {
                    Some(new)
                } else {
                    None // over limit — reject
                }
            })
            .is_ok()
    }

    /// Record `bytes` of outbound traffic. Returns `false` if the configured
    /// per-second outbound limit would be exceeded (bytes NOT counted when over limit).
    pub fn record_outbound(&self, bytes: u64) -> bool {
        let _ = self
            .total_outbound
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_add(bytes))
            });
        if self.max_outbound == 0 {
            self.outbound_bytes.fetch_add(bytes, Ordering::Relaxed);
            return true;
        }
        // CAS loop: atomically check-and-increment to prevent race (F-056).
        self.outbound_bytes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                let new = current.saturating_add(bytes);
                if new <= self.max_outbound {
                    Some(new)
                } else {
                    None
                }
            })
            .is_ok()
    }

    /// Reset per-second counters if at least one second has elapsed.
    pub fn reset_if_needed(&self) {
        let mut last = self.last_reset.lock().expect("lock poisoned");
        if last.elapsed() >= std::time::Duration::from_secs(1) {
            // Use SeqCst to synchronize with concurrent record operations (F-057).
            self.inbound_bytes.store(0, Ordering::SeqCst);
            self.outbound_bytes.store(0, Ordering::SeqCst);
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
    fn rejected_bytes_not_counted_in_window() {
        let tracker = BandwidthTracker::new(100, 0);
        assert!(tracker.record_inbound(100));
        assert!(!tracker.record_inbound(50)); // rejected
                                              // Window counter stays at 100, not 150
        assert_eq!(tracker.stats().inbound_bytes_per_sec, 100);
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
    fn total_counters_saturate_not_wrap() {
        let tracker = BandwidthTracker::new(0, 0);
        // Pre-fill near max
        tracker
            .total_inbound
            .store(u64::MAX - 10, Ordering::Relaxed);
        tracker.record_inbound(100);
        assert_eq!(tracker.stats().total_inbound, u64::MAX);
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
