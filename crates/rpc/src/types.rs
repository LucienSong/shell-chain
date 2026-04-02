//! Hex-formatted JSON-RPC response types for Ethereum API compatibility.

use serde::{Deserialize, Serialize};
use shell_primitives::{Address, ShellHash, U256};

/// Hex-encoded block response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcBlock {
    pub hash: ShellHash,
    pub parent_hash: ShellHash,
    pub number: String,
    pub timestamp: String,
    pub gas_limit: String,
    pub gas_used: String,
    pub miner: Address,
    pub state_root: ShellHash,
    pub transactions_root: ShellHash,
    pub receipts_root: ShellHash,
    pub transactions: Vec<ShellHash>,
    pub size: String,
}

/// Hex-encoded transaction response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcTransaction {
    pub hash: ShellHash,
    pub block_hash: Option<ShellHash>,
    pub block_number: Option<String>,
    pub transaction_index: Option<String>,
    pub from: Address,
    pub to: Option<Address>,
    pub value: String,
    pub gas: String,
    pub gas_price: String,
    pub max_fee_per_gas: String,
    pub max_priority_fee_per_gas: String,
    pub nonce: String,
    pub input: String,
    pub chain_id: String,
}

/// Hex-encoded transaction receipt response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcReceipt {
    pub transaction_hash: ShellHash,
    pub block_number: String,
    pub transaction_index: String,
    pub status: String,
    pub gas_used: String,
    pub cumulative_gas_used: String,
    pub contract_address: Option<Address>,
    pub logs: Vec<RpcLog>,
}

/// Hex-encoded log response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcLog {
    pub address: Address,
    pub topics: Vec<ShellHash>,
    pub data: String,
}

/// Format a u64 as "0x..." hex string.
pub fn hex_u64(v: u64) -> String {
    format!("{:#x}", v)
}

/// Format a U256 as "0x..." hex string.
pub fn hex_u256(v: U256) -> String {
    format!("{:#x}", v)
}

/// Format bytes as "0x..." hex string.
pub fn hex_bytes(data: &[u8]) -> String {
    format!("0x{}", hex::encode(data))
}
