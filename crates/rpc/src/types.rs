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
    pub base_fee_per_gas: String,
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
    /// EIP-2718 transaction type — always "0x2" (EIP-1559).
    #[serde(rename = "type")]
    pub tx_type: String,
    /// Legacy ECDSA compat stub — always "0x0" (PQ chain has no ECDSA).
    pub v: String,
    /// Legacy ECDSA compat stub — always "0x0".
    pub r: String,
    /// Legacy ECDSA compat stub — always "0x0".
    pub s: String,
}

/// Hex-encoded transaction receipt response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcReceipt {
    pub transaction_hash: ShellHash,
    pub block_hash: ShellHash,
    pub block_number: String,
    pub transaction_index: String,
    pub from: Address,
    pub to: Option<Address>,
    pub status: String,
    pub gas_used: String,
    pub cumulative_gas_used: String,
    pub effective_gas_price: String,
    pub contract_address: Option<Address>,
    pub logs: Vec<RpcLog>,
    pub logs_bloom: String,
    #[serde(rename = "type")]
    pub tx_type: String,
}

/// Hex-encoded log response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcLog {
    pub address: Address,
    pub topics: Vec<ShellHash>,
    pub data: String,
}

/// Full log object returned by `eth_getLogs` with block/tx metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcLogWithMeta {
    pub address: Address,
    pub topics: Vec<ShellHash>,
    pub data: String,
    pub block_number: String,
    pub block_hash: ShellHash,
    pub transaction_hash: ShellHash,
    pub transaction_index: String,
    pub log_index: String,
    /// Always `false` for a non-reorg chain.
    pub removed: bool,
}

/// Ethereum `eth_call` / `eth_estimateGas` request object.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallRequest {
    /// Sender address (defaults to zero address if absent).
    pub from: Option<Address>,
    /// Destination address (required for calls, absent for contract creation).
    pub to: Option<Address>,
    /// Hex-encoded call data.
    pub data: Option<String>,
    /// Hex-encoded value in wei.
    pub value: Option<String>,
    /// Hex-encoded gas limit.
    pub gas: Option<String>,
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
