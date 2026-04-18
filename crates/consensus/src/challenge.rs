//! I2: Proof validity challenge mechanism.
//!
//! When a node receives a `ProofAmendment` that it cannot independently verify,
//! it may broadcast a `ProofChallenge` to the network. Other nodes that hold the
//! original STARK proof can respond with a `ChallengeResponse`.
//!
//! # Protocol sketch
//!
//! 1. Verifier receives `ProofAmendment { block_hash, payload }`.
//! 2. Verifier fails local `verify_sig_batch()` on the payload.
//! 3. Verifier broadcasts `ProofChallenge { block_hash, reason, challenger }`.
//! 4. Any node holding the raw proof re-broadcasts `ChallengeResponse { block_hash, proof_bytes }`.
//! 5. Original submitter can re-submit a corrected proof.
//!
//! Challenges are rate-limited (see `I3: rate_limiter`) to prevent DoS.

use serde::{Deserialize, Serialize};
use shell_primitives::{Address, ShellHash};

/// Reason a proof amendment is being challenged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChallengeReason {
    /// The STARK proof bytes failed local verification.
    VerificationFailed,
    /// The proof covers a block number that doesn't match our canonical chain.
    BlockNotFound,
    /// The proof payload is malformed (deserialisation error).
    MalformedPayload,
    /// The proof covers a block whose state root differs from our local view.
    StateRootMismatch,
}

impl std::fmt::Display for ChallengeReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VerificationFailed => write!(f, "verification-failed"),
            Self::BlockNotFound => write!(f, "block-not-found"),
            Self::MalformedPayload => write!(f, "malformed-payload"),
            Self::StateRootMismatch => write!(f, "state-root-mismatch"),
        }
    }
}

/// I2: A challenge broadcast when a `ProofAmendment` cannot be verified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofChallenge {
    /// Hash of the block whose proof is being challenged.
    pub block_hash: ShellHash,
    /// Block number (for logging and routing).
    pub block_number: u64,
    /// Why the proof was rejected.
    pub reason: ChallengeReason,
    /// Address of the node raising the challenge.
    pub challenger: Address,
    /// Monotonic challenge sequence number (from the challenger's rate limiter).
    /// Allows recipients to deduplicate and detect replay.
    pub sequence: u64,
}

impl ProofChallenge {
    /// Create a new challenge.
    pub fn new(
        block_hash: ShellHash,
        block_number: u64,
        reason: ChallengeReason,
        challenger: Address,
        sequence: u64,
    ) -> Self {
        Self {
            block_hash,
            block_number,
            reason,
            challenger,
            sequence,
        }
    }
}

/// I2: Response to a `ProofChallenge` — provides the raw proof bytes for re-verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeResponse {
    /// Hash of the challenged block.
    pub block_hash: ShellHash,
    /// Raw proof payload bytes (same format as `ProofAmendment::payload`).
    pub proof_bytes: Vec<u8>,
    /// Address of the responding node.
    pub responder: Address,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::{Address, ShellHash};

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    fn hash(n: u8) -> ShellHash {
        ShellHash::from([n; 32])
    }

    #[test]
    fn proof_challenge_round_trip_serialization() {
        let challenge =
            ProofChallenge::new(hash(1), 42, ChallengeReason::VerificationFailed, addr(2), 7);
        let json = serde_json::to_string(&challenge).unwrap();
        let decoded: ProofChallenge = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.block_hash, challenge.block_hash);
        assert_eq!(decoded.block_number, 42);
        assert_eq!(decoded.reason, ChallengeReason::VerificationFailed);
        assert_eq!(decoded.challenger, addr(2));
        assert_eq!(decoded.sequence, 7);
    }

    #[test]
    fn challenge_response_round_trip_serialization() {
        let resp = ChallengeResponse {
            block_hash: hash(3),
            proof_bytes: vec![1, 2, 3, 4],
            responder: addr(5),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: ChallengeResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.block_hash, resp.block_hash);
        assert_eq!(decoded.proof_bytes, vec![1, 2, 3, 4]);
        assert_eq!(decoded.responder, addr(5));
    }

    #[test]
    fn challenge_reason_display() {
        assert_eq!(
            ChallengeReason::VerificationFailed.to_string(),
            "verification-failed"
        );
        assert_eq!(
            ChallengeReason::BlockNotFound.to_string(),
            "block-not-found"
        );
        assert_eq!(
            ChallengeReason::MalformedPayload.to_string(),
            "malformed-payload"
        );
        assert_eq!(
            ChallengeReason::StateRootMismatch.to_string(),
            "state-root-mismatch"
        );
    }

    #[test]
    fn proof_challenge_all_reasons() {
        for reason in [
            ChallengeReason::VerificationFailed,
            ChallengeReason::BlockNotFound,
            ChallengeReason::MalformedPayload,
            ChallengeReason::StateRootMismatch,
        ] {
            let c = ProofChallenge::new(hash(0), 0, reason.clone(), addr(0), 0);
            assert_eq!(c.reason, reason);
        }
    }
}
