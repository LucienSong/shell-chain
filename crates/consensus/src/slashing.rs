//! Slashing evidence detection for wPoA.
//!
//! Provides detection of two categories of validator misbehaviour:
//!
//! 1. **Double-sign**: the same proposer sealed two different blocks at the same height.
//! 2. **Offline**: a proposer has not produced a block within `threshold` blocks of
//!    their expected slot, as detected by consecutive-missed-slot counting.
//!
//! # I1: Equivocation propagation
//!
//! When a double-sign is detected during `import_block`, an [`EquivocationProof`]
//! is broadcast to the network via `NetworkMessage::EquivocationEvidence`.
//! Receiving nodes independently verify the proof and apply slashing.

use serde::{Deserialize, Serialize};
use shell_core::BlockHeader;
use shell_primitives::{Address, ShellHash};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// I1: A broadcastable equivocation proof bundle.
///
/// Sent by the node that first detects a double-sign. Peers independently
/// verify the two conflicting headers before applying slashing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquivocationProof {
    /// The misbehaving validator's address.
    pub offender: Address,
    /// First conflicting block header (sealed by `offender`).
    pub header_a: Box<BlockHeader>,
    /// Second conflicting block header (sealed by `offender`, same height, different hash).
    pub header_b: Box<BlockHeader>,
    /// Hash of `header_a`.
    pub hash_a: ShellHash,
    /// Hash of `header_b`.
    pub hash_b: ShellHash,
}

impl EquivocationProof {
    /// Construct from a `SlashRecord` with `DoubleSign` evidence.
    ///
    /// Returns `None` if the slash record is not a double-sign or the hashes match.
    pub fn from_slash_record(record: &SlashRecord) -> Option<Self> {
        if let SlashEvidence::DoubleSign { header_a, header_b } = &record.evidence {
            let hash_a = header_a.hash();
            let hash_b = header_b.hash();
            if hash_a == hash_b {
                return None;
            }
            Some(Self {
                offender: record.validator,
                header_a: header_a.clone(),
                header_b: header_b.clone(),
                hash_a,
                hash_b,
            })
        } else {
            None
        }
    }

    /// Verify the equivocation proof is internally consistent:
    /// - Both headers have the same block number.
    /// - Both headers have the same proposer (matching `offender`).
    /// - The two hashes are different.
    pub fn verify(&self) -> bool {
        if self.header_a.number != self.header_b.number {
            return false;
        }
        if self.header_a.proposer != self.offender || self.header_b.proposer != self.offender {
            return false;
        }
        let computed_a = self.header_a.hash();
        let computed_b = self.header_b.hash();
        computed_a == self.hash_a && computed_b == self.hash_b && self.hash_a != self.hash_b
    }
}

/// Category of misbehaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlashType {
    /// Proposer signed two conflicting blocks at the same height.
    DoubleSign,
    /// Proposer has been absent for more than the configured threshold.
    Offline,
}

/// Evidence bundle attached to a `SlashRecord`.
#[derive(Debug, Clone)]
pub enum SlashEvidence {
    /// Two headers from the same proposer at the same block number with different hashes.
    DoubleSign {
        header_a: Box<BlockHeader>,
        header_b: Box<BlockHeader>,
    },
    /// Proposer last produced a block at `last_block`; currently at `current_block`.
    Offline { last_block: u64, current_block: u64 },
}

/// A finalized slashing record ready for on-chain submission.
#[derive(Debug, Clone)]
pub struct SlashRecord {
    /// The misbehaving validator's address.
    pub validator: Address,
    /// Type of misbehaviour.
    pub slash_type: SlashType,
    /// Block number at which the misbehaviour was detected.
    pub block_number: u64,
    /// Supporting evidence.
    pub evidence: SlashEvidence,
}

/// Chain-level slashing policy (configured in genesis).
#[derive(Debug, Clone)]
pub struct SlashingConfig {
    /// Fraction of stake to slash for double-sign, in basis points (0-10000).
    /// Default: 1000 (10 %).
    pub double_sign_slash_bps: u32,
    /// Fraction of stake to slash for offline misbehaviour, in basis points.
    /// Default: 100 (1 %).
    pub offline_slash_bps: u32,
    /// Number of consecutive blocks a validator must miss before being considered
    /// offline. Default: 50.
    pub offline_window_blocks: u64,
}

impl Default for SlashingConfig {
    fn default() -> Self {
        Self {
            double_sign_slash_bps: 1_000, // 10 %
            offline_slash_bps: 100,       // 1 %
            offline_window_blocks: 50,
        }
    }
}

impl SlashingConfig {
    /// Validate that basis-point values are within range.
    pub fn validate(&self) -> Result<(), String> {
        if self.double_sign_slash_bps > 10_000 {
            return Err(format!(
                "double_sign_slash_bps {} > 10000",
                self.double_sign_slash_bps
            ));
        }
        if self.offline_slash_bps > 10_000 {
            return Err(format!(
                "offline_slash_bps {} > 10000",
                self.offline_slash_bps
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Detection functions
// ---------------------------------------------------------------------------

/// Detect double-sign evidence in two block headers.
///
/// Returns `Some(SlashRecord)` when:
///   - Both headers have the same `number` (block height).
///   - Both headers have the same `proposer` address.
///   - The two header hashes are **different** (distinct conflicting blocks).
///
/// Returns `None` if the headers are identical or do not share a proposer+height.
pub fn detect_double_sign(h1: &BlockHeader, h2: &BlockHeader) -> Option<SlashRecord> {
    if h1.number != h2.number {
        return None;
    }
    if h1.proposer != h2.proposer {
        return None;
    }
    let hash1 = h1.hash();
    let hash2 = h2.hash();
    if hash1 == hash2 {
        // Identical headers — no evidence.
        return None;
    }
    Some(SlashRecord {
        validator: h1.proposer,
        slash_type: SlashType::DoubleSign,
        block_number: h1.number,
        evidence: SlashEvidence::DoubleSign {
            header_a: Box::new(h1.clone()),
            header_b: Box::new(h2.clone()),
        },
    })
}

/// Detect an offline validator.
///
/// Returns `Some(SlashRecord)` when `current_block - last_proposed_block`
/// exceeds `config.offline_window_blocks`.
///
/// Callers should invoke this once per epoch (or on a fixed cadence) for
/// every active validator.
pub fn detect_offline(
    validator: &Address,
    last_proposed_block: u64,
    current_block: u64,
    config: &SlashingConfig,
) -> Option<SlashRecord> {
    if current_block <= last_proposed_block {
        return None;
    }
    let gap = current_block.saturating_sub(last_proposed_block);
    if gap <= config.offline_window_blocks {
        return None;
    }
    Some(SlashRecord {
        validator: *validator,
        slash_type: SlashType::Offline,
        block_number: current_block,
        evidence: SlashEvidence::Offline {
            last_block: last_proposed_block,
            current_block,
        },
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shell_core::BlockHeader;
    use shell_primitives::{Address, Bytes, ShellHash};

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    fn header(number: u64, proposer: Address, nonce: u64) -> BlockHeader {
        BlockHeader {
            number,
            proposer,
            parent_hash: ShellHash::ZERO,
            timestamp: 1_000_000 + nonce,
            transactions_root: ShellHash::ZERO,
            state_root: ShellHash::ZERO,
            receipts_root: ShellHash::ZERO,
            gas_limit: 30_000_000,
            gas_used: 0,
            base_fee_per_gas: 0u64,
            extra_data: Bytes::new(),
            logs_bloom: Bytes::new(),
            sig_aggregate_proof: None,
            withdrawals_root: ShellHash::ZERO,
            parent_beacon_block_root: ShellHash::ZERO,
            blob_gas_used: 0,
            excess_blob_gas: 0,
            witness_root: None,
        }
    }

    #[test]
    fn double_sign_detected() {
        let h1 = header(10, addr(1), 0);
        let h2 = header(10, addr(1), 99); // same height + proposer, different content
        let record = detect_double_sign(&h1, &h2).expect("should detect double sign");
        assert_eq!(record.validator, addr(1));
        assert_eq!(record.slash_type, SlashType::DoubleSign);
        assert_eq!(record.block_number, 10);
    }

    #[test]
    fn double_sign_identical_headers_ignored() {
        let h = header(5, addr(1), 0);
        assert!(detect_double_sign(&h, &h).is_none());
    }

    #[test]
    fn double_sign_different_heights_ignored() {
        let h1 = header(5, addr(1), 0);
        let h2 = header(6, addr(1), 0);
        assert!(detect_double_sign(&h1, &h2).is_none());
    }

    #[test]
    fn double_sign_different_proposers_ignored() {
        let h1 = header(5, addr(1), 0);
        let h2 = header(5, addr(2), 0);
        assert!(detect_double_sign(&h1, &h2).is_none());
    }

    #[test]
    fn offline_detected_over_threshold() {
        let config = SlashingConfig {
            offline_window_blocks: 50,
            ..SlashingConfig::default()
        };
        let record = detect_offline(&addr(2), 100, 151, &config).expect("should detect offline");
        assert_eq!(record.validator, addr(2));
        assert_eq!(record.slash_type, SlashType::Offline);
    }

    #[test]
    fn offline_within_window_not_detected() {
        let config = SlashingConfig {
            offline_window_blocks: 50,
            ..SlashingConfig::default()
        };
        assert!(detect_offline(&addr(2), 100, 149, &config).is_none());
        assert!(detect_offline(&addr(2), 100, 150, &config).is_none());
    }

    #[test]
    fn slashing_config_validates_bps_range() {
        let bad = SlashingConfig {
            double_sign_slash_bps: 10_001,
            ..SlashingConfig::default()
        };
        assert!(bad.validate().is_err());
        let good = SlashingConfig::default();
        assert!(good.validate().is_ok());
    }

    // -----------------------------------------------------------------------
    // I1: EquivocationProof tests
    // -----------------------------------------------------------------------

    #[test]
    fn equivocation_proof_from_double_sign_slash_record() {
        let h1 = header(10, addr(1), 0);
        let h2 = header(10, addr(1), 99);
        let record = detect_double_sign(&h1, &h2).unwrap();
        let eq = EquivocationProof::from_slash_record(&record).expect("should build equivocation");
        assert_eq!(eq.offender, addr(1));
        assert_eq!(eq.header_a.number, 10);
        assert_eq!(eq.header_b.number, 10);
        assert_ne!(eq.hash_a, eq.hash_b);
    }

    #[test]
    fn equivocation_proof_verify_valid() {
        let h1 = header(10, addr(1), 0);
        let h2 = header(10, addr(1), 99);
        let record = detect_double_sign(&h1, &h2).unwrap();
        let eq = EquivocationProof::from_slash_record(&record).unwrap();
        assert!(eq.verify(), "valid equivocation should verify");
    }

    #[test]
    fn equivocation_proof_verify_tampered_hash_rejected() {
        let h1 = header(10, addr(1), 0);
        let h2 = header(10, addr(1), 99);
        let record = detect_double_sign(&h1, &h2).unwrap();
        let mut eq = EquivocationProof::from_slash_record(&record).unwrap();
        // Tamper: replace hash_a with hash_b (makes hash_a == hash_b).
        eq.hash_a = eq.hash_b;
        assert!(!eq.verify(), "tampered hash should fail verify");
    }

    #[test]
    fn equivocation_proof_verify_wrong_proposer_rejected() {
        let h1 = header(10, addr(1), 0);
        let h2 = header(10, addr(1), 99);
        let record = detect_double_sign(&h1, &h2).unwrap();
        let mut eq = EquivocationProof::from_slash_record(&record).unwrap();
        // Tamper: claim a different offender.
        eq.offender = addr(2);
        assert!(!eq.verify(), "wrong offender should fail verify");
    }

    #[test]
    fn equivocation_proof_from_offline_slash_returns_none() {
        let config = SlashingConfig::default();
        let record = detect_offline(&addr(3), 0, 100, &config).unwrap();
        assert!(
            EquivocationProof::from_slash_record(&record).is_none(),
            "offline slash record should not produce equivocation proof"
        );
    }
}
