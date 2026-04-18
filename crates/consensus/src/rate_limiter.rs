//! I3: Proof submission rate limiting.
//!
//! Prevents a malicious or buggy prover from flooding the network with proof
//! amendments or challenges. Uses a token-bucket model per `Address`.
//!
//! # Design
//!
//! - Each address starts with `initial_tokens` tokens.
//! - Tokens refill at `refill_rate` tokens per `refill_interval_secs` seconds.
//! - Each submission consumes one token.
//! - Submitters with zero tokens are denied until the next refill.
//! - Entries are garbage-collected after `gc_after_secs` seconds of inactivity.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use shell_primitives::Address;

/// Configuration for the proof submission rate limiter.
#[derive(Debug, Clone)]
pub struct RateLimiterConfig {
    /// Initial token count per address. Default: 10.
    pub initial_tokens: u64,
    /// Tokens added per refill. Default: 5.
    pub refill_rate: u64,
    /// Refill interval. Default: 60 seconds.
    pub refill_interval: Duration,
    /// Inactivity duration after which an entry is removed. Default: 600 seconds.
    pub gc_after: Duration,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            initial_tokens: 10,
            refill_rate: 5,
            refill_interval: Duration::from_secs(60),
            gc_after: Duration::from_secs(600),
        }
    }
}

/// Per-address token-bucket state.
#[derive(Debug)]
struct Bucket {
    tokens: u64,
    last_refill: Instant,
    last_used: Instant,
}

impl Bucket {
    fn new(initial_tokens: u64) -> Self {
        let now = Instant::now();
        Self {
            tokens: initial_tokens,
            last_refill: now,
            last_used: now,
        }
    }

    /// Refill tokens based on elapsed time, capped at `initial_tokens`.
    fn refill(&mut self, config: &RateLimiterConfig) {
        let elapsed = self.last_refill.elapsed();
        let interval_ms = config.refill_interval.as_millis().max(1);
        let elapsed_ms = elapsed.as_millis();
        if elapsed_ms >= interval_ms {
            let periods = elapsed_ms / interval_ms;
            let add = (periods as u64).saturating_mul(config.refill_rate);
            self.tokens = self.tokens.saturating_add(add).min(config.initial_tokens);
            self.last_refill = Instant::now();
        }
    }
}

/// I3: Token-bucket rate limiter for proof/challenge submissions.
///
/// `ProofRateLimiter` is `!Send` due to `Instant` and HashMap. Wrap in
/// `parking_lot::Mutex` when sharing across threads.
#[derive(Debug)]
pub struct ProofRateLimiter {
    config: RateLimiterConfig,
    buckets: HashMap<Address, Bucket>,
}

impl ProofRateLimiter {
    pub fn new(config: RateLimiterConfig) -> Self {
        Self {
            config,
            buckets: HashMap::new(),
        }
    }

    /// Try to consume one token for `address`.
    ///
    /// Returns `true` (allowed) if the address has tokens remaining.
    /// Returns `false` (denied) if the bucket is empty.
    pub fn try_consume(&mut self, address: &Address) -> bool {
        let config = &self.config;
        let bucket = self
            .buckets
            .entry(*address)
            .or_insert_with(|| Bucket::new(config.initial_tokens));

        bucket.refill(config);
        bucket.last_used = Instant::now();

        if bucket.tokens > 0 {
            bucket.tokens -= 1;
            true
        } else {
            false
        }
    }

    /// Return current token count for `address` (after refill).
    pub fn tokens_remaining(&mut self, address: &Address) -> u64 {
        let config = &self.config;
        let bucket = self
            .buckets
            .entry(*address)
            .or_insert_with(|| Bucket::new(config.initial_tokens));
        bucket.refill(config);
        bucket.tokens
    }

    /// Remove entries that have been inactive longer than `gc_after`.
    ///
    /// Should be called periodically (e.g., once per epoch) to bound memory.
    pub fn gc(&mut self) {
        let gc_after = self.config.gc_after;
        self.buckets.retain(|_, b| b.last_used.elapsed() < gc_after);
    }

    /// Number of tracked addresses.
    pub fn len(&self) -> usize {
        self.buckets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::Address;

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    fn fast_config() -> RateLimiterConfig {
        RateLimiterConfig {
            initial_tokens: 3,
            refill_rate: 3,
            refill_interval: Duration::from_millis(50),
            gc_after: Duration::from_secs(10),
        }
    }

    #[test]
    fn allows_up_to_initial_tokens() {
        let mut rl = ProofRateLimiter::new(fast_config());
        let a = addr(1);
        assert!(rl.try_consume(&a));
        assert!(rl.try_consume(&a));
        assert!(rl.try_consume(&a));
        // Fourth attempt should be denied.
        assert!(!rl.try_consume(&a));
    }

    #[test]
    fn refill_after_interval() {
        let mut rl = ProofRateLimiter::new(fast_config());
        let a = addr(2);
        // Drain bucket.
        for _ in 0..3 {
            rl.try_consume(&a);
        }
        assert!(!rl.try_consume(&a));
        // Wait for refill interval.
        std::thread::sleep(Duration::from_millis(60));
        // After refill, should be allowed again.
        assert!(rl.try_consume(&a));
    }

    #[test]
    fn different_addresses_are_independent() {
        let mut rl = ProofRateLimiter::new(fast_config());
        let a = addr(1);
        let b = addr(2);
        // Drain a.
        for _ in 0..3 {
            rl.try_consume(&a);
        }
        assert!(!rl.try_consume(&a));
        // b should still have tokens.
        assert!(rl.try_consume(&b));
    }

    #[test]
    fn tokens_remaining_returns_correct_count() {
        let mut rl = ProofRateLimiter::new(fast_config());
        let a = addr(3);
        assert_eq!(rl.tokens_remaining(&a), 3);
        rl.try_consume(&a);
        assert_eq!(rl.tokens_remaining(&a), 2);
        rl.try_consume(&a);
        assert_eq!(rl.tokens_remaining(&a), 1);
    }

    #[test]
    fn gc_removes_inactive_entries() {
        let mut config = fast_config();
        config.gc_after = Duration::from_millis(50);
        let mut rl = ProofRateLimiter::new(config);
        let a = addr(4);
        rl.try_consume(&a);
        assert_eq!(rl.len(), 1);
        std::thread::sleep(Duration::from_millis(60));
        rl.gc();
        assert_eq!(rl.len(), 0);
    }

    #[test]
    fn gc_keeps_active_entries() {
        let mut rl = ProofRateLimiter::new(fast_config());
        let a = addr(5);
        rl.try_consume(&a);
        // GC with full gc_after window — should keep the entry.
        rl.gc();
        assert_eq!(rl.len(), 1);
    }

    #[test]
    fn tokens_capped_at_initial_after_multiple_refills() {
        let mut rl = ProofRateLimiter::new(fast_config());
        let a = addr(6);
        // Use one token, then sleep through multiple refill intervals.
        rl.try_consume(&a);
        std::thread::sleep(Duration::from_millis(200));
        // After several refill periods, tokens should be capped at initial_tokens (3).
        assert_eq!(rl.tokens_remaining(&a), 3);
    }
}
