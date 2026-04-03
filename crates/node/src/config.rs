//! Node configuration aggregating all component configs.

use std::net::SocketAddr;

use shell_consensus::PoaConfig;
use shell_mempool::MempoolConfig;
use shell_network::NetworkConfig;
use shell_primitives::Address;
use shell_rpc::RpcConfig;

/// Top-level configuration for a shell-chain node.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Chain identifier.
    pub chain_id: u64,
    /// PoA consensus configuration.
    pub consensus: PoaConfig,
    /// Transaction pool configuration.
    pub mempool: MempoolConfig,
    /// JSON-RPC server configuration.
    pub rpc: RpcConfig,
    /// P2P network configuration.
    pub network: NetworkConfig,
    /// This node's authority address (if it is a block producer).
    pub proposer_address: Option<Address>,
    /// Block production interval in milliseconds.
    pub block_time_ms: u64,
    /// Data directory for persistent storage.
    pub data_dir: String,
}

impl NodeConfig {
    /// Create a minimal dev-mode configuration with a single authority.
    pub fn dev(authority: Address) -> Self {
        Self {
            chain_id: 1337,
            consensus: PoaConfig::new(vec![authority], 2),
            mempool: MempoolConfig {
                chain_id: 1337,
                ..MempoolConfig::default()
            },
            rpc: RpcConfig {
                listen_addr: SocketAddr::from(([127, 0, 0, 1], 8545)),
                ws_addr: None,
                ..RpcConfig::default()
            },
            network: NetworkConfig::default(),
            proposer_address: Some(authority),
            block_time_ms: 2000,
            data_dir: "shell-data".into(),
        }
    }
}
