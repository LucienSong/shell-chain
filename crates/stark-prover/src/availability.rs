//! K1: Proof availability tracking.
//!
//! Tracks which blocks have received STARK proof amendments, how many peers
//! have acknowledged holding a copy (`ProofAck`), and whether a block's proof
//! is considered "available" (i.e., replicated enough times to be safe).
//!
//! # Design
//!
//! `ProofAvailabilityTracker` maintains a per-block record:
//! - Whether a proof has been submitted locally.
//! - The set of peer addresses that sent a `ProofAck` for this block.
//! - A computed availability status (Pending / Available / Unavailable).
//!
//! The tracker does not verify proof content — it only counts acknowledgements.
//! Actual verification is done by `ProofAmendmentStore` (G3) and the challenge
//! mechanism (I2).

use std::collections::{HashMap, HashSet};
use shell_primitives::{Address, ShellHash};

/// Configuration for proof availability.
#[derive(Debug, Clone)]
pub struct AvailabilityConfig {
    /// Minimum number of unique peers that must hold a proof for it to be
    /// considered "available". Default: 2.
    pub min_ack_count: usize,
    /// Number of blocks after which an unproven block is considered unavailable.
    /// Default: 200.
    pub availability_timeout_blocks: u64,
}

impl Default for AvailabilityConfig {
    fn default() -> Self {
        Self {
            min_ack_count: 2,
            availability_timeout_blocks: 200,
        }
    }
}

/// Availability status of a block's proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProofAvailability {
    /// No proof has been submitted yet.
    Pending,
    /// A proof has been submitted and replicated to enough peers.
    Available { ack_count: usize },
    /// The availability window has closed without sufficient replication.
    Unavailable,
}

/// Per-block proof availability record.
#[derive(Debug, Clone)]
struct BlockProofRecord {
    /// True if we have the proof locally.
    local: bool,
    /// Set of peer addresses that sent `ProofAck` for this block.
    acks: HashSet<Address>,
    /// Block number at which the proof was first seen (for timeout tracking).
    first_seen_at: u64,
}

/// K1: Tracks STARK proof availability across the network.
#[derive(Debug)]
pub struct ProofAvailabilityTracker {
    config: AvailabilityConfig,
    records: HashMap<ShellHash, BlockProofRecord>,
}

impl ProofAvailabilityTracker {
    pub fn new(config: AvailabilityConfig) -> Self {
        Self { config, records: HashMap::new() }
    }

    /// Record that we have received and stored a proof locally for `block_hash`.
    pub fn record_local_proof(&mut self, block_hash: ShellHash, current_block: u64) {
        let record = self.records.entry(block_hash).or_insert_with(|| BlockProofRecord {
            local: false,
            acks: HashSet::new(),
            first_seen_at: current_block,
        });
        record.local = true;
    }

    /// Record a `ProofAck` from a peer for `block_hash`.
    pub fn record_ack(&mut self, block_hash: ShellHash, holder: Address, current_block: u64) {
        let record = self.records.entry(block_hash).or_insert_with(|| BlockProofRecord {
            local: false,
            acks: HashSet::new(),
            first_seen_at: current_block,
        });
        record.acks.insert(holder);
    }

    /// Query the current availability status for `block_hash`.
    pub fn availability(&self, block_hash: &ShellHash, current_block: u64) -> ProofAvailability {
        match self.records.get(block_hash) {
            None => ProofAvailability::Pending,
            Some(record) => {
                // Count local + remote acks (local counts as 1 if held).
                let total = record.acks.len() + if record.local { 1 } else { 0 };
                if total >= self.config.min_ack_count {
                    ProofAvailability::Available { ack_count: total }
                } else if current_block
                    > record.first_seen_at + self.config.availability_timeout_blocks
                {
                    ProofAvailability::Unavailable
                } else {
                    ProofAvailability::Pending
                }
            }
        }
    }

    /// Remove records for blocks older than `availability_timeout_blocks`.
    pub fn gc(&mut self, current_block: u64) {
        let timeout = self.config.availability_timeout_blocks;
        self.records.retain(|_, r| {
            current_block <= r.first_seen_at + timeout + 10
        });
    }

    /// Number of tracked blocks.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::{Address, ShellHash};

    fn hash(n: u8) -> ShellHash { ShellHash::from([n; 32]) }
    fn addr(n: u8) -> Address { Address::from([n; 20]) }

    fn tracker() -> ProofAvailabilityTracker {
        ProofAvailabilityTracker::new(AvailabilityConfig {
            min_ack_count: 2,
            availability_timeout_blocks: 100,
        })
    }

    #[test]
    fn pending_when_no_record() {
        let t = tracker();
        assert_eq!(t.availability(&hash(1), 0), ProofAvailability::Pending);
    }

    #[test]
    fn local_proof_alone_not_available_at_min_2() {
        let mut t = tracker();
        t.record_local_proof(hash(1), 0);
        assert_eq!(t.availability(&hash(1), 0), ProofAvailability::Pending);
    }

    #[test]
    fn local_plus_one_ack_makes_available() {
        let mut t = tracker();
        t.record_local_proof(hash(1), 0);
        t.record_ack(hash(1), addr(1), 0);
        assert!(matches!(t.availability(&hash(1), 0), ProofAvailability::Available { ack_count: 2 }));
    }

    #[test]
    fn two_acks_without_local_makes_available() {
        let mut t = tracker();
        t.record_ack(hash(1), addr(1), 0);
        t.record_ack(hash(1), addr(2), 0);
        assert!(matches!(t.availability(&hash(1), 0), ProofAvailability::Available { ack_count: 2 }));
    }

    #[test]
    fn duplicate_ack_from_same_peer_not_double_counted() {
        let mut t = tracker();
        t.record_ack(hash(1), addr(1), 0);
        t.record_ack(hash(1), addr(1), 0); // duplicate
        assert_eq!(t.availability(&hash(1), 0), ProofAvailability::Pending);
    }

    #[test]
    fn unavailable_after_timeout() {
        let mut t = tracker();
        t.record_local_proof(hash(1), 0); // only 1 holder
        // Advance past timeout.
        assert_eq!(t.availability(&hash(1), 101), ProofAvailability::Unavailable);
    }

    #[test]
    fn gc_removes_old_records() {
        let mut t = tracker();
        t.record_local_proof(hash(1), 0);
        t.gc(200); // 200 > 0+100+10 → removed
        assert_eq!(t.len(), 0);
    }
}
