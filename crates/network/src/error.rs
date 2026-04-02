//! Network error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("transport error: {0}")]
    Transport(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("peer not found: {0}")]
    PeerNotFound(String),

    #[error("channel closed")]
    ChannelClosed,

    #[error("network shut down")]
    Shutdown,
}
