//! Network configuration.

use std::net::SocketAddr;

/// Configuration for the P2P network service.
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    /// Address to listen on for incoming connections.
    pub listen_addr: SocketAddr,
    /// Bootstrap peer addresses to connect to on startup.
    pub boot_nodes: Vec<String>,
    /// Gossipsub topic name for block announcements.
    pub blocks_topic: String,
    /// Gossipsub topic name for transaction announcements.
    pub txs_topic: String,
    /// Maximum number of peers to maintain.
    pub max_peers: usize,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 30303)),
            boot_nodes: vec![],
            blocks_topic: "/shell/blocks/1".into(),
            txs_topic: "/shell/txs/1".into(),
            max_peers: 50,
        }
    }
}
