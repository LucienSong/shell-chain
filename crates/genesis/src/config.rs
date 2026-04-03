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
        let content = std::fs::read_to_string(path)
            .map_err(|e| GenesisError::Io(e.to_string()))?;
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

    fn sample_genesis_json() -> &'static str {
        r#"{
            "chain_id": 1337,
            "chain_name": "shell-testnet",
            "timestamp": 1700000000,
            "gas_limit": 30000000,
            "extra_data": "shell-genesis",
            "consensus": {
                "engine": "poa",
                "authorities": ["0x0000000000000000000000000000000000000001"],
                "block_time_secs": 2
            },
            "alloc": {
                "0x0000000000000000000000000000000000000001": {
                    "balance": "0x3635c9adc5dea00000"
                },
                "0x0000000000000000000000000000000000000002": {
                    "balance": "0xde0b6b3a7640000",
                    "nonce": 5
                }
            }
        }"#
    }

    #[test]
    fn parse_genesis_json() {
        let config = GenesisConfig::from_json(sample_genesis_json()).unwrap();
        assert_eq!(config.chain_id, 1337);
        assert_eq!(config.chain_name, "shell-testnet");
        assert_eq!(config.gas_limit, 30_000_000);
        assert_eq!(config.alloc.len(), 2);
    }

    #[test]
    fn consensus_config_is_poa() {
        let config = GenesisConfig::from_json(sample_genesis_json()).unwrap();
        match &config.consensus {
            ConsensusConfig::PoA {
                authorities,
                block_time_secs,
                ..
            } => {
                assert_eq!(authorities.len(), 1);
                assert_eq!(*block_time_secs, 2);
            }
        }
    }

    #[test]
    fn alloc_entry_with_nonce() {
        let config = GenesisConfig::from_json(sample_genesis_json()).unwrap();
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
        let config = GenesisConfig::from_json(sample_genesis_json()).unwrap();
        let json = config.to_json_pretty().unwrap();
        let config2 = GenesisConfig::from_json(&json).unwrap();
        assert_eq!(config.chain_id, config2.chain_id);
        assert_eq!(config.alloc.len(), config2.alloc.len());
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
    }
}
