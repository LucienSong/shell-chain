//! RPC handler implementation backed by chain storage, world state, and mempool.

use std::sync::Arc;

use jsonrpsee::types::ErrorObjectOwned;

use shell_core::{Block, SignedTransaction};
use shell_mempool::TxPool;
use shell_primitives::{Address, ShellHash};
use shell_storage::{ChainStore, KvStore, WorldState};

use crate::api::{EthApiServer, ShellApiServer};
use crate::types::*;

/// JSON-RPC handler wired to storage and mempool backends.
///
/// All methods are read-only against storage (no state mutation).
/// `send_raw_transaction` is a stub that returns an error until
/// full tx deserialization from raw bytes is implemented.
pub struct RpcHandler<S: KvStore + 'static> {
    chain_store: Arc<ChainStore<S>>,
    world_state: Arc<parking_lot::RwLock<WorldState<S>>>,
    tx_pool: Arc<TxPool>,
    chain_id: u64,
}

impl<S: KvStore + 'static> Clone for RpcHandler<S> {
    fn clone(&self) -> Self {
        Self {
            chain_store: Arc::clone(&self.chain_store),
            world_state: Arc::clone(&self.world_state),
            tx_pool: Arc::clone(&self.tx_pool),
            chain_id: self.chain_id,
        }
    }
}

impl<S: KvStore + 'static> RpcHandler<S> {
    /// Create a new RPC handler with access to chain data.
    pub fn new(
        chain_store: Arc<ChainStore<S>>,
        world_state: Arc<parking_lot::RwLock<WorldState<S>>>,
        tx_pool: Arc<TxPool>,
        chain_id: u64,
    ) -> Self {
        Self {
            chain_store,
            world_state,
            tx_pool,
            chain_id,
        }
    }
}

/// Convert a storage error into a JSON-RPC internal error.
fn internal_err(msg: impl std::fmt::Display) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32603, msg.to_string(), None::<()>)
}

/// Parse a block number string: "latest", "earliest", or "0x..." hex.
fn parse_block_number(s: &str) -> Result<Option<u64>, ErrorObjectOwned> {
    match s {
        "latest" | "pending" => Ok(None), // None = head
        "earliest" => Ok(Some(0)),
        hex if hex.starts_with("0x") => {
            u64::from_str_radix(&hex[2..], 16)
                .map(Some)
                .map_err(|_| internal_err(format!("invalid block number: {hex}")))
        }
        _ => Err(internal_err(format!("invalid block number: {s}"))),
    }
}

/// Convert a core Block to an RpcBlock response.
fn block_to_rpc(block: &Block) -> RpcBlock {
    RpcBlock {
        hash: block.hash(),
        parent_hash: block.header.parent_hash,
        number: hex_u64(block.header.number),
        timestamp: hex_u64(block.header.timestamp),
        gas_limit: hex_u64(block.header.gas_limit),
        gas_used: hex_u64(block.header.gas_used),
        miner: block.header.proposer,
        state_root: block.header.state_root,
        transactions_root: block.header.transactions_root,
        receipts_root: block.header.receipts_root,
        transactions: block.transactions.iter().map(|tx| tx.hash()).collect(),
        size: hex_u64(0), // placeholder
    }
}

/// Convert a SignedTransaction to an RpcTransaction response.
fn tx_to_rpc(
    tx: &SignedTransaction,
    block_hash: Option<ShellHash>,
    block_number: Option<u64>,
    tx_index: Option<u32>,
) -> RpcTransaction {
    RpcTransaction {
        hash: tx.hash(),
        block_hash,
        block_number: block_number.map(hex_u64),
        transaction_index: tx_index.map(|i| hex_u64(i as u64)),
        from: tx.sender(),
        to: tx.tx.to,
        value: hex_u256(tx.tx.value),
        gas: hex_u64(tx.tx.gas_limit),
        gas_price: hex_u64(tx.tx.max_fee_per_gas),
        max_fee_per_gas: hex_u64(tx.tx.max_fee_per_gas),
        max_priority_fee_per_gas: hex_u64(tx.tx.max_priority_fee_per_gas),
        nonce: hex_u64(tx.tx.nonce),
        input: hex_bytes(tx.tx.data.as_ref()),
        chain_id: hex_u64(tx.tx.chain_id),
    }
}

#[jsonrpsee::core::async_trait]
impl<S: KvStore + 'static> EthApiServer for RpcHandler<S> {
    async fn block_number(&self) -> Result<String, ErrorObjectOwned> {
        let head = self
            .chain_store
            .get_head_block()
            .map_err(internal_err)?;
        let num = head.map(|b| b.number()).unwrap_or(0);
        Ok(hex_u64(num))
    }

    async fn chain_id(&self) -> Result<String, ErrorObjectOwned> {
        Ok(hex_u64(self.chain_id))
    }

    async fn get_block_by_number(
        &self,
        number: String,
        _full_txs: bool,
    ) -> Result<Option<RpcBlock>, ErrorObjectOwned> {
        let num = parse_block_number(&number)?;
        let block = match num {
            Some(n) => self.chain_store.get_block_by_number(n).map_err(internal_err)?,
            None => self.chain_store.get_head_block().map_err(internal_err)?,
        };
        Ok(block.as_ref().map(block_to_rpc))
    }

    async fn get_block_by_hash(
        &self,
        hash: ShellHash,
        _full_txs: bool,
    ) -> Result<Option<RpcBlock>, ErrorObjectOwned> {
        let block = self
            .chain_store
            .get_block_by_hash(&hash)
            .map_err(internal_err)?;
        Ok(block.as_ref().map(block_to_rpc))
    }

    async fn get_transaction_by_hash(
        &self,
        hash: ShellHash,
    ) -> Result<Option<RpcTransaction>, ErrorObjectOwned> {
        // Check mempool first
        if let Some(pending_tx) = self.tx_pool.get(&hash) {
            return Ok(Some(tx_to_rpc(&pending_tx, None, None, None)));
        }

        // Then check on-chain index
        let location = self
            .chain_store
            .get_tx_location(&hash)
            .map_err(internal_err)?;

        if let Some((block_hash, tx_index)) = location {
            let block = self
                .chain_store
                .get_block_by_hash(&block_hash)
                .map_err(internal_err)?;
            if let Some(block) = block {
                if let Some(tx) = block.transactions.get(tx_index as usize) {
                    return Ok(Some(tx_to_rpc(
                        tx,
                        Some(block_hash),
                        Some(block.number()),
                        Some(tx_index),
                    )));
                }
            }
        }

        Ok(None)
    }

    async fn get_transaction_receipt(
        &self,
        hash: ShellHash,
    ) -> Result<Option<RpcReceipt>, ErrorObjectOwned> {
        let location = self
            .chain_store
            .get_tx_location(&hash)
            .map_err(internal_err)?;

        if let Some((block_hash, tx_index)) = location {
            let receipts = self
                .chain_store
                .get_receipts(&block_hash)
                .map_err(internal_err)?;
            if let Some(receipts) = receipts {
                if let Some(receipt) = receipts.get(tx_index as usize) {
                    return Ok(Some(RpcReceipt {
                        transaction_hash: receipt.tx_hash,
                        block_number: hex_u64(receipt.block_number),
                        transaction_index: hex_u64(tx_index as u64),
                        status: hex_u64(receipt.status as u64),
                        gas_used: hex_u64(receipt.gas_used),
                        cumulative_gas_used: hex_u64(receipt.cumulative_gas_used),
                        contract_address: receipt.contract_address,
                        logs: receipt
                            .logs
                            .iter()
                            .map(|log| RpcLog {
                                address: log.address,
                                topics: log.topics.clone(),
                                data: hex_bytes(log.data.as_ref()),
                            })
                            .collect(),
                    }));
                }
            }
        }

        Ok(None)
    }

    async fn get_balance(
        &self,
        address: Address,
        _block: Option<String>,
    ) -> Result<String, ErrorObjectOwned> {
        let ws = self.world_state.read();
        let balance = ws.get_balance(&address).map_err(internal_err)?;
        Ok(hex_u256(balance))
    }

    async fn get_transaction_count(
        &self,
        address: Address,
        _block: Option<String>,
    ) -> Result<String, ErrorObjectOwned> {
        let ws = self.world_state.read();
        let nonce = ws.get_nonce(&address).map_err(internal_err)?;
        Ok(hex_u64(nonce))
    }

    async fn gas_price(&self) -> Result<String, ErrorObjectOwned> {
        // Minimum gas price from a fixed baseline; will be dynamic later.
        Ok(hex_u64(1_000_000_000)) // 1 gwei
    }

    async fn send_raw_transaction(
        &self,
        _data: String,
    ) -> Result<ShellHash, ErrorObjectOwned> {
        // Full implementation requires RLP decoding of SignedTransaction from raw bytes.
        // Stub: return an error until wire format is finalized.
        Err(ErrorObjectOwned::owned(
            -32000,
            "eth_sendRawTransaction not yet implemented — use shell_sendTransaction",
            None::<()>,
        ))
    }
}

#[jsonrpsee::core::async_trait]
impl<S: KvStore + 'static> ShellApiServer for RpcHandler<S> {
    async fn get_pq_pubkey(
        &self,
        address: Address,
    ) -> Result<Option<String>, ErrorObjectOwned> {
        let pk = self
            .chain_store
            .get_pubkey(&address)
            .map_err(internal_err)?;
        Ok(pk.map(|bytes| hex_bytes(&bytes)))
    }

    async fn pending_count(&self) -> Result<String, ErrorObjectOwned> {
        Ok(hex_u64(self.tx_pool.len() as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_core::{Block, BlockHeader, Transaction, TransactionReceipt};
    use shell_crypto::{DilithiumSigner, Signer};
    use shell_primitives::Bytes;
    use shell_storage::MemoryDb;

    fn setup() -> RpcHandler<MemoryDb> {
        let db = Arc::new(MemoryDb::new());
        let chain_store = Arc::new(ChainStore::new(db.clone()));
        let world_state = Arc::new(parking_lot::RwLock::new(WorldState::new(db)));
        let tx_pool = Arc::new(TxPool::new(shell_mempool::MempoolConfig::default()));
        RpcHandler::new(chain_store, world_state, tx_pool, 42)
    }

    fn make_genesis_block() -> Block {
        Block {
            header: BlockHeader {
                parent_hash: ShellHash::default(),
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::default(),
                number: 0,
                gas_limit: 30_000_000,
                gas_used: 0,
                timestamp: 1_700_000_000,
                extra_data: Bytes::default(),
                proposer: Address::from_public_key(b"proposer-key-data"),
                sig_aggregate_proof: None,
            },
            transactions: vec![],
            proposer_seal: None,
        }
    }

    #[tokio::test]
    async fn block_number_empty_chain() {
        let handler = setup();
        let result = EthApiServer::block_number(&handler).await.unwrap();
        assert_eq!(result, "0x0");
    }

    #[tokio::test]
    async fn chain_id() {
        let handler = setup();
        let result = EthApiServer::chain_id(&handler).await.unwrap();
        assert_eq!(result, "0x2a"); // 42
    }

    #[tokio::test]
    async fn get_block_after_store() {
        let handler = setup();
        let block = make_genesis_block();
        let hash = block.hash();

        handler.chain_store.put_block(&block).unwrap();
        handler.chain_store.set_canonical(0, &hash).unwrap();
        handler.chain_store.set_head(&hash).unwrap();

        // By number
        let rpc_block = EthApiServer::get_block_by_number(&handler, "0x0".into(), false)
            .await
            .unwrap();
        assert!(rpc_block.is_some());
        assert_eq!(rpc_block.as_ref().unwrap().number, "0x0");

        // By hash
        let rpc_block = EthApiServer::get_block_by_hash(&handler, hash, false)
            .await
            .unwrap();
        assert!(rpc_block.is_some());

        // Latest
        let rpc_block = EthApiServer::get_block_by_number(&handler, "latest".into(), false)
            .await
            .unwrap();
        assert!(rpc_block.is_some());
        assert_eq!(rpc_block.unwrap().number, "0x0");
    }

    #[tokio::test]
    async fn get_balance_default_zero() {
        let handler = setup();
        let addr = Address::from_public_key(b"test-address-key");
        let result = EthApiServer::get_balance(&handler, addr, None).await.unwrap();
        assert_eq!(result, "0x0");
    }

    #[tokio::test]
    async fn get_nonce_default_zero() {
        let handler = setup();
        let addr = Address::from_public_key(b"test-address-key");
        let result = EthApiServer::get_transaction_count(&handler, addr, None)
            .await
            .unwrap();
        assert_eq!(result, "0x0");
    }

    #[tokio::test]
    async fn gas_price_returns_default() {
        let handler = setup();
        let result = EthApiServer::gas_price(&handler).await.unwrap();
        assert_eq!(result, "0x3b9aca00"); // 1 gwei
    }

    #[tokio::test]
    async fn get_nonexistent_tx_returns_none() {
        let handler = setup();
        let result = EthApiServer::get_transaction_by_hash(&handler, ShellHash::default())
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_nonexistent_receipt_returns_none() {
        let handler = setup();
        let result = EthApiServer::get_transaction_receipt(&handler, ShellHash::default())
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn shell_pending_count_empty() {
        let handler = setup();
        let result = ShellApiServer::pending_count(&handler).await.unwrap();
        assert_eq!(result, "0x0");
    }

    #[tokio::test]
    async fn shell_get_pq_pubkey_not_found() {
        let handler = setup();
        let addr = Address::from_public_key(b"unknown");
        let result = ShellApiServer::get_pq_pubkey(&handler, addr).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn shell_get_pq_pubkey_found() {
        let handler = setup();
        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let addr = Address::from_public_key(&pubkey);

        handler.chain_store.put_pubkey(&addr, &pubkey).unwrap();

        let result = ShellApiServer::get_pq_pubkey(&handler, addr).await.unwrap();
        assert!(result.is_some());
        assert!(result.unwrap().starts_with("0x"));
    }
}
