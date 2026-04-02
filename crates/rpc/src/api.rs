//! JSON-RPC API trait definitions using jsonrpsee proc macros.

use jsonrpsee::proc_macros::rpc;
use shell_primitives::{Address, ShellHash};

use crate::types::{RpcBlock, RpcReceipt, RpcTransaction};

/// Ethereum-compatible JSON-RPC API.
#[rpc(server, namespace = "eth")]
pub trait EthApi {
    /// Returns the current block number.
    #[method(name = "blockNumber")]
    async fn block_number(&self) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the chain ID.
    #[method(name = "chainId")]
    async fn chain_id(&self) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns a block by number (hex-encoded or "latest").
    #[method(name = "getBlockByNumber")]
    async fn get_block_by_number(
        &self,
        number: String,
        full_txs: bool,
    ) -> Result<Option<RpcBlock>, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns a block by hash.
    #[method(name = "getBlockByHash")]
    async fn get_block_by_hash(
        &self,
        hash: ShellHash,
        full_txs: bool,
    ) -> Result<Option<RpcBlock>, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns a transaction by hash.
    #[method(name = "getTransactionByHash")]
    async fn get_transaction_by_hash(
        &self,
        hash: ShellHash,
    ) -> Result<Option<RpcTransaction>, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the receipt of a transaction by hash.
    #[method(name = "getTransactionReceipt")]
    async fn get_transaction_receipt(
        &self,
        hash: ShellHash,
    ) -> Result<Option<RpcReceipt>, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the balance of an address.
    #[method(name = "getBalance")]
    async fn get_balance(
        &self,
        address: Address,
        block: Option<String>,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the nonce (transaction count) of an address.
    #[method(name = "getTransactionCount")]
    async fn get_transaction_count(
        &self,
        address: Address,
        block: Option<String>,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the current gas price suggestion.
    #[method(name = "gasPrice")]
    async fn gas_price(&self) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Submits a signed transaction to the mempool.
    #[method(name = "sendRawTransaction")]
    async fn send_raw_transaction(
        &self,
        data: String,
    ) -> Result<ShellHash, jsonrpsee::types::ErrorObjectOwned>;
}

/// Shell-chain extension API for PQ-specific features.
#[rpc(server, namespace = "shell")]
pub trait ShellApi {
    /// Returns the registered PQ public key for an address.
    #[method(name = "getPqPubkey")]
    async fn get_pq_pubkey(
        &self,
        address: Address,
    ) -> Result<Option<String>, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the number of pending transactions in the mempool.
    #[method(name = "pendingCount")]
    async fn pending_count(&self) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Submit a signed transaction as structured JSON (developer-friendly).
    #[method(name = "sendTransaction")]
    async fn send_transaction(
        &self,
        tx: shell_core::SignedTransaction,
    ) -> Result<ShellHash, jsonrpsee::types::ErrorObjectOwned>;
}
