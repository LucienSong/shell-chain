use async_trait::async_trait;
use shell_core::{Block, BlockHeader};
use shell_primitives::Address;

use crate::ConsensusError;

/// Consensus engine type identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineType {
    /// Proof of Authority — Phase 1 consensus.
    PoA,
    /// Byzantine Fault Tolerant — reserved for Phase 2 upgrade.
    BFT,
}

/// Pluggable consensus engine interface.
///
/// Implementations provide block validation, sealing, and proposer selection.
/// The trait is designed for extensibility: adding a new consensus algorithm
/// (e.g., BFT) requires only a new implementation, no changes to existing code.
#[async_trait]
pub trait ConsensusEngine: Send + Sync {
    /// Validate a block header against consensus rules.
    ///
    /// Checks: proposer is authorized, timestamp is valid, signature is correct.
    fn verify_header(&self, header: &BlockHeader) -> Result<(), ConsensusError>;

    /// Seal a block by signing and finalizing it for broadcast.
    ///
    /// The implementation should set `block.proposer_seal` with a valid
    /// PQ signature over the block header.
    async fn seal_block(&self, block: &mut Block) -> Result<(), ConsensusError>;

    /// Check whether the given address is the proposer for the given slot.
    ///
    /// Slot is typically `timestamp / block_interval`.
    fn is_proposer(&self, slot: u64, address: &Address) -> bool;

    /// Return the engine type identifier.
    fn engine_type(&self) -> EngineType;
}
