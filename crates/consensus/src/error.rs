use shell_primitives::Address;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConsensusError {
    #[error("invalid proposer: expected {expected}, got {got}")]
    InvalidProposer { expected: Address, got: Address },

    #[error("invalid seal signature")]
    InvalidSignature,

    #[error("invalid timestamp: {0}")]
    InvalidTimestamp(String),

    #[error("unknown proposer: {0}")]
    UnknownProposer(Address),

    #[error("not the proposer for slot {0}")]
    NotProposer(u64),

    #[error("block sealing failed: {0}")]
    SealingFailed(String),

    #[error("internal error: {0}")]
    Internal(String),
}
