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
    /// Enable mDNS local peer discovery (disable in production/cloud).
    pub enable_mdns: bool,
    /// Enable Kademlia DHT for global peer discovery.
    pub enable_kademlia: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 30303)),
            boot_nodes: vec![],
            blocks_topic: "/shell/blocks/1".into(),
            txs_topic: "/shell/txs/1".into(),
            max_peers: 50,
            enable_mdns: false,
            enable_kademlia: true,
        }
    }
}

/// Validate that a bootnode multiaddr string is well-formed for P2P bootstrap.
///
/// Checks:
/// - Parses as a valid [`libp2p::Multiaddr`]
/// - Contains an IP transport layer (`/ip4/` or `/ip6/`)
/// - Contains a TCP or UDP transport layer (`/tcp/` or `/udp/`)
/// - Contains a `/p2p/<peer_id>` component with a valid PeerId
#[cfg(feature = "libp2p")]
pub fn validate_bootnode_multiaddr(addr: &str) -> bool {
    use libp2p::Multiaddr;

    let ma: Multiaddr = match addr.parse() {
        Ok(ma) => ma,
        Err(_) => return false,
    };

    let mut has_ip = false;
    let mut has_transport = false;
    let mut has_peer_id = false;

    for proto in ma.iter() {
        match proto {
            libp2p::multiaddr::Protocol::Ip4(_)
            | libp2p::multiaddr::Protocol::Ip6(_) => {
                has_ip = true;
            }
            libp2p::multiaddr::Protocol::Tcp(_)
            | libp2p::multiaddr::Protocol::Udp(_) => {
                has_transport = true;
            }
            libp2p::multiaddr::Protocol::P2p(_) => {
                has_peer_id = true;
            }
            _ => {}
        }
    }

    has_ip && has_transport && has_peer_id
}

#[cfg(test)]
#[cfg(feature = "libp2p")]
mod tests {
    use super::*;

    #[test]
    fn valid_ipv4_tcp_multiaddr() {
        let addr = "/ip4/1.2.3.4/tcp/30303/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
        assert!(validate_bootnode_multiaddr(addr));
    }

    #[test]
    fn valid_ipv6_tcp_multiaddr() {
        let addr = "/ip6/::1/tcp/30303/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
        assert!(validate_bootnode_multiaddr(addr));
    }

    #[test]
    fn valid_ipv4_udp_multiaddr() {
        let addr = "/ip4/10.0.0.1/udp/9000/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
        assert!(validate_bootnode_multiaddr(addr));
    }

    #[test]
    fn reject_missing_peer_id() {
        let addr = "/ip4/1.2.3.4/tcp/30303";
        assert!(!validate_bootnode_multiaddr(addr));
    }

    #[test]
    fn reject_missing_ip() {
        let addr = "/tcp/30303/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
        assert!(!validate_bootnode_multiaddr(addr));
    }

    #[test]
    fn reject_missing_transport() {
        let addr = "/ip4/1.2.3.4/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN";
        assert!(!validate_bootnode_multiaddr(addr));
    }

    #[test]
    fn reject_garbage_string() {
        assert!(!validate_bootnode_multiaddr("not-a-multiaddr"));
    }

    #[test]
    fn reject_empty_string() {
        assert!(!validate_bootnode_multiaddr(""));
    }
}
