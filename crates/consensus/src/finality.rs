use std::collections::{HashMap, HashSet};
use serde::{Serialize, Deserialize};
use shell_primitives::{Address, ShellHash};

/// An attestation is a validator's signed confirmation that they accept a block.
/// Validators broadcast attestations after importing a valid block.
/// When a quorum (ceil(N/2)+1) of validators attest to a block, it becomes finalized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    /// Hash of the attested block.
    pub block_hash: ShellHash,
    /// Number of the attested block.
    pub block_number: u64,
    /// Address of the attesting validator.
    pub validator: Address,
    /// PQ signature over (block_hash || block_number) by the validator.
    pub signature: Vec<u8>,
}

impl Attestation {
    /// Create a new attestation.
    pub fn new(block_hash: ShellHash, block_number: u64, validator: Address, signature: Vec<u8>) -> Self {
        Self { block_hash, block_number, validator, signature }
    }

    /// The message that must be signed: block_hash ++ block_number (big-endian).
    pub fn signing_message(block_hash: &ShellHash, block_number: u64) -> Vec<u8> {
        let mut msg = Vec::with_capacity(40);
        msg.extend_from_slice(block_hash.as_bytes());
        msg.extend_from_slice(&block_number.to_be_bytes());
        msg
    }
}

/// Tracks finality state: which blocks have been finalized and pending attestations.
#[derive(Debug, Clone)]
pub struct FinalityState {
    /// The highest finalized block number.
    last_finalized_number: u64,
    /// The hash of the highest finalized block.
    last_finalized_hash: ShellHash,
    /// Pending attestations per block hash: maps block_hash -> set of validator addresses.
    pending_attestations: HashMap<ShellHash, HashSet<Address>>,
    /// Full attestation objects stored per block hash for verification.
    attestation_store: HashMap<ShellHash, Vec<Attestation>>,
}

impl FinalityState {
    /// Create a new finality state starting from genesis.
    pub fn new() -> Self {
        Self {
            last_finalized_number: 0,
            last_finalized_hash: ShellHash::ZERO,
            pending_attestations: HashMap::new(),
            attestation_store: HashMap::new(),
        }
    }

    /// Create a finality state restored from persistent storage.
    pub fn with_finalized(number: u64, hash: ShellHash) -> Self {
        Self {
            last_finalized_number: number,
            last_finalized_hash: hash,
            pending_attestations: HashMap::new(),
            attestation_store: HashMap::new(),
        }
    }

    /// Record an attestation. Returns true if this is a new (non-duplicate) attestation.
    pub fn record_attestation(&mut self, attestation: Attestation) -> bool {
        let validators = self.pending_attestations
            .entry(attestation.block_hash)
            .or_default();
        let is_new = validators.insert(attestation.validator);
        if is_new {
            self.attestation_store
                .entry(attestation.block_hash)
                .or_default()
                .push(attestation);
        }
        is_new
    }

    /// Check if a block has reached finality given the total validator count.
    /// Quorum = ceil(N/2) + 1 for N validators (strictly more than half).
    pub fn check_finality(&mut self, block_hash: &ShellHash, block_number: u64, total_validators: usize) -> bool {
        let quorum = Self::quorum_threshold(total_validators);
        let count = self.pending_attestations
            .get(block_hash)
            .map(|s| s.len())
            .unwrap_or(0);

        if count >= quorum && block_number > self.last_finalized_number {
            self.last_finalized_number = block_number;
            self.last_finalized_hash = *block_hash;
            // Prune attestations for blocks at or below the newly finalized block
            self.prune_below(block_number);
            true
        } else {
            false
        }
    }

    /// Calculate the quorum threshold: strictly more than half.
    /// For N validators: ceil(N/2) + 1 when N is even, (N+1)/2 when N is odd.
    /// Special case: N <= 1 returns 1.
    pub fn quorum_threshold(total_validators: usize) -> usize {
        if total_validators <= 1 {
            return 1;
        }
        (total_validators / 2) + 1
    }

    /// Last finalized block number.
    pub fn last_finalized_number(&self) -> u64 {
        self.last_finalized_number
    }

    /// Last finalized block hash.
    pub fn last_finalized_hash(&self) -> &ShellHash {
        &self.last_finalized_hash
    }

    /// Number of attestations for a specific block.
    pub fn attestation_count(&self, block_hash: &ShellHash) -> usize {
        self.pending_attestations
            .get(block_hash)
            .map(|s| s.len())
            .unwrap_or(0)
    }

    /// Get all attestations for a block.
    pub fn get_attestations(&self, block_hash: &ShellHash) -> Option<&Vec<Attestation>> {
        self.attestation_store.get(block_hash)
    }

    /// Total number of pending attestations across all blocks.
    pub fn total_pending_attestations(&self) -> usize {
        self.pending_attestations.values().map(|s| s.len()).sum()
    }

    /// Check if a validator has already attested to a block.
    pub fn has_attested(&self, block_hash: &ShellHash, validator: &Address) -> bool {
        self.pending_attestations
            .get(block_hash)
            .map(|s| s.contains(validator))
            .unwrap_or(false)
    }

    /// Detect equivocation: a validator attesting to two different blocks at the same height.
    /// Returns the conflicting block hash if equivocation is found.
    pub fn detect_equivocation(&self, block_hash: &ShellHash, block_number: u64, validator: &Address) -> Option<ShellHash> {
        for (hash, validators) in &self.pending_attestations {
            if hash != block_hash && validators.contains(validator) {
                // Check if any attestation for this different hash is at the same block number
                if let Some(attestations) = self.attestation_store.get(hash) {
                    for att in attestations {
                        if att.block_number == block_number && &att.validator == validator {
                            return Some(*hash);
                        }
                    }
                }
            }
        }
        None
    }

    /// Remove attestation data for blocks at or below the given number.
    fn prune_below(&mut self, finalized_number: u64) {
        let hashes_to_remove: Vec<ShellHash> = self.attestation_store
            .iter()
            .filter_map(|(hash, atts)| {
                atts.first()
                    .filter(|a| a.block_number <= finalized_number)
                    .map(|_| *hash)
            })
            .collect();

        for hash in hashes_to_remove {
            self.pending_attestations.remove(&hash);
            self.attestation_store.remove(&hash);
        }
    }
}

impl Default for FinalityState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(n: u8) -> ShellHash {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        ShellHash::from(bytes)
    }

    fn make_addr(n: u8) -> Address {
        let mut bytes = [0u8; 20];
        bytes[0] = n;
        Address::from(bytes)
    }

    #[test]
    fn test_attestation_new() {
        let hash = make_hash(1);
        let addr = make_addr(1);
        let att = Attestation::new(hash, 10, addr, vec![1, 2, 3]);
        assert_eq!(att.block_hash, hash);
        assert_eq!(att.block_number, 10);
        assert_eq!(att.validator, addr);
        assert_eq!(att.signature, vec![1, 2, 3]);
    }

    #[test]
    fn test_signing_message() {
        let hash = make_hash(42);
        let msg = Attestation::signing_message(&hash, 100);
        assert_eq!(msg.len(), 40); // 32 bytes hash + 8 bytes number
        assert_eq!(msg[0], 42);
        assert_eq!(&msg[32..], &100u64.to_be_bytes());
    }

    #[test]
    fn test_quorum_threshold() {
        assert_eq!(FinalityState::quorum_threshold(1), 1);
        assert_eq!(FinalityState::quorum_threshold(2), 2);
        assert_eq!(FinalityState::quorum_threshold(3), 2);
        assert_eq!(FinalityState::quorum_threshold(4), 3);
        assert_eq!(FinalityState::quorum_threshold(5), 3);
        assert_eq!(FinalityState::quorum_threshold(7), 4);
        assert_eq!(FinalityState::quorum_threshold(10), 6);
    }

    #[test]
    fn test_record_attestation_dedup() {
        let mut state = FinalityState::new();
        let hash = make_hash(1);
        let addr = make_addr(1);
        let att1 = Attestation::new(hash, 10, addr, vec![1]);
        let att2 = Attestation::new(hash, 10, addr, vec![2]);

        assert!(state.record_attestation(att1));
        assert!(!state.record_attestation(att2)); // duplicate validator
        assert_eq!(state.attestation_count(&hash), 1);
    }

    #[test]
    fn test_finality_not_reached() {
        let mut state = FinalityState::new();
        let hash = make_hash(1);

        // 1 of 3 validators
        state.record_attestation(Attestation::new(hash, 10, make_addr(1), vec![]));
        assert!(!state.check_finality(&hash, 10, 3));
        assert_eq!(state.last_finalized_number(), 0);
    }

    #[test]
    fn test_finality_reached() {
        let mut state = FinalityState::new();
        let hash = make_hash(1);

        // 2 of 3 validators → quorum = 2
        state.record_attestation(Attestation::new(hash, 10, make_addr(1), vec![]));
        state.record_attestation(Attestation::new(hash, 10, make_addr(2), vec![]));
        assert!(state.check_finality(&hash, 10, 3));
        assert_eq!(state.last_finalized_number(), 10);
        assert_eq!(state.last_finalized_hash(), &hash);
    }

    #[test]
    fn test_finality_requires_higher_block() {
        let mut state = FinalityState::with_finalized(20, make_hash(2));
        let hash = make_hash(1);

        // Even with quorum, block 10 < finalized 20 → no update
        state.record_attestation(Attestation::new(hash, 10, make_addr(1), vec![]));
        state.record_attestation(Attestation::new(hash, 10, make_addr(2), vec![]));
        assert!(!state.check_finality(&hash, 10, 3));
        assert_eq!(state.last_finalized_number(), 20);
    }

    #[test]
    fn test_has_attested() {
        let mut state = FinalityState::new();
        let hash = make_hash(1);
        let addr = make_addr(1);

        assert!(!state.has_attested(&hash, &addr));
        state.record_attestation(Attestation::new(hash, 10, addr, vec![]));
        assert!(state.has_attested(&hash, &addr));
    }

    #[test]
    fn test_equivocation_detection() {
        let mut state = FinalityState::new();
        let hash1 = make_hash(1);
        let hash2 = make_hash(2);
        let validator = make_addr(1);

        state.record_attestation(Attestation::new(hash1, 10, validator, vec![]));

        // Same validator, same height, different hash → equivocation
        let conflict = state.detect_equivocation(&hash2, 10, &validator);
        assert_eq!(conflict, Some(hash1));
    }

    #[test]
    fn test_no_equivocation_different_height() {
        let mut state = FinalityState::new();
        let hash1 = make_hash(1);
        let hash2 = make_hash(2);
        let validator = make_addr(1);

        state.record_attestation(Attestation::new(hash1, 10, validator, vec![]));

        // Different height → not equivocation
        let conflict = state.detect_equivocation(&hash2, 11, &validator);
        assert_eq!(conflict, None);
    }

    #[test]
    fn test_prune_below() {
        let mut state = FinalityState::new();
        let hash1 = make_hash(1);
        let hash2 = make_hash(2);

        state.record_attestation(Attestation::new(hash1, 5, make_addr(1), vec![]));
        state.record_attestation(Attestation::new(hash2, 15, make_addr(1), vec![]));

        // Finalize at block 15 → prune block 5 attestations
        state.record_attestation(Attestation::new(hash2, 15, make_addr(2), vec![]));
        assert!(state.check_finality(&hash2, 15, 3));

        assert_eq!(state.attestation_count(&hash1), 0); // pruned
        // hash2 also pruned since it's <= finalized (15)
    }

    #[test]
    fn test_five_of_seven_quorum() {
        let mut state = FinalityState::new();
        let hash = make_hash(1);

        // 7 validators, quorum = 4
        for i in 0..3 {
            state.record_attestation(Attestation::new(hash, 10, make_addr(i), vec![]));
        }
        assert!(!state.check_finality(&hash, 10, 7)); // 3 < 4

        state.record_attestation(Attestation::new(hash, 10, make_addr(3), vec![]));
        assert!(state.check_finality(&hash, 10, 7)); // 4 >= 4
    }

    #[test]
    fn test_default_state() {
        let state = FinalityState::default();
        assert_eq!(state.last_finalized_number(), 0);
        assert_eq!(state.last_finalized_hash(), &ShellHash::ZERO);
    }

    // ---- Additional comprehensive tests ----

    #[test]
    fn quorum_exactly_at_threshold() {
        // Verify quorum detection at exact threshold for various validator counts
        for total in [3, 4, 5, 6, 7, 10, 13, 20] {
            let quorum = FinalityState::quorum_threshold(total);
            let hash = make_hash(total as u8);
            let mut state = FinalityState::new();

            // Add exactly quorum - 1 attestations → should NOT finalize
            for i in 0..quorum - 1 {
                state.record_attestation(Attestation::new(hash, 100, make_addr(i as u8), vec![]));
            }
            assert!(
                !state.check_finality(&hash, 100, total),
                "N={total}: {0} attestations (quorum={quorum}) should NOT finalize",
                quorum - 1
            );

            // Add one more → exactly at quorum → should finalize
            state.record_attestation(Attestation::new(hash, 100, make_addr(quorum as u8), vec![]));
            // Reset finalized state so block 100 > 0 finalized
            let mut state2 = FinalityState::new();
            for i in 0..quorum {
                state2.record_attestation(Attestation::new(hash, 100, make_addr(i as u8), vec![]));
            }
            assert!(
                state2.check_finality(&hash, 100, total),
                "N={total}: {quorum} attestations should finalize"
            );
        }
    }

    #[test]
    fn below_quorum_does_not_finalize() {
        let mut state = FinalityState::new();
        let hash = make_hash(1);

        // 5 of 10 validators → quorum is 6
        for i in 0..5 {
            state.record_attestation(Attestation::new(hash, 50, make_addr(i), vec![]));
        }
        assert!(!state.check_finality(&hash, 50, 10));
        assert_eq!(state.last_finalized_number(), 0, "should not have advanced finality");
    }

    #[test]
    fn multiple_finalization_rounds() {
        let mut state = FinalityState::new();

        // Round 1: finalize block 10
        let hash10 = make_hash(10);
        for i in 0..3 {
            state.record_attestation(Attestation::new(hash10, 10, make_addr(i), vec![]));
        }
        assert!(state.check_finality(&hash10, 10, 4)); // quorum = 3 for N=4
        assert_eq!(state.last_finalized_number(), 10);

        // Round 2: finalize block 20
        let hash20 = make_hash(20);
        for i in 0..3 {
            state.record_attestation(Attestation::new(hash20, 20, make_addr(100 + i), vec![]));
        }
        assert!(state.check_finality(&hash20, 20, 4));
        assert_eq!(state.last_finalized_number(), 20);
        assert_eq!(state.last_finalized_hash(), &hash20);

        // Round 3: finalize block 30
        let hash30 = make_hash(30);
        for i in 0..3 {
            state.record_attestation(Attestation::new(hash30, 30, make_addr(200 + i), vec![]));
        }
        assert!(state.check_finality(&hash30, 30, 4));
        assert_eq!(state.last_finalized_number(), 30);
    }

    #[test]
    fn large_validator_set_quorum() {
        let mut state = FinalityState::new();
        let hash = make_hash(1);
        let total: usize = 100;
        let quorum = FinalityState::quorum_threshold(total); // 51

        assert_eq!(quorum, 51);

        // Add 50 attestations → not enough
        for i in 0..50u8 {
            state.record_attestation(Attestation::new(hash, 500, make_addr(i), vec![]));
        }
        assert!(!state.check_finality(&hash, 500, total));

        // Add 1 more → exactly 51 → quorum
        state.record_attestation(Attestation::new(hash, 500, make_addr(50), vec![]));
        assert!(state.check_finality(&hash, 500, total));
        assert_eq!(state.last_finalized_number(), 500);
    }

    #[test]
    fn finalization_monotonically_advances() {
        let mut state = FinalityState::new();

        // Finalize block 20 first
        let hash20 = make_hash(20);
        for i in 0..3 {
            state.record_attestation(Attestation::new(hash20, 20, make_addr(i), vec![]));
        }
        assert!(state.check_finality(&hash20, 20, 4));
        assert_eq!(state.last_finalized_number(), 20);

        // Try to finalize block 15 (lower) — should fail
        let hash15 = make_hash(15);
        for i in 10..13 {
            state.record_attestation(Attestation::new(hash15, 15, make_addr(i), vec![]));
        }
        assert!(!state.check_finality(&hash15, 15, 4));
        assert_eq!(state.last_finalized_number(), 20, "finality must not go backwards");

        // Finalize block 25 (higher) — should succeed
        let hash25 = make_hash(25);
        for i in 20..23 {
            state.record_attestation(Attestation::new(hash25, 25, make_addr(i), vec![]));
        }
        assert!(state.check_finality(&hash25, 25, 4));
        assert_eq!(state.last_finalized_number(), 25);
    }

    #[test]
    fn prune_preserves_above_finalized() {
        let mut state = FinalityState::new();
        let hash_low = make_hash(1);
        let hash_high = make_hash(2);
        let hash_future = make_hash(3);

        // Attestation at height 5
        state.record_attestation(Attestation::new(hash_low, 5, make_addr(1), vec![]));
        // Attestation at height 10
        state.record_attestation(Attestation::new(hash_high, 10, make_addr(2), vec![]));
        state.record_attestation(Attestation::new(hash_high, 10, make_addr(3), vec![]));
        // Attestation at height 20
        state.record_attestation(Attestation::new(hash_future, 20, make_addr(4), vec![]));

        // Finalize at height 10 → prune heights <= 10
        assert!(state.check_finality(&hash_high, 10, 3));

        // Height 5 should be pruned
        assert_eq!(state.attestation_count(&hash_low), 0);
        // Height 10 also pruned (it's <= finalized)
        assert_eq!(state.attestation_count(&hash_high), 0);
        // Height 20 should survive
        assert_eq!(state.attestation_count(&hash_future), 1);
    }

    #[test]
    fn total_pending_attestations_tracking() {
        let mut state = FinalityState::new();
        assert_eq!(state.total_pending_attestations(), 0);

        let hash1 = make_hash(1);
        let hash2 = make_hash(2);

        state.record_attestation(Attestation::new(hash1, 10, make_addr(1), vec![]));
        assert_eq!(state.total_pending_attestations(), 1);

        state.record_attestation(Attestation::new(hash1, 10, make_addr(2), vec![]));
        assert_eq!(state.total_pending_attestations(), 2);

        state.record_attestation(Attestation::new(hash2, 11, make_addr(3), vec![]));
        assert_eq!(state.total_pending_attestations(), 3);

        // Duplicate should not increase count
        state.record_attestation(Attestation::new(hash1, 10, make_addr(1), vec![]));
        assert_eq!(state.total_pending_attestations(), 3);
    }

    #[test]
    fn with_finalized_constructor() {
        let hash = make_hash(42);
        let state = FinalityState::with_finalized(100, hash);
        assert_eq!(state.last_finalized_number(), 100);
        assert_eq!(state.last_finalized_hash(), &hash);
        assert_eq!(state.total_pending_attestations(), 0);
    }

    #[test]
    fn equivocation_not_detected_same_hash() {
        let mut state = FinalityState::new();
        let hash = make_hash(1);
        let validator = make_addr(1);

        state.record_attestation(Attestation::new(hash, 10, validator, vec![]));

        // Same hash, same validator — not equivocation (just a duplicate)
        let conflict = state.detect_equivocation(&hash, 10, &validator);
        assert_eq!(conflict, None, "same hash should not be equivocation");
    }

    #[test]
    fn concurrent_blocks_at_same_height() {
        let mut state = FinalityState::new();
        let hash_a = make_hash(1);
        let hash_b = make_hash(2);

        // Different validators attest to different blocks at height 10
        state.record_attestation(Attestation::new(hash_a, 10, make_addr(1), vec![]));
        state.record_attestation(Attestation::new(hash_a, 10, make_addr(2), vec![]));
        state.record_attestation(Attestation::new(hash_b, 10, make_addr(3), vec![]));

        // hash_a has 2 attestations, hash_b has 1
        assert_eq!(state.attestation_count(&hash_a), 2);
        assert_eq!(state.attestation_count(&hash_b), 1);

        // With 3 total validators, quorum = 2: hash_a should finalize
        assert!(state.check_finality(&hash_a, 10, 3));
        assert_eq!(state.last_finalized_hash(), &hash_a);
    }

    #[test]
    fn finality_requires_quorum_not_just_any_count() {
        let mut state = FinalityState::new();
        let hash = make_hash(1);

        // 1 of 10 validators
        state.record_attestation(Attestation::new(hash, 10, make_addr(1), vec![]));
        assert!(!state.check_finality(&hash, 10, 10)); // quorum = 6

        // 5 of 10 validators
        for i in 2..=5 {
            state.record_attestation(Attestation::new(hash, 10, make_addr(i), vec![]));
        }
        assert!(!state.check_finality(&hash, 10, 10)); // still only 5 < 6

        // 6 of 10 validators → exactly quorum
        state.record_attestation(Attestation::new(hash, 10, make_addr(6), vec![]));
        assert!(state.check_finality(&hash, 10, 10));
    }
}
