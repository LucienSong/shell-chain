use shell_core::{Block, BlockHeader};
use shell_crypto::{PQSignature, Signer, Verifier};
use shell_primitives::Address;

use crate::{ConsensusEngine, ConsensusError, EngineType};

/// PoA configuration: authority list and block timing.
#[derive(Debug, Clone)]
pub struct PoaConfig {
    /// Ordered list of authority addresses. Position determines round-robin slot.
    pub authorities: Vec<Address>,
    /// Minimum seconds between consecutive blocks.
    pub block_time_secs: u64,
}

impl PoaConfig {
    pub fn new(authorities: Vec<Address>, block_time_secs: u64) -> Self {
        Self {
            authorities,
            block_time_secs,
        }
    }

    /// Return the expected proposer for a given block number.
    pub fn proposer_for_block(&self, block_number: u64) -> Address {
        let idx = block_number as usize % self.authorities.len();
        self.authorities[idx]
    }

    pub fn is_authority(&self, address: &Address) -> bool {
        self.authorities.contains(address)
    }
}

/// Proof-of-Authority consensus engine.
///
/// Round-robin proposer selection based on `block_number % authority_count`.
/// Each block must be sealed with the proposer's PQ signature.
pub struct PoaEngine {
    config: PoaConfig,
}

impl PoaEngine {
    pub fn new(config: PoaConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &PoaConfig {
        &self.config
    }

    fn verify_proposer(&self, header: &BlockHeader) -> Result<(), ConsensusError> {
        if !self.config.is_authority(&header.proposer) {
            return Err(ConsensusError::UnknownProposer(header.proposer));
        }

        let expected = self.config.proposer_for_block(header.number);
        if header.proposer != expected {
            return Err(ConsensusError::InvalidProposer {
                expected,
                got: header.proposer,
            });
        }
        Ok(())
    }

    fn verify_timestamp(
        &self,
        header: &BlockHeader,
        parent: Option<&BlockHeader>,
    ) -> Result<(), ConsensusError> {
        if let Some(parent) = parent {
            if header.timestamp < parent.timestamp + self.config.block_time_secs {
                return Err(ConsensusError::InvalidTimestamp(format!(
                    "block {} timestamp {} < parent {} + block_time {}",
                    header.number, header.timestamp, parent.timestamp, self.config.block_time_secs,
                )));
            }
            if header.number != parent.number + 1 {
                return Err(ConsensusError::InvalidTimestamp(format!(
                    "block number {} != parent {} + 1",
                    header.number, parent.number,
                )));
            }
            if header.parent_hash != parent.hash() {
                return Err(ConsensusError::Internal(
                    "parent_hash does not match parent header".into(),
                ));
            }
        }
        Ok(())
    }

    /// Verify a proposer seal (PQ signature over header hash).
    pub fn verify_seal(
        &self,
        header: &BlockHeader,
        seal: &PQSignature,
        proposer_pubkey: &[u8],
        verifier: &dyn Verifier,
    ) -> Result<(), ConsensusError> {
        let header_hash = header.hash();
        let valid = verifier
            .verify(proposer_pubkey, header_hash.as_bytes(), seal)
            .map_err(|_| ConsensusError::InvalidSignature)?;
        if !valid {
            return Err(ConsensusError::InvalidSignature);
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl ConsensusEngine for PoaEngine {
    fn verify_header(&self, header: &BlockHeader) -> Result<(), ConsensusError> {
        self.verify_proposer(header)?;
        // Note: parent verification requires the parent header, which the caller
        // should provide via verify_header_with_parent for full validation.
        Ok(())
    }

    async fn seal_block(&self, block: &mut Block) -> Result<(), ConsensusError> {
        // Sealing requires a Signer which is injected externally.
        // The caller is responsible for signing — this validates the block is
        // sealable by checking the proposer slot.
        let expected = self.config.proposer_for_block(block.header.number);
        if block.header.proposer != expected {
            return Err(ConsensusError::InvalidProposer {
                expected,
                got: block.header.proposer,
            });
        }
        Ok(())
    }

    fn is_proposer(&self, slot: u64, address: &Address) -> bool {
        self.config.proposer_for_block(slot) == *address
    }

    fn engine_type(&self) -> EngineType {
        EngineType::PoA
    }
}

impl PoaEngine {
    /// Full header verification including parent checks and seal.
    pub fn verify_header_with_parent(
        &self,
        header: &BlockHeader,
        parent: &BlockHeader,
        seal: &PQSignature,
        proposer_pubkey: &[u8],
        verifier: &dyn Verifier,
    ) -> Result<(), ConsensusError> {
        self.verify_proposer(header)?;
        self.verify_timestamp(header, Some(parent))?;
        self.verify_seal(header, seal, proposer_pubkey, verifier)?;
        Ok(())
    }

    /// Sign a block header with the proposer's key.
    pub fn sign_block(
        &self,
        block: &mut Block,
        signer: &dyn Signer,
    ) -> Result<(), ConsensusError> {
        let expected = self.config.proposer_for_block(block.header.number);
        if block.header.proposer != expected {
            return Err(ConsensusError::InvalidProposer {
                expected,
                got: block.header.proposer,
            });
        }

        let header_hash = block.header.hash();
        let sig = signer
            .sign(header_hash.as_bytes())
            .map_err(|e| ConsensusError::SealingFailed(e.to_string()))?;
        block.proposer_seal = Some(sig);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_crypto::{DilithiumSigner, DilithiumVerifier};
    use shell_primitives::{Bytes, ShellHash};

    fn test_config() -> (PoaConfig, Address, DilithiumSigner) {
        let signer = DilithiumSigner::generate();
        let addr = Address::from_public_key(signer.public_key());
        let config = PoaConfig::new(vec![addr], 1);
        (config, addr, signer)
    }

    fn sample_header(number: u64, proposer: Address, timestamp: u64) -> BlockHeader {
        BlockHeader {
            parent_hash: ShellHash::ZERO,
            state_root: ShellHash::ZERO,
            transactions_root: ShellHash::ZERO,
            receipts_root: ShellHash::ZERO,
            logs_bloom: Bytes::new(),
            number,
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp,
            extra_data: Bytes::new(),
            proposer,
            sig_aggregate_proof: None,
        }
    }

    #[test]
    fn proposer_round_robin() {
        let a1 = Address::from_public_key(shell_primitives::keccak256(b"a1").as_bytes());
        let a2 = Address::from_public_key(shell_primitives::keccak256(b"a2").as_bytes());
        let a3 = Address::from_public_key(shell_primitives::keccak256(b"a3").as_bytes());
        let config = PoaConfig::new(vec![a1, a2, a3], 1);

        assert_eq!(config.proposer_for_block(0), a1);
        assert_eq!(config.proposer_for_block(1), a2);
        assert_eq!(config.proposer_for_block(2), a3);
        assert_eq!(config.proposer_for_block(3), a1); // wraps around
    }

    #[test]
    fn verify_header_valid() {
        let (config, addr, _) = test_config();
        let engine = PoaEngine::new(config);
        let header = sample_header(0, addr, 1000);

        assert!(engine.verify_header(&header).is_ok());
    }

    #[test]
    fn verify_header_wrong_proposer() {
        let (config, _, _) = test_config();
        let engine = PoaEngine::new(config);
        let wrong = Address::from_public_key(shell_primitives::keccak256(b"intruder").as_bytes());
        let header = sample_header(0, wrong, 1000);

        let err = engine.verify_header(&header).unwrap_err();
        assert!(matches!(err, ConsensusError::UnknownProposer(_)));
    }

    #[test]
    fn verify_timestamp_too_early() {
        let (config, addr, _) = test_config();
        let engine = PoaEngine::new(config);

        let parent = sample_header(0, addr, 1000);
        let child = sample_header(1, addr, 1000); // same timestamp, needs +1

        let result = engine.verify_timestamp(&child, Some(&parent));
        assert!(result.is_err());
    }

    #[test]
    fn verify_timestamp_valid() {
        let (config, addr, _) = test_config();
        let engine = PoaEngine::new(config);

        let parent = sample_header(0, addr, 1000);
        let mut child = sample_header(1, addr, 1001);
        child.parent_hash = parent.hash();

        let result = engine.verify_timestamp(&child, Some(&parent));
        assert!(result.is_ok());
    }

    #[test]
    fn sign_and_verify_seal() {
        let (config, addr, signer) = test_config();
        let engine = PoaEngine::new(config);

        let header = sample_header(0, addr, 1000);
        let mut block = Block {
            header,
            transactions: vec![],
            proposer_seal: None,
        };

        engine.sign_block(&mut block, &signer).unwrap();
        assert!(block.proposer_seal.is_some());

        let verifier = DilithiumVerifier;
        let seal = block.proposer_seal.as_ref().unwrap();
        assert!(engine
            .verify_seal(&block.header, seal, signer.public_key(), &verifier)
            .is_ok());
    }

    #[test]
    fn is_proposer_check() {
        let (config, addr, _) = test_config();
        let engine = PoaEngine::new(config);

        assert!(engine.is_proposer(0, &addr));
        // With single authority, all slots map to same address
        assert!(engine.is_proposer(1, &addr));
    }

    #[test]
    fn engine_type_is_poa() {
        let (config, _, _) = test_config();
        let engine = PoaEngine::new(config);
        assert_eq!(engine.engine_type(), EngineType::PoA);
    }
}
