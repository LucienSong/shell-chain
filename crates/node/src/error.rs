//! Node error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("storage error: {0}")]
    Storage(#[from] shell_storage::StorageError),

    #[error("consensus error: {0}")]
    Consensus(#[from] shell_consensus::ConsensusError),

    #[error("evm error: {0}")]
    Evm(#[from] shell_evm::ExecutorError),

    #[error("network error: {0}")]
    Network(#[from] shell_network::NetworkError),

    #[error("node not configured as proposer")]
    NotProposer,

    #[error("missing genesis block")]
    NoGenesis,

    #[error("startup failed: {0}")]
    Startup(String),
}
