//! J3: Aggregation scheduler — decides when to trigger L2 recursive aggregation.
//!
//! The `AggregationScheduler` monitors the stream of completed L1 proofs and
//! emits `AggregationTrigger` events when enough proofs have accumulated to
//! justify an L2 aggregation round.
//!
//! ## Trigger conditions (any one suffices)
//!
//! 1. **Interval trigger**: every `trigger_block_interval` blocks.
//! 2. **Proof count trigger**: at least `min_l1_proofs_for_l2` L1 proofs are
//!    available for a contiguous window.
//! 3. **Epoch boundary**: when `epoch_length > 0` and the current block is the
//!    first block of a new epoch.
//!
//! The scheduler does **not** run proving itself — it only decides *when* to
//! aggregate. The `ProverService` (G2) is responsible for actually executing
//! the aggregation job.

use crate::recursive_air::AggregationJob;

// ── AggregationConfig ─────────────────────────────────────────────────────────

/// Configuration for the aggregation scheduler.
#[derive(Debug, Clone)]
pub struct AggregationConfig {
    /// Number of blocks per epoch. 0 disables epoch-boundary triggering.
    pub epoch_length: u64,
    /// Minimum number of L1 proofs required to trigger an L2 aggregation round.
    /// Must be ≥ 2 (aggregating a single proof is a no-op).
    pub min_l1_proofs_for_l2: u64,
    /// Trigger aggregation every N blocks regardless of proof count.
    /// 0 disables interval triggering.
    pub trigger_block_interval: u64,
}

impl Default for AggregationConfig {
    fn default() -> Self {
        Self {
            epoch_length: 100,
            min_l1_proofs_for_l2: 8,
            trigger_block_interval: 50,
        }
    }
}

// ── AggregationTrigger ────────────────────────────────────────────────────────

/// The reason an aggregation round was triggered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerReason {
    /// Triggered because `min_l1_proofs_for_l2` was reached.
    ProofThreshold,
    /// Triggered by `trigger_block_interval` cadence.
    BlockInterval,
    /// Triggered at an epoch boundary.
    EpochBoundary,
}

/// Emitted by [`AggregationScheduler::on_block`] when an aggregation should run.
#[derive(Debug, Clone)]
pub struct AggregationTrigger {
    /// The block number that triggered aggregation.
    pub at_block: u64,
    /// The aggregation window to process.
    pub job: AggregationJob,
    /// Why aggregation was triggered.
    pub reason: TriggerReason,
}

// ── AggregationScheduler ──────────────────────────────────────────────────────

/// Stateful scheduler that tracks pending L1 proofs and decides when to
/// trigger L2 aggregation.
///
/// Call [`on_proof`] each time a new L1 proof becomes available.
/// Call [`on_block`] each time a new block is sealed.
/// Aggregation triggers are returned from [`on_block`].
#[derive(Debug)]
pub struct AggregationScheduler {
    config: AggregationConfig,
    /// Block number of the start of the current pending aggregation window.
    window_start: u64,
    /// Count of L1 proofs accumulated since `window_start`.
    pending_proof_count: u64,
    /// Block number when the last aggregation was triggered.
    last_trigger_block: u64,
}

impl AggregationScheduler {
    /// Create a new scheduler, anchored at `genesis_block` (usually 0).
    pub fn new(config: AggregationConfig, genesis_block: u64) -> Self {
        Self {
            config,
            window_start: genesis_block,
            pending_proof_count: 0,
            last_trigger_block: genesis_block,
        }
    }

    /// Notify the scheduler that a new L1 proof for `block_number` is available.
    pub fn on_proof(&mut self, _block_number: u64) {
        self.pending_proof_count += 1;
    }

    /// Advance the scheduler to `block_number`.
    ///
    /// Returns `Some(AggregationTrigger)` if aggregation should start now,
    /// or `None` if no trigger condition is met.
    pub fn on_block(&mut self, block_number: u64) -> Option<AggregationTrigger> {
        let reason = self.check_triggers(block_number)?;

        let job = AggregationJob::new(self.window_start, block_number);
        let trigger = AggregationTrigger {
            at_block: block_number,
            job,
            reason,
        };

        // Reset window.
        self.window_start = block_number + 1;
        self.pending_proof_count = 0;
        self.last_trigger_block = block_number;

        Some(trigger)
    }

    /// Returns the number of L1 proofs accumulated in the current window.
    pub fn pending_proof_count(&self) -> u64 {
        self.pending_proof_count
    }

    /// Returns the block number where the current window started.
    pub fn window_start(&self) -> u64 {
        self.window_start
    }

    fn check_triggers(&self, block_number: u64) -> Option<TriggerReason> {
        // 1. Proof threshold.
        if self.pending_proof_count >= self.config.min_l1_proofs_for_l2 {
            return Some(TriggerReason::ProofThreshold);
        }

        // 2. Block interval.
        if self.config.trigger_block_interval > 0 {
            let since_last = block_number.saturating_sub(self.last_trigger_block);
            if since_last >= self.config.trigger_block_interval {
                return Some(TriggerReason::BlockInterval);
            }
        }

        // 3. Epoch boundary.
        if self.config.epoch_length > 0
            && block_number > 0
            && block_number.is_multiple_of(self.config.epoch_length)
        {
            return Some(TriggerReason::EpochBoundary);
        }

        None
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_scheduler() -> AggregationScheduler {
        AggregationScheduler::new(AggregationConfig::default(), 0)
    }

    #[test]
    fn no_trigger_when_few_proofs_and_early_block() {
        let mut sched = default_scheduler();
        sched.on_proof(1);
        sched.on_proof(2);
        // Only 2 proofs, block 5 — none of the triggers fire.
        assert!(sched.on_block(5).is_none());
    }

    #[test]
    fn proof_threshold_triggers_aggregation() {
        let mut sched = default_scheduler(); // min_l1 = 8
        for i in 1u64..=8 {
            sched.on_proof(i);
        }
        let trigger = sched
            .on_block(10)
            .expect("should trigger on proof threshold");
        assert_eq!(trigger.reason, TriggerReason::ProofThreshold);
        assert_eq!(trigger.at_block, 10);
    }

    #[test]
    fn block_interval_triggers_aggregation() {
        let mut sched = default_scheduler(); // interval = 50
                                             // No proofs — block 50 hits interval.
        let trigger = sched
            .on_block(50)
            .expect("should trigger on block interval");
        assert_eq!(trigger.reason, TriggerReason::BlockInterval);
    }

    #[test]
    fn epoch_boundary_triggers_aggregation() {
        // Disable interval so only the epoch boundary fires.
        let config = AggregationConfig {
            epoch_length: 100,
            min_l1_proofs_for_l2: 8,
            trigger_block_interval: 0,
        };
        let mut sched = AggregationScheduler::new(config, 0);
        // Block 100 is epoch boundary.
        let trigger = sched
            .on_block(100)
            .expect("should trigger on epoch boundary");
        assert_eq!(trigger.reason, TriggerReason::EpochBoundary);
    }

    #[test]
    fn proof_threshold_takes_priority_over_interval() {
        let mut sched = default_scheduler();
        for i in 1u64..=8 {
            sched.on_proof(i);
        }
        // Block 50 would also trigger interval, but proof threshold is checked first.
        let trigger = sched.on_block(50).expect("trigger");
        assert_eq!(trigger.reason, TriggerReason::ProofThreshold);
    }

    #[test]
    fn window_resets_after_trigger() {
        let mut sched = default_scheduler();
        for i in 1u64..=8 {
            sched.on_proof(i);
        }
        sched.on_block(10); // triggers, resets window
        assert_eq!(sched.pending_proof_count(), 0);
        assert_eq!(sched.window_start(), 11);
    }

    #[test]
    fn trigger_job_covers_correct_range() {
        let mut sched = AggregationScheduler::new(AggregationConfig::default(), 0);
        for _ in 0..8 {
            sched.on_proof(1);
        }
        let trigger = sched.on_block(15).unwrap();
        assert_eq!(trigger.job.start_block, 0);
        assert_eq!(trigger.job.end_block, 15);
    }

    #[test]
    fn no_epoch_trigger_at_zero() {
        // Block 0 should not trigger epoch boundary (genesis block).
        let mut sched = default_scheduler();
        assert!(sched.on_block(0).is_none());
    }

    #[test]
    fn disabled_interval_does_not_trigger() {
        let config = AggregationConfig {
            trigger_block_interval: 0, // disabled
            min_l1_proofs_for_l2: 100,
            epoch_length: 0,
        };
        let mut sched = AggregationScheduler::new(config, 0);
        // No triggers should fire for many blocks.
        for b in 1u64..=200 {
            assert!(
                sched.on_block(b).is_none(),
                "unexpected trigger at block {b}"
            );
        }
    }

    #[test]
    fn pending_proof_count_accessor() {
        let mut sched = default_scheduler();
        assert_eq!(sched.pending_proof_count(), 0);
        sched.on_proof(1);
        sched.on_proof(2);
        assert_eq!(sched.pending_proof_count(), 2);
    }

    #[test]
    fn aggregation_config_default_values() {
        let cfg = AggregationConfig::default();
        assert_eq!(cfg.epoch_length, 100);
        assert_eq!(cfg.min_l1_proofs_for_l2, 8);
        assert_eq!(cfg.trigger_block_interval, 50);
    }
}
