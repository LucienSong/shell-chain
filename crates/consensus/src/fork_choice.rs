use std::collections::HashMap;
use shell_primitives::ShellHash;

/// Score assigned to a block for fork choice comparison.
/// Higher score = preferred chain. Compared lexicographically by fields in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockScore {
    /// Whether this block is on the finalized chain (1 = yes, 0 = no).
    /// Finalized chains always win.
    pub is_finalized: u8,
    /// Number of attestations this block has received.
    pub attestation_count: usize,
    /// Block number (height). Higher = better.
    pub block_number: u64,
    /// Block hash used as tiebreaker (higher hash bytes = preferred).
    pub block_hash: ShellHash,
}

impl PartialOrd for BlockScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for BlockScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.is_finalized
            .cmp(&other.is_finalized)
            .then(self.attestation_count.cmp(&other.attestation_count))
            .then(self.block_number.cmp(&other.block_number))
            .then(self.block_hash.as_bytes().cmp(other.block_hash.as_bytes()))
    }
}

/// Fork choice rule implementation.
///
/// Maintains a block tree and selects the canonical head based on:
/// 1. Finalized chain always wins
/// 2. More attestations = preferred
/// 3. Higher block number = preferred
/// 4. Higher block hash = tiebreaker
pub struct ForkChoice {
    /// Maps block hash to parent hash for tree traversal.
    parent_map: HashMap<ShellHash, ShellHash>,
    /// Maps block hash to its score.
    scores: HashMap<ShellHash, BlockScore>,
    /// Current canonical head.
    head: ShellHash,
    /// Current head score.
    head_score: BlockScore,
}

impl ForkChoice {
    /// Create a new fork choice tracker starting from genesis.
    pub fn new(genesis_hash: ShellHash) -> Self {
        let score = BlockScore {
            is_finalized: 0,
            attestation_count: 0,
            block_number: 0,
            block_hash: genesis_hash,
        };
        let mut scores = HashMap::new();
        scores.insert(genesis_hash, score.clone());
        let mut parent_map = HashMap::new();
        parent_map.insert(genesis_hash, ShellHash::ZERO);

        Self {
            parent_map,
            scores,
            head: genesis_hash,
            head_score: score,
        }
    }

    /// Register a new block in the fork choice tree.
    /// Returns true if this block becomes the new head.
    pub fn add_block(
        &mut self,
        block_hash: ShellHash,
        parent_hash: ShellHash,
        block_number: u64,
        attestation_count: usize,
        is_on_finalized_chain: bool,
    ) -> bool {
        let score = BlockScore {
            is_finalized: if is_on_finalized_chain { 1 } else { 0 },
            attestation_count,
            block_number,
            block_hash,
        };

        self.parent_map.insert(block_hash, parent_hash);
        self.scores.insert(block_hash, score.clone());

        if score > self.head_score {
            self.head = block_hash;
            self.head_score = score;
            true
        } else {
            false
        }
    }

    /// Update attestation count for a block. Returns true if head changed.
    pub fn update_attestations(&mut self, block_hash: &ShellHash, new_count: usize) -> bool {
        if let Some(score) = self.scores.get_mut(block_hash) {
            score.attestation_count = new_count;
            let updated_score = score.clone();

            if updated_score > self.head_score {
                self.head = *block_hash;
                self.head_score = updated_score;
                return true;
            }
            // Re-check in case the current head's score was updated
            if block_hash == &self.head {
                self.head_score = updated_score;
            }
        }
        false
    }

    /// Mark a block as finalized. Returns true if head changed.
    pub fn mark_finalized(&mut self, block_hash: &ShellHash) -> bool {
        if let Some(score) = self.scores.get_mut(block_hash) {
            score.is_finalized = 1;
            let updated_score = score.clone();

            if updated_score > self.head_score {
                self.head = *block_hash;
                self.head_score = updated_score;
                return true;
            }
            if block_hash == &self.head {
                self.head_score = updated_score;
            }
        }
        false
    }

    /// Get the current canonical head hash.
    pub fn head(&self) -> &ShellHash {
        &self.head
    }

    /// Get the score for a block.
    pub fn score(&self, block_hash: &ShellHash) -> Option<&BlockScore> {
        self.scores.get(block_hash)
    }

    /// Get the parent hash of a block.
    pub fn parent(&self, block_hash: &ShellHash) -> Option<&ShellHash> {
        self.parent_map.get(block_hash)
    }

    /// Check if a block is known to fork choice.
    pub fn contains(&self, block_hash: &ShellHash) -> bool {
        self.scores.contains_key(block_hash)
    }

    /// Find the common ancestor of two blocks by walking up the parent chain.
    /// Returns None if blocks are not in the same tree.
    pub fn find_common_ancestor(
        &self,
        hash_a: &ShellHash,
        hash_b: &ShellHash,
    ) -> Option<ShellHash> {
        // Collect ancestors of A
        let mut ancestors_a = std::collections::HashSet::new();
        let mut current = *hash_a;
        loop {
            ancestors_a.insert(current);
            match self.parent_map.get(&current) {
                Some(parent) if *parent != ShellHash::ZERO => current = *parent,
                Some(_) => break, // reached genesis
                None => break,
            }
        }

        // Walk up from B until we find a common ancestor
        let mut current = *hash_b;
        loop {
            if ancestors_a.contains(&current) {
                return Some(current);
            }
            match self.parent_map.get(&current) {
                Some(parent) if *parent != ShellHash::ZERO => current = *parent,
                Some(_) => {
                    // At genesis — check if genesis is a common ancestor
                    if ancestors_a.contains(&current) {
                        return Some(current);
                    }
                    return None;
                }
                None => return None,
            }
        }
    }

    /// Collect the chain from `from_hash` back to `to_hash` (exclusive).
    /// Returns block hashes in order from oldest to newest.
    pub fn chain_between(&self, from_hash: &ShellHash, to_hash: &ShellHash) -> Vec<ShellHash> {
        let mut chain = Vec::new();
        let mut current = *from_hash;
        while current != *to_hash {
            chain.push(current);
            match self.parent_map.get(&current) {
                Some(parent) => current = *parent,
                None => return Vec::new(), // broken chain
            }
        }
        chain.reverse();
        chain
    }

    /// Remove blocks that are below the finalized height and not on the canonical chain.
    /// This prevents unbounded memory growth.
    pub fn prune_below(&mut self, finalized_number: u64) {
        let to_remove: Vec<ShellHash> = self
            .scores
            .iter()
            .filter(|(_, score)| {
                score.block_number < finalized_number
                    && score.is_finalized == 0
                    && score.block_number > 0 // never prune genesis
            })
            .map(|(hash, _)| *hash)
            .collect();

        for hash in to_remove {
            self.scores.remove(&hash);
            self.parent_map.remove(&hash);
        }
    }

    /// Number of tracked blocks.
    pub fn block_count(&self) -> usize {
        self.scores.len()
    }

    /// Re-evaluate head by scanning all scores. Use after bulk updates.
    pub fn recalculate_head(&mut self) {
        if let Some((hash, score)) = self.scores.iter().max_by_key(|(_, s)| (*s).clone()) {
            self.head = *hash;
            self.head_score = score.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> ShellHash {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        bytes[31] = 1; // ensure non-zero so it doesn't collide with ZERO sentinel
        ShellHash::from(bytes)
    }

    #[test]
    fn test_genesis_is_head() {
        let fc = ForkChoice::new(hash(0));
        assert_eq!(fc.head(), &hash(0));
        assert!(fc.contains(&hash(0)));
    }

    #[test]
    fn test_linear_chain() {
        let mut fc = ForkChoice::new(hash(0));
        assert!(fc.add_block(hash(1), hash(0), 1, 0, true));
        assert_eq!(fc.head(), &hash(1));
        assert!(fc.add_block(hash(2), hash(1), 2, 0, true));
        assert_eq!(fc.head(), &hash(2));
    }

    #[test]
    fn test_fork_higher_block_wins() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 0, false);
        // Fork at genesis: block 2 is also at height 1 but with higher hash
        let became_head = fc.add_block(hash(2), hash(0), 1, 0, false);
        // hash(2) > hash(1) as tiebreaker
        assert!(became_head);
        assert_eq!(fc.head(), &hash(2));
    }

    #[test]
    fn test_attestations_win_over_height() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 0, false);
        fc.add_block(hash(2), hash(1), 2, 0, false); // height 2, 0 attestations
        fc.add_block(hash(3), hash(0), 1, 5, false); // height 1, 5 attestations
        assert_eq!(fc.head(), &hash(3));
    }

    #[test]
    fn test_finalized_always_wins() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 10, false); // 10 attestations, not finalized
        fc.add_block(hash(2), hash(0), 1, 1, true); // 1 attestation, finalized
        assert_eq!(fc.head(), &hash(2));
    }

    #[test]
    fn test_update_attestations_changes_head() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 0, false);
        fc.add_block(hash(2), hash(0), 1, 0, false);
        // hash(2) > hash(1) as bytes, so hash(2) is head
        assert_eq!(fc.head(), &hash(2));
        let changed = fc.update_attestations(&hash(1), 5);
        assert!(changed);
        assert_eq!(fc.head(), &hash(1));
    }

    #[test]
    fn test_mark_finalized() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 0, false);
        fc.add_block(hash(2), hash(1), 2, 5, false); // higher score
        // is_finalized comparison happens first: 1 > 0, so hash(1) wins
        fc.mark_finalized(&hash(1));
        assert_eq!(fc.head(), &hash(1));
    }

    #[test]
    fn test_common_ancestor_linear() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 0, true);
        fc.add_block(hash(2), hash(1), 2, 0, true);

        let ancestor = fc.find_common_ancestor(&hash(2), &hash(1));
        assert_eq!(ancestor, Some(hash(1)));

        let ancestor = fc.find_common_ancestor(&hash(2), &hash(0));
        assert_eq!(ancestor, Some(hash(0)));
    }

    #[test]
    fn test_common_ancestor_fork() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 0, false);
        fc.add_block(hash(2), hash(1), 2, 0, false);
        fc.add_block(hash(3), hash(0), 1, 0, false); // fork from genesis
        fc.add_block(hash(4), hash(3), 2, 0, false);
        let ancestor = fc.find_common_ancestor(&hash(2), &hash(4));
        assert_eq!(ancestor, Some(hash(0)));
    }

    #[test]
    fn test_chain_between() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 0, true);
        fc.add_block(hash(2), hash(1), 2, 0, true);
        fc.add_block(hash(3), hash(2), 3, 0, true);
        let chain = fc.chain_between(&hash(3), &hash(0));
        assert_eq!(chain, vec![hash(1), hash(2), hash(3)]);
    }

    #[test]
    fn test_prune_below() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 0, false);
        fc.add_block(hash(2), hash(1), 2, 0, true); // finalized
        fc.add_block(hash(3), hash(0), 1, 0, false); // fork, not finalized
        fc.prune_below(2);
        assert!(!fc.contains(&hash(3))); // pruned
        assert!(!fc.contains(&hash(1))); // pruned
        assert!(fc.contains(&hash(2))); // kept (finalized)
        assert!(fc.contains(&hash(0))); // kept (genesis, block_number=0)
    }

    #[test]
    fn test_block_count() {
        let mut fc = ForkChoice::new(hash(0));
        assert_eq!(fc.block_count(), 1);
        fc.add_block(hash(1), hash(0), 1, 0, false);
        assert_eq!(fc.block_count(), 2);
    }

    #[test]
    fn test_score_ordering() {
        let s1 = BlockScore {
            is_finalized: 0,
            attestation_count: 10,
            block_number: 5,
            block_hash: hash(1),
        };
        let s2 = BlockScore {
            is_finalized: 1,
            attestation_count: 0,
            block_number: 1,
            block_hash: hash(2),
        };
        assert!(s2 > s1); // finalized wins

        let s3 = BlockScore {
            is_finalized: 0,
            attestation_count: 5,
            block_number: 10,
            block_hash: hash(3),
        };
        let s4 = BlockScore {
            is_finalized: 0,
            attestation_count: 3,
            block_number: 100,
            block_hash: hash(4),
        };
        assert!(s3 > s4); // more attestations wins over height
    }

    #[test]
    fn test_recalculate_head() {
        let mut fc = ForkChoice::new(hash(0));
        fc.add_block(hash(1), hash(0), 1, 0, false);
        fc.add_block(hash(2), hash(0), 1, 0, false);

        // Manually change score
        if let Some(score) = fc.scores.get_mut(&hash(1)) {
            score.attestation_count = 100;
        }
        fc.recalculate_head();
        assert_eq!(fc.head(), &hash(1));
    }
}
