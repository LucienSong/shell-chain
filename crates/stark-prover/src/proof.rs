//! Serializable wrapper for STARK proofs produced by the signature batch
//! commitment circuit.
//!
//! [`SigBatchProof`] bundles the Winterfell [`Proof`] bytes with the
//! `batch_root` and `n_sigs` needed to verify it.  This is what gets stored
//! in `BlockHeader::sig_aggregate_proof` as a full proof (when available).
//!
//! For the compact commitment stored in the block header at production time,
//! only `batch_root_bytes` (32 bytes) and `n_sigs` are required; the full
//! `proof_bytes` are populated later and gossiped as a `ProofAmendment`.

use serde::{Deserialize, Serialize};
use winterfell::Proof;

/// Current serialization version tag.
pub const SIG_BATCH_PROOF_VERSION: u8 = 2;

/// Serializable aggregate proof for a block's signature batch.
///
/// Contains everything a verifier needs:
/// - `batch_root`: the 32-byte (256-bit) Merkle-accumulator root.
/// - `n_sigs`: number of signatures included.
/// - `proof_bytes`: Winterfell [`Proof`] serialized via its own codec
///   (empty when only the compact commitment is stored in the header).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigBatchProof {
    /// Protocol version (always [`SIG_BATCH_PROOF_VERSION`]).
    pub version: u8,
    /// Final accumulator root as 32 little-endian bytes (256-bit).
    pub batch_root_bytes: [u8; 32],
    /// Number of signatures covered by this proof.
    pub n_sigs: usize,
    /// Raw Winterfell proof bytes (empty for commitment-only payloads).
    pub proof_bytes: Vec<u8>,
}

impl SigBatchProof {
    /// Serialise to JSON bytes.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Deserialise from JSON bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Build a commitment-only payload (no STARK proof bytes).
    ///
    /// Use this to populate `BlockHeader::sig_aggregate_proof` immediately
    /// during block production, before async STARK proving completes.
    pub fn commitment_only(batch_root_bytes: [u8; 32], n_sigs: usize) -> Self {
        Self {
            version: SIG_BATCH_PROOF_VERSION,
            batch_root_bytes,
            n_sigs,
            proof_bytes: Vec::new(),
        }
    }

    /// Returns true if this carries a full STARK proof (not just a commitment).
    pub fn has_proof(&self) -> bool {
        !self.proof_bytes.is_empty()
    }

    /// Wrap a raw Winterfell proof.
    pub fn from_proof(proof: Proof, batch_root_bytes: [u8; 32], n_sigs: usize) -> Self {
        let proof_bytes = proof.to_bytes();
        Self {
            version: SIG_BATCH_PROOF_VERSION,
            batch_root_bytes,
            n_sigs,
            proof_bytes,
        }
    }

    /// Attempt to deserialise the inner Winterfell proof.
    pub fn inner_proof(&self) -> Result<Proof, String> {
        Proof::from_bytes(&self.proof_bytes).map_err(|e| format!("proof decode: {:?}", e))
    }

    /// Estimated proof size in bytes.
    pub fn size_bytes(&self) -> usize {
        self.proof_bytes.len()
    }
}
