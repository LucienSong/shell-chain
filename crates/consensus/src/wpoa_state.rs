//! wPoA round state machine: propose → vote → commit + view-change.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use shell_crypto::PQSignature;
use shell_primitives::{Address, ShellHash};

/// States for a single consensus round.
pub enum RoundPhase {
    Idle,
    Proposing {
        block_hash: ShellHash,
        proposed_at: Instant,
    },
    Voting {
        block_hash: ShellHash,
        votes: HashMap<Address, PQSignature>,
        vote_weight: u64,
        started_at: Instant,
    },
    Committed {
        block_hash: ShellHash,
    },
    ViewChanging {
        new_view: u64,
        votes: HashSet<Address>,
        vote_weight: u64,
        started_at: Instant,
    },
}

/// Events emitted by the wPoA round state machine.
#[derive(Debug, Clone)]
pub enum WPoaEvent {
    ProposeAccepted {
        block_hash: ShellHash,
    },
    VoteNeeded {
        block_hash: ShellHash,
    },
    BlockCommitted {
        block_hash: ShellHash,
        quorum_signatures: HashMap<Address, PQSignature>,
    },
    ProposeTimeout {
        current_round: u64,
    },
    VoteTimeout {
        current_round: u64,
    },
    ViewChangeReady {
        new_view: u64,
    },
    DuplicateVote {
        voter: Address,
    },
    WrongBlockHash {
        expected: ShellHash,
        got: ShellHash,
    },
}

/// wPoA round state machine for a single block height.
pub struct WPoaRound {
    /// Current view/round number (increments on view-change).
    pub round: u64,
    /// Block number this round targets.
    pub block_number: u64,
    /// Current phase.
    pub phase: RoundPhase,
    /// Sum of all active validator weights.
    total_weight: u64,
    /// Per-validator weight lookup.
    validator_weights: HashMap<Address, u64>,
    /// Timeout (ms) for voting phase.
    pub vote_timeout_ms: u64,
    /// Timeout (ms) for proposing phase.
    pub propose_timeout_ms: u64,
}

impl WPoaRound {
    /// Create a new round in Idle state.
    pub fn new(block_number: u64, round: u64, validator_weights: HashMap<Address, u64>) -> Self {
        let total_weight = validator_weights.values().sum();
        Self {
            round,
            block_number,
            phase: RoundPhase::Idle,
            total_weight,
            validator_weights,
            vote_timeout_ms: 4000,
            propose_timeout_ms: 2000,
        }
    }

    /// Compute the quorum threshold: ceiling of 2/3 * total_weight.
    fn quorum_weight(&self) -> u64 {
        (2 * self.total_weight + 2) / 3
    }

    /// Handle a block proposal. Transitions Idle → Voting.
    ///
    /// Returns ProposeAccepted + VoteNeeded events on success.
    pub fn on_block_proposed(
        &mut self,
        block_hash: ShellHash,
        _proposer: Address,
    ) -> Vec<WPoaEvent> {
        match &self.phase {
            RoundPhase::Idle => {
                self.phase = RoundPhase::Voting {
                    block_hash,
                    votes: HashMap::new(),
                    vote_weight: 0,
                    started_at: Instant::now(),
                };
                vec![
                    WPoaEvent::ProposeAccepted { block_hash },
                    WPoaEvent::VoteNeeded { block_hash },
                ]
            }
            _ => vec![],
        }
    }

    /// Handle an incoming vote. Returns BlockCommitted when quorum is reached.
    pub fn on_vote(
        &mut self,
        voter: Address,
        block_hash: ShellHash,
        sig: PQSignature,
    ) -> Vec<WPoaEvent> {
        // Validate voter is known.
        let weight = match self.validator_weights.get(&voter).copied() {
            Some(w) => w,
            None => return vec![],
        };

        let phase = std::mem::replace(&mut self.phase, RoundPhase::Idle);
        match phase {
            RoundPhase::Voting {
                block_hash: expected_hash,
                mut votes,
                mut vote_weight,
                started_at,
            } => {
                // Check block hash matches.
                if block_hash != expected_hash {
                    self.phase = RoundPhase::Voting {
                        block_hash: expected_hash,
                        votes,
                        vote_weight,
                        started_at,
                    };
                    return vec![WPoaEvent::WrongBlockHash {
                        expected: expected_hash,
                        got: block_hash,
                    }];
                }
                // Check for duplicate vote.
                if votes.contains_key(&voter) {
                    self.phase = RoundPhase::Voting {
                        block_hash: expected_hash,
                        votes,
                        vote_weight,
                        started_at,
                    };
                    return vec![WPoaEvent::DuplicateVote { voter }];
                }
                // Record vote.
                votes.insert(voter, sig);
                vote_weight += weight;

                if vote_weight >= self.quorum_weight() {
                    // Quorum reached!
                    let quorum_signatures = votes.clone();
                    self.phase = RoundPhase::Committed {
                        block_hash: expected_hash,
                    };
                    vec![WPoaEvent::BlockCommitted {
                        block_hash: expected_hash,
                        quorum_signatures,
                    }]
                } else {
                    self.phase = RoundPhase::Voting {
                        block_hash: expected_hash,
                        votes,
                        vote_weight,
                        started_at,
                    };
                    vec![]
                }
            }
            other => {
                self.phase = other;
                vec![]
            }
        }
    }

    /// Handle a view-change vote. Returns ViewChangeReady when quorum is reached.
    pub fn on_view_change_vote(&mut self, voter: Address, new_view: u64) -> Vec<WPoaEvent> {
        let weight = match self.validator_weights.get(&voter).copied() {
            Some(w) => w,
            None => return vec![],
        };

        let phase = std::mem::replace(&mut self.phase, RoundPhase::Idle);
        match phase {
            RoundPhase::ViewChanging {
                new_view: expected_view,
                mut votes,
                mut vote_weight,
                started_at,
            } if expected_view == new_view => {
                if votes.contains(&voter) {
                    self.phase = RoundPhase::ViewChanging {
                        new_view: expected_view,
                        votes,
                        vote_weight,
                        started_at,
                    };
                    return vec![];
                }
                votes.insert(voter);
                vote_weight += weight;

                if vote_weight >= self.quorum_weight() {
                    self.phase = RoundPhase::Idle;
                    vec![WPoaEvent::ViewChangeReady { new_view }]
                } else {
                    self.phase = RoundPhase::ViewChanging {
                        new_view: expected_view,
                        votes,
                        vote_weight,
                        started_at,
                    };
                    vec![]
                }
            }
            other => {
                self.phase = other;
                vec![]
            }
        }
    }

    /// Check for timeouts. Should be called periodically.
    pub fn tick(&self, now: Instant) -> Vec<WPoaEvent> {
        match &self.phase {
            RoundPhase::Proposing { proposed_at, .. } => {
                if now.duration_since(*proposed_at).as_millis() as u64 > self.propose_timeout_ms {
                    vec![WPoaEvent::ProposeTimeout {
                        current_round: self.round,
                    }]
                } else {
                    vec![]
                }
            }
            RoundPhase::Voting { started_at, .. } => {
                if now.duration_since(*started_at).as_millis() as u64 > self.vote_timeout_ms {
                    vec![WPoaEvent::VoteTimeout {
                        current_round: self.round,
                    }]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    /// Transition any phase to ViewChanging.
    pub fn start_view_change(&mut self, new_view: u64) {
        self.phase = RoundPhase::ViewChanging {
            new_view,
            votes: HashSet::new(),
            vote_weight: 0,
            started_at: Instant::now(),
        };
    }

    /// Get the current phase name for logging.
    pub fn phase_name(&self) -> &'static str {
        match &self.phase {
            RoundPhase::Idle => "Idle",
            RoundPhase::Proposing { .. } => "Proposing",
            RoundPhase::Voting { .. } => "Voting",
            RoundPhase::Committed { .. } => "Committed",
            RoundPhase::ViewChanging { .. } => "ViewChanging",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_crypto::{PQSignature, SignatureType};

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    fn sig() -> PQSignature {
        PQSignature::new(SignatureType::Dilithium3, vec![1, 2, 3])
    }

    fn hash(n: u8) -> ShellHash {
        ShellHash::from_slice(&[n; 32])
    }

    fn uniform_weights(n: u8) -> HashMap<Address, u64> {
        (1..=n).map(|i| (addr(i), 1u64)).collect()
    }

    #[test]
    fn quorum_uniform_3_validators() {
        let weights = uniform_weights(3);
        let round = WPoaRound::new(1, 0, weights);
        assert_eq!(round.quorum_weight(), 2);
    }

    #[test]
    fn quorum_nonuniform_weights() {
        let mut weights = HashMap::new();
        weights.insert(addr(1), 3u64);
        weights.insert(addr(2), 2u64);
        weights.insert(addr(3), 1u64);
        let round = WPoaRound::new(1, 0, weights);
        assert_eq!(round.quorum_weight(), 4);
    }

    #[test]
    fn propose_transitions_idle_to_voting() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        let events = round.on_block_proposed(hash(1), addr(1));
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], WPoaEvent::ProposeAccepted { .. }));
        assert!(matches!(events[1], WPoaEvent::VoteNeeded { .. }));
        assert_eq!(round.phase_name(), "Voting");
    }

    #[test]
    fn propose_ignored_when_not_idle() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.on_block_proposed(hash(1), addr(1));
        let events = round.on_block_proposed(hash(2), addr(1));
        assert!(events.is_empty());
    }

    #[test]
    fn vote_commits_at_quorum_uniform() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.on_block_proposed(hash(1), addr(1));

        let e1 = round.on_vote(addr(1), hash(1), sig());
        assert!(e1.is_empty());

        let e2 = round.on_vote(addr(2), hash(1), sig());
        assert_eq!(e2.len(), 1);
        assert!(matches!(e2[0], WPoaEvent::BlockCommitted { .. }));
        if let WPoaEvent::BlockCommitted {
            quorum_signatures, ..
        } = &e2[0]
        {
            assert_eq!(quorum_signatures.len(), 2);
        }
        assert_eq!(round.phase_name(), "Committed");
    }

    #[test]
    fn vote_commits_at_quorum_nonuniform() {
        let mut weights = HashMap::new();
        weights.insert(addr(1), 3u64);
        weights.insert(addr(2), 2u64);
        weights.insert(addr(3), 1u64);
        let mut round = WPoaRound::new(1, 0, weights);
        round.on_block_proposed(hash(1), addr(1));

        let e1 = round.on_vote(addr(1), hash(1), sig());
        assert!(e1.is_empty());

        let e2 = round.on_vote(addr(2), hash(1), sig());
        assert!(matches!(e2[0], WPoaEvent::BlockCommitted { .. }));
    }

    #[test]
    fn duplicate_vote_rejected() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.on_block_proposed(hash(1), addr(1));

        round.on_vote(addr(1), hash(1), sig());
        let events = round.on_vote(addr(1), hash(1), sig());
        assert!(matches!(events[0], WPoaEvent::DuplicateVote { voter } if voter == addr(1)));
    }

    #[test]
    fn wrong_block_hash_rejected() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.on_block_proposed(hash(1), addr(1));

        let events = round.on_vote(addr(1), hash(99), sig());
        assert!(
            matches!(events[0], WPoaEvent::WrongBlockHash { expected, got }
            if expected == hash(1) && got == hash(99))
        );
        assert_eq!(round.phase_name(), "Voting");
    }

    #[test]
    fn unknown_voter_ignored() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.on_block_proposed(hash(1), addr(1));

        let events = round.on_vote(addr(99), hash(1), sig());
        assert!(events.is_empty());
    }

    #[test]
    fn view_change_quorum() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.start_view_change(1);
        assert_eq!(round.phase_name(), "ViewChanging");

        round.on_view_change_vote(addr(1), 1);
        let events = round.on_view_change_vote(addr(2), 1);
        assert!(matches!(
            events[0],
            WPoaEvent::ViewChangeReady { new_view: 1 }
        ));
    }

    #[test]
    fn view_change_wrong_view_ignored() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.start_view_change(1);

        let events = round.on_view_change_vote(addr(1), 2);
        assert!(events.is_empty());
        assert_eq!(round.phase_name(), "ViewChanging");
    }

    #[test]
    fn tick_propose_timeout() {
        use std::time::Duration;
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.propose_timeout_ms = 0;
        round.phase = RoundPhase::Proposing {
            block_hash: hash(1),
            proposed_at: Instant::now() - Duration::from_millis(100),
        };

        let events = round.tick(Instant::now());
        assert!(matches!(
            events[0],
            WPoaEvent::ProposeTimeout { current_round: 0 }
        ));
    }

    #[test]
    fn tick_vote_timeout() {
        use std::time::Duration;
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.vote_timeout_ms = 0;
        round.on_block_proposed(hash(1), addr(1));
        if let RoundPhase::Voting {
            ref mut started_at, ..
        } = round.phase
        {
            *started_at = Instant::now() - Duration::from_millis(100);
        }

        let events = round.tick(Instant::now());
        assert!(matches!(
            events[0],
            WPoaEvent::VoteTimeout { current_round: 0 }
        ));
    }

    #[test]
    fn tick_idle_no_events() {
        let weights = uniform_weights(3);
        let round = WPoaRound::new(1, 0, weights);
        assert!(round.tick(Instant::now()).is_empty());
    }

    #[test]
    fn full_propose_vote_commit_flow() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(10, 0, weights);

        let events = round.on_block_proposed(hash(42), addr(1));
        assert_eq!(events.len(), 2);

        let v1 = round.on_vote(addr(1), hash(42), sig());
        assert!(v1.is_empty());

        let v2 = round.on_vote(addr(2), hash(42), sig());
        assert_eq!(v2.len(), 1);
        let committed_hash = match &v2[0] {
            WPoaEvent::BlockCommitted {
                block_hash,
                quorum_signatures,
            } => {
                assert_eq!(quorum_signatures.len(), 2);
                *block_hash
            }
            other => panic!("expected BlockCommitted, got {:?}", other),
        };
        assert_eq!(committed_hash, hash(42));
    }

    #[test]
    fn start_view_change_from_voting() {
        let weights = uniform_weights(3);
        let mut round = WPoaRound::new(1, 0, weights);
        round.on_block_proposed(hash(1), addr(1));
        assert_eq!(round.phase_name(), "Voting");

        round.start_view_change(1);
        assert_eq!(round.phase_name(), "ViewChanging");
    }

    #[test]
    fn single_validator_commits_on_own_vote() {
        let mut weights = HashMap::new();
        weights.insert(addr(1), 1u64);
        let mut round = WPoaRound::new(1, 0, weights);
        round.on_block_proposed(hash(1), addr(1));

        let events = round.on_vote(addr(1), hash(1), sig());
        assert!(matches!(events[0], WPoaEvent::BlockCommitted { .. }));
    }
}
