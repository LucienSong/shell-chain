//! K2: Block proof state machine.
//!
//! Tracks the lifecycle of a block's proof from sealing to stripping:
//!
//! ```text
//! Sealed → Proving → Proven → Available → Stripped
//!        ↘                 ↗
//!          ProofUnavailable
//! ```
//!
//! - **Sealed**: Block is on-chain but no proof has been submitted yet.
//! - **Proving**: A prover has claimed the window (I4) and is working on it.
//! - **Proven**: A valid proof amendment has been accepted locally.
//! - **Available**: Proof has been replicated to `min_ack_count` peers (K1).
//! - **Stripped**: Old proof data has been pruned to reclaim storage.
//! - **ProofUnavailable**: The proof window expired without a valid submission.

use shell_primitives::ShellHash;
use std::collections::HashMap;

/// The state of a block's STARK proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockProofState {
    /// Block sealed; proof not yet started.
    Sealed,
    /// A prover has claimed the proof window and is proving.
    Proving { claimer: shell_primitives::Address },
    /// A valid proof amendment has been stored locally.
    Proven,
    /// Proof replicated to enough peers (K1 threshold met).
    Available,
    /// Proof data pruned from local storage.
    Stripped,
    /// Proof window expired without a valid proof.
    ProofUnavailable,
}

impl std::fmt::Display for BlockProofState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sealed => write!(f, "Sealed"),
            Self::Proving { claimer } => write!(f, "Proving({claimer})"),
            Self::Proven => write!(f, "Proven"),
            Self::Available => write!(f, "Available"),
            Self::Stripped => write!(f, "Stripped"),
            Self::ProofUnavailable => write!(f, "ProofUnavailable"),
        }
    }
}

/// Error from an invalid state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTransition {
    pub from: String,
    pub to: String,
}

impl std::fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid transition {} → {}", self.from, self.to)
    }
}

/// K2: Manages the per-block proof state machine.
#[derive(Debug)]
pub struct BlockStateMachine {
    states: HashMap<ShellHash, BlockProofState>,
}

impl BlockStateMachine {
    pub fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
    }

    /// Get the current state for a block (defaults to `Sealed` if unseen).
    pub fn state(&self, block_hash: &ShellHash) -> &BlockProofState {
        self.states
            .get(block_hash)
            .unwrap_or(&BlockProofState::Sealed)
    }

    /// Transition a block to `Proving`.
    pub fn start_proving(
        &mut self,
        block_hash: ShellHash,
        claimer: shell_primitives::Address,
    ) -> Result<(), InvalidTransition> {
        match self
            .states
            .get(&block_hash)
            .unwrap_or(&BlockProofState::Sealed)
        {
            BlockProofState::Sealed => {
                self.states
                    .insert(block_hash, BlockProofState::Proving { claimer });
                Ok(())
            }
            other => Err(InvalidTransition {
                from: other.to_string(),
                to: format!("Proving({claimer})"),
            }),
        }
    }

    /// Transition a block to `Proven`.
    pub fn mark_proven(&mut self, block_hash: ShellHash) -> Result<(), InvalidTransition> {
        match self
            .states
            .get(&block_hash)
            .unwrap_or(&BlockProofState::Sealed)
        {
            BlockProofState::Sealed | BlockProofState::Proving { .. } => {
                self.states.insert(block_hash, BlockProofState::Proven);
                Ok(())
            }
            other => Err(InvalidTransition {
                from: other.to_string(),
                to: "Proven".to_string(),
            }),
        }
    }

    /// Transition a block to `Available`.
    pub fn mark_available(&mut self, block_hash: ShellHash) -> Result<(), InvalidTransition> {
        match self.states.get(&block_hash) {
            Some(BlockProofState::Proven) => {
                self.states.insert(block_hash, BlockProofState::Available);
                Ok(())
            }
            other => Err(InvalidTransition {
                from: other
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Sealed".to_string()),
                to: "Available".to_string(),
            }),
        }
    }

    /// Transition a block to `Stripped`.
    pub fn mark_stripped(&mut self, block_hash: ShellHash) -> Result<(), InvalidTransition> {
        match self.states.get(&block_hash) {
            Some(BlockProofState::Available) => {
                self.states.insert(block_hash, BlockProofState::Stripped);
                Ok(())
            }
            other => Err(InvalidTransition {
                from: other
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Sealed".to_string()),
                to: "Stripped".to_string(),
            }),
        }
    }

    /// Mark a block's proof as unavailable (window expired).
    pub fn mark_unavailable(&mut self, block_hash: ShellHash) {
        match self
            .states
            .get(&block_hash)
            .unwrap_or(&BlockProofState::Sealed)
        {
            BlockProofState::Proven | BlockProofState::Available | BlockProofState::Stripped => {}
            _ => {
                self.states
                    .insert(block_hash, BlockProofState::ProofUnavailable);
            }
        }
    }

    /// Number of tracked blocks.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

impl Default for BlockStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::{Address, ShellHash};

    fn hash(n: u8) -> ShellHash {
        ShellHash::from([n; 32])
    }
    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    #[test]
    fn default_state_is_sealed() {
        let sm = BlockStateMachine::new();
        assert_eq!(*sm.state(&hash(1)), BlockProofState::Sealed);
    }

    #[test]
    fn sealed_to_proving() {
        let mut sm = BlockStateMachine::new();
        sm.start_proving(hash(1), addr(1)).unwrap();
        assert!(matches!(
            sm.state(&hash(1)),
            BlockProofState::Proving { .. }
        ));
    }

    #[test]
    fn proving_to_proven() {
        let mut sm = BlockStateMachine::new();
        sm.start_proving(hash(1), addr(1)).unwrap();
        sm.mark_proven(hash(1)).unwrap();
        assert_eq!(*sm.state(&hash(1)), BlockProofState::Proven);
    }

    #[test]
    fn sealed_directly_to_proven() {
        let mut sm = BlockStateMachine::new();
        sm.mark_proven(hash(1)).unwrap(); // Sealed → Proven is allowed
        assert_eq!(*sm.state(&hash(1)), BlockProofState::Proven);
    }

    #[test]
    fn proven_to_available() {
        let mut sm = BlockStateMachine::new();
        sm.mark_proven(hash(1)).unwrap();
        sm.mark_available(hash(1)).unwrap();
        assert_eq!(*sm.state(&hash(1)), BlockProofState::Available);
    }

    #[test]
    fn available_to_stripped() {
        let mut sm = BlockStateMachine::new();
        sm.mark_proven(hash(1)).unwrap();
        sm.mark_available(hash(1)).unwrap();
        sm.mark_stripped(hash(1)).unwrap();
        assert_eq!(*sm.state(&hash(1)), BlockProofState::Stripped);
    }

    #[test]
    fn invalid_transition_proving_to_available() {
        let mut sm = BlockStateMachine::new();
        sm.start_proving(hash(1), addr(1)).unwrap();
        let err = sm.mark_available(hash(1)).unwrap_err();
        assert!(err.to.contains("Available"));
    }

    #[test]
    fn mark_unavailable_on_sealed() {
        let mut sm = BlockStateMachine::new();
        sm.mark_unavailable(hash(1));
        assert_eq!(*sm.state(&hash(1)), BlockProofState::ProofUnavailable);
    }

    #[test]
    fn mark_unavailable_does_not_override_proven() {
        let mut sm = BlockStateMachine::new();
        sm.mark_proven(hash(1)).unwrap();
        sm.mark_unavailable(hash(1)); // should be ignored
        assert_eq!(*sm.state(&hash(1)), BlockProofState::Proven);
    }
}
