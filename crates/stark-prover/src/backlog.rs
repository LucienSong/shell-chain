//! Async proof backlog — decouples block production from STARK proving.
//!
//! [`ProofBacklog`] holds a queue of [`ProofTask`]s awaiting async proving.
//! A high-watermark threshold signals when the backlog is growing faster than
//! the prover can drain it, enabling the system to shed non-critical work or
//! activate additional prover capacity.

use std::collections::VecDeque;

use crate::prover::SigBatchEntry;

// ── ProofTask ─────────────────────────────────────────────────────────────────

/// A single unit of work for the async prover: one block worth of signatures.
#[derive(Debug, Clone)]
pub struct ProofTask {
    /// The block hash identifying which block this task covers.
    pub block_hash: [u8; 32],
    /// The block number (used for ordered range scans and priority).
    pub block_number: u64,
    /// Signature batch entries from the block — inputs to the STARK prover.
    pub entries: Vec<SigBatchEntry>,
}

impl ProofTask {
    /// Create a new proof task.
    pub fn new(block_hash: [u8; 32], block_number: u64, entries: Vec<SigBatchEntry>) -> Self {
        Self { block_hash, block_number, entries }
    }

    /// Number of signatures in this task.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

// ── ProofBacklog ──────────────────────────────────────────────────────────────

/// Default high-watermark: warn when the backlog exceeds this many tasks.
pub const DEFAULT_WATERMARK_THRESHOLD: usize = 64;

/// Async proof backlog — a bounded work queue for the background prover.
///
/// Tasks are queued in FIFO order.  The backlog exposes a *watermark*: the
/// depth at which it considers itself "above threshold" and signals that the
/// prover is falling behind block production.
///
/// # Thread safety
///
/// `ProofBacklog` is not `Sync` — callers should wrap it in a `Mutex` or
/// `tokio::sync::Mutex` when sharing across async tasks.
#[derive(Debug)]
pub struct ProofBacklog {
    pending: VecDeque<ProofTask>,
    /// Depth at which [`is_above_threshold`] returns `true`.
    ///
    /// [`is_above_threshold`]: ProofBacklog::is_above_threshold
    watermark_threshold: usize,
    /// Total tasks ever enqueued (monotonically increasing; never wraps in practice).
    total_enqueued: u64,
    /// Total tasks ever completed (popped via [`pop`]).
    ///
    /// [`pop`]: ProofBacklog::pop
    total_completed: u64,
}

impl ProofBacklog {
    /// Create a new backlog with the default watermark threshold.
    pub fn new() -> Self {
        Self::with_threshold(DEFAULT_WATERMARK_THRESHOLD)
    }

    /// Create a new backlog with a custom watermark threshold.
    pub fn with_threshold(watermark_threshold: usize) -> Self {
        Self {
            pending: VecDeque::new(),
            watermark_threshold,
            total_enqueued: 0,
            total_completed: 0,
        }
    }

    /// Push a new proving task onto the back of the queue.
    pub fn push(&mut self, task: ProofTask) {
        self.pending.push_back(task);
        self.total_enqueued += 1;
    }

    /// Pop the next task from the front of the queue (FIFO).
    ///
    /// Returns `None` when the backlog is empty.
    pub fn pop(&mut self) -> Option<ProofTask> {
        let task = self.pending.pop_front()?;
        self.total_completed += 1;
        Some(task)
    }

    /// Peek at the next task without removing it.
    pub fn peek(&self) -> Option<&ProofTask> {
        self.pending.front()
    }

    /// Current number of pending tasks.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// `true` when the backlog is empty.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// The configured high-watermark depth.
    pub fn watermark_threshold(&self) -> usize {
        self.watermark_threshold
    }

    /// `true` when the backlog depth exceeds the watermark threshold.
    ///
    /// Consumers (e.g. `ProverService`) should check this after each block to
    /// decide whether to activate additional prover capacity or log a warning.
    pub fn is_above_threshold(&self) -> bool {
        self.pending.len() > self.watermark_threshold
    }

    /// How far above (or below) the threshold the current depth is.
    ///
    /// Positive means the backlog is `n` tasks over the watermark.
    /// Negative means `n` tasks of remaining capacity before warning.
    pub fn watermark(&self) -> i64 {
        self.pending.len() as i64 - self.watermark_threshold as i64
    }

    /// Total tasks ever enqueued since creation.
    pub fn total_enqueued(&self) -> u64 {
        self.total_enqueued
    }

    /// Total tasks ever completed (successfully popped) since creation.
    pub fn total_completed(&self) -> u64 {
        self.total_completed
    }

    /// Drain all pending tasks, returning them in FIFO order.
    ///
    /// Useful for graceful shutdown — the caller can persist or re-queue tasks.
    pub fn drain(&mut self) -> Vec<ProofTask> {
        let tasks: Vec<_> = self.pending.drain(..).collect();
        self.total_completed += tasks.len() as u64;
        tasks
    }
}

impl Default for ProofBacklog {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(n: u64) -> ProofTask {
        ProofTask::new([n as u8; 32], n, vec![])
    }

    #[test]
    fn new_backlog_is_empty() {
        let b = ProofBacklog::new();
        assert!(b.is_empty());
        assert_eq!(b.len(), 0);
        assert_eq!(b.watermark_threshold(), DEFAULT_WATERMARK_THRESHOLD);
    }

    #[test]
    fn push_increases_len() {
        let mut b = ProofBacklog::new();
        b.push(make_task(1));
        assert_eq!(b.len(), 1);
        b.push(make_task(2));
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn pop_returns_fifo_order() {
        let mut b = ProofBacklog::new();
        b.push(make_task(10));
        b.push(make_task(20));
        b.push(make_task(30));

        assert_eq!(b.pop().unwrap().block_number, 10);
        assert_eq!(b.pop().unwrap().block_number, 20);
        assert_eq!(b.pop().unwrap().block_number, 30);
        assert!(b.pop().is_none());
    }

    #[test]
    fn pop_empty_returns_none() {
        let mut b = ProofBacklog::new();
        assert!(b.pop().is_none());
    }

    #[test]
    fn peek_does_not_remove() {
        let mut b = ProofBacklog::new();
        b.push(make_task(7));
        assert_eq!(b.peek().unwrap().block_number, 7);
        assert_eq!(b.len(), 1); // still there
    }

    #[test]
    fn watermark_below_threshold() {
        let b = ProofBacklog::with_threshold(10);
        assert!(!b.is_above_threshold());
        assert_eq!(b.watermark(), -10);
    }

    #[test]
    fn watermark_exactly_at_threshold_is_not_above() {
        let mut b = ProofBacklog::with_threshold(3);
        b.push(make_task(1));
        b.push(make_task(2));
        b.push(make_task(3));
        assert!(!b.is_above_threshold()); // len == threshold → NOT above
        assert_eq!(b.watermark(), 0);
    }

    #[test]
    fn watermark_above_threshold() {
        let mut b = ProofBacklog::with_threshold(3);
        for i in 0..5 {
            b.push(make_task(i));
        }
        assert!(b.is_above_threshold()); // len=5 > threshold=3
        assert_eq!(b.watermark(), 2);
    }

    #[test]
    fn total_enqueued_and_completed_counters() {
        let mut b = ProofBacklog::new();
        b.push(make_task(1));
        b.push(make_task(2));
        b.push(make_task(3));
        assert_eq!(b.total_enqueued(), 3);
        assert_eq!(b.total_completed(), 0);

        b.pop();
        assert_eq!(b.total_completed(), 1);
        b.pop();
        assert_eq!(b.total_completed(), 2);
    }

    #[test]
    fn drain_empties_backlog() {
        let mut b = ProofBacklog::new();
        for i in 0..5 {
            b.push(make_task(i));
        }
        let tasks = b.drain();
        assert_eq!(tasks.len(), 5);
        assert!(b.is_empty());
        assert_eq!(b.total_completed(), 5);
        // Tasks come out in FIFO order
        for (i, task) in tasks.iter().enumerate() {
            assert_eq!(task.block_number, i as u64);
        }
    }

    #[test]
    fn drain_empty_backlog_is_ok() {
        let mut b = ProofBacklog::new();
        let tasks = b.drain();
        assert!(tasks.is_empty());
    }

    #[test]
    fn proof_task_entry_count() {
        let entries = vec![
            SigBatchEntry { msg_hash: [0u8; 32], pk_hash: [1u8; 32] },
            SigBatchEntry { msg_hash: [2u8; 32], pk_hash: [3u8; 32] },
        ];
        let task = ProofTask::new([0u8; 32], 1, entries);
        assert_eq!(task.entry_count(), 2);
    }

    #[test]
    fn default_backlog_uses_default_threshold() {
        let b = ProofBacklog::default();
        assert_eq!(b.watermark_threshold(), DEFAULT_WATERMARK_THRESHOLD);
    }

    #[test]
    fn custom_threshold_respected() {
        let b = ProofBacklog::with_threshold(128);
        assert_eq!(b.watermark_threshold(), 128);
    }
}
