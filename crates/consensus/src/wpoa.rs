//! Weighted Proof-of-Authority (wPoA) consensus engine.
//!
//! Extends the basic PoA round-robin with per-validator weights so that
//! validators with a higher stake/reputation are elected to propose more
//! blocks proportionally.
//!
//! Proposer selection uses **weighted round-robin**:
//!   - `total_weight` = sum of active validator weights.
//!   - `slot = block_number % total_weight`.
//!   - The validator whose cumulative-weight window contains `slot` is elected.
//!
//! For signature verification and block sealing, `WPoaEngine` delegates to
//! the existing `PoaEngine` logic.

use std::sync::Arc;

use async_trait::async_trait;
use shell_core::{Block, BlockHeader};
use shell_crypto::{Signer, Verifier};
use shell_primitives::Address;

use crate::poa::PoaEngine;
use crate::validator::{ValidatorSet, ValidatorSetConfig};
use crate::{ConsensusEngine, ConsensusError, EngineType, PoaConfig};

/// Configuration for the weighted PoA engine.
#[derive(Debug, Clone)]
pub struct WPoaConfig {
    /// Base PoA configuration (authority list, block time, etc.).
    pub poa: PoaConfig,
    /// Initial validator weights indexed by position in `poa.authorities`.
    ///
    /// If shorter than `authorities`, missing entries default to weight 1.
    pub weights: Vec<u64>,
    /// Validator set lifecycle parameters.
    pub validator_set_config: ValidatorSetConfig,
}

impl WPoaConfig {
    /// Create a `WPoaConfig` from a `PoaConfig` with uniform weights.
    pub fn from_poa(poa: PoaConfig) -> Self {
        let n = poa.authorities.len();
        Self {
            poa,
            weights: vec![1u64; n],
            validator_set_config: ValidatorSetConfig::default(),
        }
    }

    /// Create a `WPoaConfig` with explicit per-validator weights.
    ///
    /// `weights` is aligned with `poa.authorities` by index. Missing entries
    /// default to weight 1.
    pub fn with_weights(poa: PoaConfig, weights: Vec<u64>) -> Self {
        Self {
            weights,
            poa,
            validator_set_config: ValidatorSetConfig::default(),
        }
    }
}

/// Weighted Proof-of-Authority consensus engine.
///
/// Delegates seal verification to `PoaEngine` and overrides proposer
/// selection with weighted round-robin via `ValidatorSet`.
pub struct WPoaEngine {
    inner: PoaEngine,
    validator_set: ValidatorSet,
    #[allow(dead_code)]
    verifier: Arc<dyn Verifier>,
    signer: Option<Arc<dyn Signer>>,
}

impl WPoaEngine {
    /// Construct a `WPoaEngine` from a `WPoaConfig`.
    pub fn new(config: WPoaConfig, verifier: Arc<dyn Verifier>) -> Self {
        let entries = config
            .poa
            .authorities
            .iter()
            .enumerate()
            .map(|(i, addr)| (*addr, config.weights.get(i).copied().unwrap_or(1)));

        let validator_set =
            ValidatorSet::from_genesis(entries, config.validator_set_config.clone());

        Self {
            inner: PoaEngine::new(config.poa),
            validator_set,
            verifier,
            signer: None,
        }
    }

    /// Attach a signer so this engine can seal blocks.
    pub fn with_signer(mut self, signer: Arc<dyn Signer>) -> Self {
        self.signer = Some(signer);
        self
    }

    /// Access the underlying `ValidatorSet`.
    pub fn validator_set(&self) -> &ValidatorSet {
        &self.validator_set
    }

    /// Mutable access to the `ValidatorSet` (for epoch boundary updates).
    pub fn validator_set_mut(&mut self) -> &mut ValidatorSet {
        &mut self.validator_set
    }

    /// Return the expected proposer for `block_number` using weighted round-robin.
    ///
    /// Falls back to the unweighted `PoaEngine` selection if the validator set
    /// is empty (should not occur in a live network).
    pub fn proposer_for_block(&self, block_number: u64) -> Address {
        self.validator_set
            .weighted_proposer(block_number)
            .unwrap_or_else(|| self.inner.config().proposer_for_block(block_number))
    }
}

#[async_trait]
impl ConsensusEngine for WPoaEngine {
    fn verify_header(&self, header: &BlockHeader) -> Result<(), ConsensusError> {
        // Check proposer is in the active set.
        if !self.validator_set.is_active(&header.proposer) {
            return Err(ConsensusError::UnknownProposer(header.proposer));
        }

        // Check weighted proposer assignment.
        let expected = self.proposer_for_block(header.number);
        if header.proposer != expected {
            return Err(ConsensusError::InvalidProposer {
                expected,
                got: header.proposer,
            });
        }

        // NOTE: `verify_header` only receives a `BlockHeader`, which does not
        // carry the proposer seal (`Block::proposer_seal`). Full PQ-signature
        // seal verification requires both the header hash and the seal bytes
        // from the enclosing `Block`, as well as a public-key lookup against
        // `ChainStore`. That verification is the responsibility of the block
        // import pipeline (e.g. `verify_header_with_parent` / `import_block`),
        // not this method.
        //
        // This method intentionally limits itself to the structural checks that
        // can be performed without ChainStore access:
        //   1. Proposer is in the active validator set (checked above).
        //   2. Proposer matches the weighted round-robin slot (checked above).
        Ok(())
    }

    async fn seal_block(&self, block: &mut Block) -> Result<(), ConsensusError> {
        let signer = self.signer.as_ref().ok_or(ConsensusError::NoSigner)?;

        let expected = self.proposer_for_block(block.header.number);
        if block.header.proposer != expected {
            return Err(ConsensusError::InvalidProposer {
                expected,
                got: block.header.proposer,
            });
        }

        let header_hash = block.header.hash();
        let seal = signer
            .sign(header_hash.as_bytes())
            .map_err(|e| ConsensusError::SigningError(e.to_string()))?;
        block.proposer_seal = Some(seal);
        Ok(())
    }

    fn is_proposer(&self, block_number: u64, address: &Address) -> bool {
        self.proposer_for_block(block_number) == *address
    }

    fn engine_type(&self) -> EngineType {
        EngineType::WPoA
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poa::PoaConfig;
    use shell_crypto::{PQSignature, SignatureType};

    fn addr(n: u8) -> Address {
        Address::from([n; 20])
    }

    struct MockVerifier;
    impl Verifier for MockVerifier {
        fn verify(
            &self,
            _pk: &[u8],
            _msg: &[u8],
            _sig: &PQSignature,
        ) -> Result<bool, shell_crypto::CryptoError> {
            Ok(true)
        }

        fn sig_type(&self) -> SignatureType {
            SignatureType::Dilithium3
        }
    }

    fn engine(authorities: Vec<Address>, weights: Vec<u64>) -> WPoaEngine {
        let poa = PoaConfig::new(authorities, 2);
        let config = WPoaConfig::with_weights(poa, weights);
        WPoaEngine::new(config, Arc::new(MockVerifier))
    }

    #[test]
    fn proposer_uniform_weights() {
        let e = engine(vec![addr(1), addr(2), addr(3)], vec![1, 1, 1]);
        assert_eq!(e.proposer_for_block(0), addr(1));
        assert_eq!(e.proposer_for_block(1), addr(2));
        assert_eq!(e.proposer_for_block(2), addr(3));
        assert_eq!(e.proposer_for_block(3), addr(1));
    }

    #[test]
    fn proposer_non_uniform_weights() {
        // A:2, B:1 → A gets blocks 0,1; B gets block 2; wraps
        let e = engine(vec![addr(1), addr(2)], vec![2, 1]);
        assert_eq!(e.proposer_for_block(0), addr(1));
        assert_eq!(e.proposer_for_block(1), addr(1));
        assert_eq!(e.proposer_for_block(2), addr(2));
        assert_eq!(e.proposer_for_block(3), addr(1));
    }

    #[test]
    fn is_proposer_returns_correct_result() {
        let e = engine(vec![addr(1), addr(2)], vec![1, 1]);
        assert!(e.is_proposer(0, &addr(1)));
        assert!(!e.is_proposer(0, &addr(2)));
        assert!(e.is_proposer(1, &addr(2)));
    }

    #[test]
    fn engine_type_is_wpoa() {
        let e = engine(vec![addr(1)], vec![1]);
        assert_eq!(e.engine_type(), EngineType::WPoA);
    }
}
