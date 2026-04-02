//! shell-network: P2P networking layer for the shell-chain node.
//!
//! Provides a trait-based network abstraction (`NetworkService`) with
//! pluggable implementations:
//!
//! - `ChannelNetwork`: In-process broadcast channels for testing and
//!   single-node development. No real TCP connections needed.
//!
//! Future: libp2p gossipsub implementation for production multi-node
//! deployments.

pub mod channel;
pub mod config;
pub mod error;
pub mod message;
pub mod service;

pub use channel::{ChannelNetwork, NetworkBus};
pub use config::NetworkConfig;
pub use error::NetworkError;
pub use message::{NetworkEvent, NetworkMessage, PeerId};
pub use service::NetworkService;
