use shell_primitives::{Address, ShellHash};
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

    #[error("equivocation detected: validator {validator} attested to conflicting block {conflicting_hash} at height {height}")]
    Equivocation {
        validator: Address,
        conflicting_hash: ShellHash,
        height: u64,
    },

    #[error("attestation for unknown block: {0}")]
    UnknownBlock(ShellHash),

    #[error("cannot reorg past finalized block {0}")]
    ReorgPastFinalized(u64),

    // wPoA / validator lifecycle errors
    #[error("signing error: {0}")]
    SigningError(String),

    #[error("no signer configured for this node")]
    NoSigner,

    #[error("validator already exists: {0}")]
    AlreadyValidator(Address),

    #[error("cannot remove last active validator")]
    LastValidator,

    #[error("invalid lifecycle transition: {0}")]
    InvalidLifecycleTransition(String),
}
