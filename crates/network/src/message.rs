//! Network message types for block and transaction propagation.

use serde::{Deserialize, Serialize};
use shell_core::{Block, SignedTransaction};

/// Unique identifier for a network peer.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct PeerId(pub String);

impl std::fmt::Display for PeerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for PeerId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for PeerId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Messages exchanged between nodes on the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// Announce a newly produced or received block.
    NewBlock(Box<Block>),
    /// Announce a new transaction for mempool inclusion.
    NewTransaction(Box<SignedTransaction>),
    /// Request a range of blocks by number.
    BlockRequest {
        start_number: u64,
        count: u64,
    },
    /// Response to a block request.
    BlockResponse {
        blocks: Vec<Block>,
    },
    /// Ping to check liveness.
    Ping,
    /// Pong response to ping.
    Pong,
}

/// Events produced by the network layer for the node to process.
#[derive(Debug)]
pub enum NetworkEvent {
    /// A message was received from a peer.
    MessageReceived {
        peer: PeerId,
        message: NetworkMessage,
    },
    /// A new peer connected.
    PeerConnected(PeerId),
    /// A peer disconnected.
    PeerDisconnected(PeerId),
}
