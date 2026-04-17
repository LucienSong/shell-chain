//! ProofAmendment — a STARK proof attached to an already-sealed block.
//!
//! When async proving is enabled, blocks are broadcast immediately without a
//! proof.  After the prover service generates the proof (potentially on a
//! separate node), it wraps it in a [`ProofAmendment`] and propagates it via
//! P2P gossip.  Peers store the amendment alongside the block so that future
//! importers can verify without re-running native signature checks.

use serde::{Deserialize, Serialize};
use shell_primitives::{Address, Bytes, ShellHash};

use crate::proof::SigBatchProof;

// ── ProofAmendment ────────────────────────────────────────────────────────────

/// A STARK proof generated asynchronously and attached to a sealed block.
///
/// The amendment is self-contained: it carries everything a verifier needs —
/// the target block identity, the proof, and the prover's cryptographic
/// signature (preventing forgeries by nodes that did not actually run the
/// prover).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofAmendment {
    /// Protocol version for forward-compatibility.
    pub version: u8,
    /// Hash of the block this proof covers.
    pub block_hash: ShellHash,
    /// Height of the block this proof covers (redundant with hash but
    /// allows cheap range queries without deserializing the proof).
    pub block_number: u64,
    /// The STARK batch-commitment proof.
    pub proof: SigBatchProof,
    /// The prover's address (registered in ProverRegistry).
    pub prover: Address,
    /// Raw serialized PQ signature over `(block_hash ‖ block_number ‖ proof_commitment)`.
    ///
    /// The exact message is the SHA3-256 of:
    ///   `b"proof-amendment" ‖ block_hash.as_bytes() ‖ block_number.to_le_bytes() ‖ proof.batch_root_bytes`
    pub prover_signature: Bytes,
}

/// Current serialization version.
pub const PROOF_AMENDMENT_VERSION: u8 = 1;

impl ProofAmendment {
    /// Serialize to JSON bytes for P2P transmission or storage.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Deserialize from JSON bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Compute the canonical signing message for this amendment.
    ///
    /// The prover must sign this message with their registered PQ key.
    /// Validators verify the signature before accepting the amendment.
    pub fn signing_message(&self) -> Vec<u8> {
        let mut msg = b"proof-amendment".to_vec();
        msg.extend_from_slice(self.block_hash.as_bytes());
        msg.extend_from_slice(&self.block_number.to_le_bytes());
        msg.extend_from_slice(&self.proof.batch_root_bytes);
        msg
    }

    /// Estimated wire size in bytes.
    pub fn size_bytes(&self) -> usize {
        self.proof.size_bytes()
            + 32  // block_hash
            + 8   // block_number
            + 20  // prover address
            + self.prover_signature.len()
            + 16  // JSON overhead estimate
    }
}

// ── Storage key helpers ───────────────────────────────────────────────────────

/// Key prefix for proof amendments in the key-value store.
///
/// Full key: `AMENDMENT_PREFIX ‖ block_hash_bytes (32 bytes)`
pub const AMENDMENT_KEY_PREFIX: &[u8] = b"pa/";

/// Build a storage key for a proof amendment.
pub fn amendment_key(block_hash: &ShellHash) -> Vec<u8> {
    let mut key = AMENDMENT_KEY_PREFIX.to_vec();
    key.extend_from_slice(block_hash.as_bytes());
    key
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::ShellHash;

    fn make_amendment() -> ProofAmendment {
        use crate::prover::{prove_sig_batch, SigBatchEntry};
        let entries = vec![
            SigBatchEntry { msg_hash: [1u8; 32], pk_hash: [2u8; 32] },
            SigBatchEntry { msg_hash: [3u8; 32], pk_hash: [4u8; 32] },
        ];
        let proof = prove_sig_batch(&entries).expect("prove failed");
        ProofAmendment {
            version: PROOF_AMENDMENT_VERSION,
            block_hash: ShellHash::from([0xAA; 32]),
            block_number: 42,
            proof,
            prover: Address::from([0x01; 20]),
            prover_signature: Bytes::from(vec![0u8; 16]),
        }
    }

    #[test]
    fn amendment_json_roundtrip() {
        let a = make_amendment();
        let json = a.to_json().expect("serialize");
        let decoded = ProofAmendment::from_json(&json).expect("deserialize");
        assert_eq!(a, decoded);
    }

    #[test]
    fn amendment_signing_message_is_deterministic() {
        let a = make_amendment();
        assert_eq!(a.signing_message(), a.signing_message());
    }

    #[test]
    fn amendment_signing_message_includes_prefix() {
        let a = make_amendment();
        let msg = a.signing_message();
        assert!(msg.starts_with(b"proof-amendment"));
    }

    #[test]
    fn amendment_key_uses_prefix() {
        let hash = ShellHash::from([0xBB; 32]);
        let key = amendment_key(&hash);
        assert!(key.starts_with(AMENDMENT_KEY_PREFIX));
        assert_eq!(key.len(), AMENDMENT_KEY_PREFIX.len() + 32);
    }

    #[test]
    fn amendment_size_bytes_nonzero() {
        let a = make_amendment();
        assert!(a.size_bytes() > 0);
    }

    #[test]
    fn different_blocks_produce_different_keys() {
        let k1 = amendment_key(&ShellHash::from([0x01; 32]));
        let k2 = amendment_key(&ShellHash::from([0x02; 32]));
        assert_ne!(k1, k2);
    }
}
