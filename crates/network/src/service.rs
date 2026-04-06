//! Network service trait defining the P2P interface.

use async_trait::async_trait;

use crate::error::NetworkError;
use crate::message::{NetworkEvent, NetworkMessage};

/// Trait abstracting the P2P network layer.
///
/// Implementations handle peer management, message serialization,
/// and gossip protocol details. The node interacts with the network
/// exclusively through this trait.
#[async_trait]
pub trait NetworkService: Send + Sync {
    /// Broadcast a message to all connected peers.
    async fn broadcast(&self, msg: NetworkMessage) -> Result<(), NetworkError>;

    /// Wait for the next network event.
    /// Returns `None` if the network has shut down.
    async fn next_event(&mut self) -> Option<NetworkEvent>;

    /// Returns the number of currently connected peers.
    async fn peer_count(&self) -> usize;

    /// Shut down the network service gracefully.
    async fn shutdown(&self) -> Result<(), NetworkError>;
}
