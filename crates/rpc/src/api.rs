//! JSON-RPC API trait definitions using jsonrpsee proc macros.

use jsonrpsee::proc_macros::rpc;
use shell_primitives::{Address, ShellHash};

use crate::types::{RpcBlock, RpcLogWithMeta, RpcReceipt, RpcTransaction, CallRequest};
use crate::filter::RawLogFilter;

/// Web3 namespace RPCs (client metadata and utility).
#[rpc(server, namespace = "web3")]
pub trait Web3Api {
    /// Returns the current client version string.
    #[method(name = "clientVersion")]
    async fn client_version(&self) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the Keccak-256 hash of the given data.
    #[method(name = "sha3")]
    async fn sha3(&self, data: String) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;
}

/// Net namespace RPCs (network status).
#[rpc(server, namespace = "net")]
pub trait NetApi {
    /// Returns the chain ID as a decimal string.
    #[method(name = "version")]
    async fn version(&self) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns true if the node is listening for connections.
    #[method(name = "listening")]
    async fn listening(&self) -> Result<bool, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the number of connected peers as a hex string.
    #[method(name = "peerCount")]
    async fn peer_count(&self) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;
}

/// Ethereum-compatible JSON-RPC API.
#[rpc(server, namespace = "eth")]
pub trait EthApi {
    /// Returns the current block number.
    #[method(name = "blockNumber")]
    async fn block_number(&self) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the chain ID.
    #[method(name = "chainId")]
    async fn chain_id(&self) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns false when not syncing; will return sync status object later.
    #[method(name = "syncing")]
    async fn syncing(&self) -> Result<serde_json::Value, jsonrpsee::types::ErrorObjectOwned>;

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

    /// Executes a call without creating a transaction (read-only).
    #[method(name = "call")]
    async fn call(
        &self,
        tx: CallRequest,
        block: Option<String>,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Estimates gas needed for a transaction.
    #[method(name = "estimateGas")]
    async fn estimate_gas(
        &self,
        tx: CallRequest,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the bytecode at a given address.
    #[method(name = "getCode")]
    async fn get_code(
        &self,
        address: Address,
        block: Option<String>,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns the value from a storage position at a given address.
    #[method(name = "getStorageAt")]
    async fn get_storage_at(
        &self,
        address: Address,
        position: String,
        block: Option<String>,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns logs matching the given filter object.
    #[method(name = "getLogs")]
    async fn get_logs(
        &self,
        filter: RawLogFilter,
    ) -> Result<Vec<RpcLogWithMeta>, jsonrpsee::types::ErrorObjectOwned>;
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

    /// Returns the current validator set from world state.
    #[method(name = "getValidators")]
    async fn get_validators(
        &self,
    ) -> Result<Vec<Address>, jsonrpsee::types::ErrorObjectOwned>;

    /// Add a validator to the active set. Unauthenticated until M3.
    #[method(name = "addValidator")]
    async fn add_validator(
        &self,
        address: String,
    ) -> Result<bool, jsonrpsee::types::ErrorObjectOwned>;

    /// Remove a validator from the active set. Unauthenticated until M3.
    #[method(name = "removeValidator")]
    async fn remove_validator(
        &self,
        address: String,
    ) -> Result<bool, jsonrpsee::types::ErrorObjectOwned>;

    /// Encode calldata for `addValidator(address)` system contract call.
    #[method(name = "encodeAddValidator")]
    async fn encode_add_validator(
        &self,
        address: String,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Encode calldata for `removeValidator(address)` system contract call.
    #[method(name = "encodeRemoveValidator")]
    async fn encode_remove_validator(
        &self,
        address: String,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Propose adding a validator via system contract transaction.
    /// Requires the node to be configured as a validator.
    /// Returns the transaction hash on success.
    #[method(name = "proposeAddValidator")]
    async fn propose_add_validator(
        &self,
        address: String,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Propose removing a validator via system contract transaction.
    /// Requires the node to be configured as a validator.
    /// Returns the transaction hash on success.
    #[method(name = "proposeRemoveValidator")]
    async fn propose_remove_validator(
        &self,
        address: String,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns whether an address is currently a validator.
    #[method(name = "getValidatorStatus")]
    async fn get_validator_status(
        &self,
        address: Address,
    ) -> Result<serde_json::Value, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns governance-related information (validator count, list, system contract address, gas limit).
    #[method(name = "getGovernanceInfo")]
    async fn get_governance_info(
        &self,
    ) -> Result<serde_json::Value, jsonrpsee::types::ErrorObjectOwned>;

    /// Returns estimated gas for a governance operation ("addValidator" or "removeValidator").
    #[method(name = "estimateGovernanceGas")]
    async fn estimate_governance_gas(
        &self,
        operation: String,
    ) -> Result<String, jsonrpsee::types::ErrorObjectOwned>;
}
