use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use shell_primitives::{Address, ShellHash, U256};

/// Genesis configuration for the Shell-Chain network.
///
/// Parsed from a `genesis.json` file. Defines chain identity,
/// consensus parameters, and initial account allocations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisConfig {
    /// Unique chain identifier.
    pub chain_id: u64,
    /// Human-readable chain name.
    #[serde(default = "default_chain_name")]
    pub chain_name: String,
    /// Unix timestamp for the genesis block.
    pub timestamp: u64,
    /// Block gas limit.
    #[serde(default = "default_gas_limit")]
    pub gas_limit: u64,
    /// Extra data embedded in the genesis block header.
    #[serde(default)]
    pub extra_data: String,
    /// Consensus engine configuration.
    pub consensus: ConsensusConfig,
    /// Initial account allocations (address → balance + optional code/storage).
    #[serde(default)]
    pub alloc: HashMap<Address, AllocEntry>,
    /// Bootstrap node multiaddrs for P2P peer discovery.
    ///
    /// Each entry should be a full multiaddr with a `/p2p/<peer_id>` component,
    /// e.g. `/ip4/1.2.3.4/tcp/30303/p2p/12D3KooW...`.
    #[serde(default)]
    pub boot_nodes: Vec<String>,
}

fn default_chain_name() -> String {
    "shell-chain".to_string()
}

fn default_gas_limit() -> u64 {
    30_000_000
}

/// Consensus engine configuration within genesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "engine")]
pub enum ConsensusConfig {
    /// Proof of Authority consensus.
    #[serde(rename = "poa")]
    PoA {
        /// Ordered list of authority addresses.
        authorities: Vec<Address>,
        /// Ordered authority PQ public keys encoded as hex strings.
        ///
        /// Entries must align with `authorities` by index so followers can
        /// verify proposer seals immediately on first block import.
        #[serde(default)]
        authority_pubkeys: Vec<String>,
        /// Minimum seconds between blocks.
        block_time_secs: u64,
        /// Number of blocks per epoch. Defaults to 0 (no epochs).
        #[serde(default)]
        epoch_length: u64,
    },
}

/// An entry in the genesis allocation table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllocEntry {
    /// Initial balance in wei.
    pub balance: U256,
    /// Optional nonce override (default 0).
    #[serde(default)]
    pub nonce: u64,
    /// Optional contract code (hex-encoded).
    #[serde(default)]
    pub code: Option<String>,
    /// Optional storage entries (slot → value).
    #[serde(default)]
    pub storage: Option<HashMap<ShellHash, ShellHash>>,
}

impl GenesisConfig {
    /// Parse genesis configuration from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Parse genesis configuration from a JSON file path.
    pub fn from_file(path: &std::path::Path) -> Result<Self, GenesisError> {
        let content = std::fs::read_to_string(path).map_err(|e| GenesisError::Io(e.to_string()))?;
        Self::from_json(&content).map_err(|e| GenesisError::Parse(e.to_string()))
    }

    /// Serialize to JSON string (pretty-printed).
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GenesisError {
    #[error("I/O error: {0}")]
    Io(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("state initialization error: {0}")]
    StateInit(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_genesis_json() -> String {
        let authority = Address::from([0x01; 20]);
        let funded = Address::from([0x02; 20]);
        serde_json::json!({
            "chain_id": 1337,
            "chain_name": "shell-testnet",
            "timestamp": 1700000000u64,
            "gas_limit": 30000000u64,
            "extra_data": "shell-genesis",
            "consensus": {
                "engine": "poa",
                "authorities": [authority],
                "authority_pubkeys": ["0x1234"],
                "block_time_secs": 2u64
            },
            "alloc": {
                authority.to_string(): {
                    "balance": "0x3635c9adc5dea00000"
                },
                funded.to_string(): {
                    "balance": "0xde0b6b3a7640000",
                    "nonce": 5u64
                }
            }
        })
        .to_string()
    }

    #[test]
    fn parse_genesis_json() {
        let config = GenesisConfig::from_json(&sample_genesis_json()).unwrap();
        assert_eq!(config.chain_id, 1337);
        assert_eq!(config.chain_name, "shell-testnet");
        assert_eq!(config.gas_limit, 30_000_000);
        assert_eq!(config.alloc.len(), 2);
    }

    #[test]
    fn consensus_config_is_poa() {
        let config = GenesisConfig::from_json(&sample_genesis_json()).unwrap();
        match &config.consensus {
            ConsensusConfig::PoA {
                authorities,
                authority_pubkeys,
                block_time_secs,
                ..
            } => {
                assert_eq!(authorities.len(), 1);
                assert_eq!(authority_pubkeys, &vec!["0x1234".to_string()]);
                assert_eq!(*block_time_secs, 2);
            }
        }
    }

    #[test]
    fn alloc_entry_with_nonce() {
        let config = GenesisConfig::from_json(&sample_genesis_json()).unwrap();
        // Find the entry with nonce=5
        let entry = config
            .alloc
            .values()
            .find(|e| e.nonce == 5)
            .expect("should have entry with nonce 5");
        assert_eq!(entry.nonce, 5);
    }

    #[test]
    fn roundtrip_json() {
        let config = GenesisConfig::from_json(&sample_genesis_json()).unwrap();
        let json = config.to_json_pretty().unwrap();
        let config2 = GenesisConfig::from_json(&json).unwrap();
        assert_eq!(config.chain_id, config2.chain_id);
        assert_eq!(config.alloc.len(), config2.alloc.len());
    }

    #[test]
    fn serialized_genesis_uses_bech32m_addresses() {
        let config = GenesisConfig::from_json(&sample_genesis_json()).unwrap();
        let json = config.to_json_pretty().unwrap();
        assert!(json.contains(&Address::from([0x01; 20]).to_string()));
        assert!(json.contains(&Address::from([0x02; 20]).to_string()));
    }

    #[test]
    fn defaults_applied() {
        let json = r#"{
            "chain_id": 42,
            "timestamp": 0,
            "consensus": {
                "engine": "poa",
                "authorities": [],
                "block_time_secs": 1
            }
        }"#;
        let config = GenesisConfig::from_json(json).unwrap();
        assert_eq!(config.chain_name, "shell-chain");
        assert_eq!(config.gas_limit, 30_000_000);
        assert!(config.alloc.is_empty());
        assert!(config.boot_nodes.is_empty());
    }

    #[test]
    fn boot_nodes_deserialization() {
        let json = r#"{
            "chain_id": 1337,
            "timestamp": 0,
            "consensus": {
                "engine": "poa",
                "authorities": [],
                "block_time_secs": 1
            },
            "boot_nodes": [
                "/ip4/1.2.3.4/tcp/30303/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN",
                "/ip4/5.6.7.8/tcp/30303/p2p/12D3KooWRPnSKiKCPdjoEyrYJzJEMc4TYuknR7ik3jCRe6RkNhWh"
            ]
        }"#;
        let config = GenesisConfig::from_json(json).unwrap();
        assert_eq!(config.boot_nodes.len(), 2);
        assert!(config.boot_nodes[0].contains("/ip4/1.2.3.4/"));
        assert!(config.boot_nodes[1].contains("/ip4/5.6.7.8/"));
    }

    #[test]
    fn boot_nodes_optional_defaults_to_empty() {
        let json = r#"{
            "chain_id": 99,
            "timestamp": 0,
            "consensus": {
                "engine": "poa",
                "authorities": [],
                "block_time_secs": 1
            }
        }"#;
        let config = GenesisConfig::from_json(json).unwrap();
        assert!(config.boot_nodes.is_empty());
    }

    #[test]
    fn boot_nodes_roundtrip_json() {
        let json = r#"{
            "chain_id": 1337,
            "timestamp": 0,
            "consensus": {
                "engine": "poa",
                "authorities": [],
                "block_time_secs": 1
            },
            "boot_nodes": [
                "/ip4/1.2.3.4/tcp/30303/p2p/12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN"
            ]
        }"#;
        let config = GenesisConfig::from_json(json).unwrap();
        assert_eq!(config.boot_nodes.len(), 1);

        let serialized = config.to_json_pretty().unwrap();
        let config2 = GenesisConfig::from_json(&serialized).unwrap();
        assert_eq!(config2.boot_nodes.len(), 1);
        assert_eq!(config.boot_nodes[0], config2.boot_nodes[0]);
    }
}
