//! RPC handler implementation backed by chain storage, world state, and mempool.

use std::sync::Arc;

use jsonrpsee::types::ErrorObjectOwned;

use shell_core::{Block, BlockHeader, SignedTransaction, Transaction};
use shell_evm::bloom::BLOOM_SIZE;
use shell_crypto::DilithiumVerifier;
use shell_evm::{ShellEvm, ShellStateDb};
use shell_mempool::TxPool;
use shell_primitives::{Address, Bytes, ShellHash, U256};
use shell_storage::{ChainStore, KvStore, WorldState};

use crate::api::{EthApiServer, ShellApiServer};
use crate::filter::{RawLogFilter, MAX_BLOCK_RANGE};
use crate::subscriptions::BlockEvent;
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
    /// Optional channel for broadcasting new transactions to the network layer.
    tx_broadcast: Option<tokio::sync::mpsc::UnboundedSender<SignedTransaction>>,
    /// Broadcast sender for block events (used by eth_subscribe).
    block_events: tokio::sync::broadcast::Sender<BlockEvent>,
}

impl<S: KvStore + 'static> Clone for RpcHandler<S> {
    fn clone(&self) -> Self {
        Self {
            chain_store: Arc::clone(&self.chain_store),
            world_state: Arc::clone(&self.world_state),
            tx_pool: Arc::clone(&self.tx_pool),
            chain_id: self.chain_id,
            tx_broadcast: self.tx_broadcast.clone(),
            block_events: self.block_events.clone(),
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
        tx_broadcast: Option<tokio::sync::mpsc::UnboundedSender<SignedTransaction>>,
        block_events: tokio::sync::broadcast::Sender<BlockEvent>,
    ) -> Self {
        Self {
            chain_store,
            world_state,
            tx_pool,
            chain_id,
            tx_broadcast,
            block_events,
        }
    }

    /// Returns a reference to the block event broadcast sender.
    pub fn block_event_sender(&self) -> &tokio::sync::broadcast::Sender<BlockEvent> {
        &self.block_events
    }

    /// Validate and submit a signed transaction to the mempool.
    /// On success, also forwards the transaction to the network broadcast channel
    /// (if one was provided) so peers can include it in their mempools.
    fn submit_tx(&self, signed_tx: SignedTransaction) -> Result<ShellHash, ErrorObjectOwned> {
        let chain_store = &self.chain_store;
        let ws = self.world_state.read();

        let known_pubkeys = |addr: &Address| -> Option<Vec<u8>> {
            chain_store.get_pubkey(addr).ok().flatten()
        };
        let balance_of = |addr: &Address| -> U256 {
            ws.get_balance(addr).unwrap_or(U256::ZERO)
        };

        // Clone before insert (which consumes the value) so we can broadcast on success.
        let tx_for_broadcast = self.tx_broadcast.as_ref().map(|_| signed_tx.clone());

        let verifier = DilithiumVerifier;
        let hash = self
            .tx_pool
            .insert(signed_tx, &verifier, &known_pubkeys, &balance_of)
            .map_err(|e| ErrorObjectOwned::owned(-32000, e.to_string(), None::<()>))?;

        // Broadcast to peers via the network channel.
        if let (Some(sender), Some(tx)) = (&self.tx_broadcast, tx_for_broadcast) {
            let _ = sender.send(tx);
        }

        Ok(hash)
    }

    /// Execute a call against a temporary EVM and return (output_bytes, gas_used).
    fn execute_call(
        &self,
        req: &crate::types::CallRequest,
    ) -> Result<(Vec<u8>, u64), ErrorObjectOwned> {
        let store = self.chain_store.store().clone();

        // Snapshot current state root so the temp WorldState sees committed data.
        let state_root = {
            let mut ws = self.world_state.write();
            ws.state_root().map_err(internal_err)?
        };

        let world_state =
            WorldState::at_root(store.clone(), &state_root).map_err(internal_err)?;
        let chain_store = ChainStore::new(store);
        let state_db = ShellStateDb::new(world_state, chain_store);
        let mut evm = ShellEvm::new(state_db, self.chain_id);

        let from = req.from.unwrap_or(Address::ZERO);
        let gas_limit = req
            .gas
            .as_deref()
            .map(|s| parse_hex_u64(s))
            .transpose()?
            .unwrap_or(30_000_000);
        let value = req
            .value
            .as_deref()
            .map(|s| parse_hex_u256(s))
            .transpose()?
            .unwrap_or(U256::ZERO);
        let data = req
            .data
            .as_deref()
            .map(|s| {
                let s = s.strip_prefix("0x").unwrap_or(s);
                hex::decode(s).map(Bytes::from)
            })
            .transpose()
            .map_err(|e| internal_err(format!("invalid call data hex: {e}")))?
            .unwrap_or_default();

        let tx = Transaction {
            chain_id: self.chain_id,
            nonce: 0,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
            gas_limit,
            to: req.to,
            value,
            data,
        };

        let sig = shell_crypto::PQSignature::new(
            shell_crypto::SignatureType::Dilithium3,
            vec![],
        );
        let signed = SignedTransaction::new(from, tx, sig);

        let header = BlockHeader {
            parent_hash: ShellHash::ZERO,
            state_root: ShellHash::ZERO,
            transactions_root: ShellHash::ZERO,
            receipts_root: ShellHash::ZERO,
            logs_bloom: Bytes::default(),
            number: 0,
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 0,
            extra_data: Bytes::default(),
            proposer: Address::ZERO,
            sig_aggregate_proof: None,
        };

        let result = evm
            .execute_tx(&signed, &header, 0, 0)
            .map_err(|e| internal_err(format!("EVM execution failed: {e}")))?;

        Ok((result.output.clone(), result.gas_used))
    }
}

/// Convert a storage error into a JSON-RPC internal error.
fn internal_err(msg: impl std::fmt::Display) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32603, msg.to_string(), None::<()>)
}

/// Parse a hex-encoded address string ("0x..." with 20 bytes).
fn parse_address(s: &str) -> Result<Address, ErrorObjectOwned> {
    let hex_str = s.strip_prefix("0x").unwrap_or(s);
    let bytes =
        hex::decode(hex_str).map_err(|e| internal_err(format!("invalid address hex: {e}")))?;
    Address::try_from_slice(&bytes)
        .map_err(|e| internal_err(format!("invalid address length: {e}")))
}

/// Parse a hex string "0x..." into u64.
fn parse_hex_u64(s: &str) -> Result<u64, ErrorObjectOwned> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(s, 16).map_err(|_| internal_err(format!("invalid hex u64: 0x{s}")))
}

/// Parse a hex string "0x..." into U256.
fn parse_hex_u256(s: &str) -> Result<U256, ErrorObjectOwned> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(
        // left-pad to 64 hex chars so from_be_slice works for short values
        if s.len() < 64 {
            format!("{:0>64}", s)
        } else {
            s.to_string()
        },
    )
    .map_err(|_| internal_err(format!("invalid hex U256: 0x{s}")))?;
    Ok(U256::from_be_slice(&bytes))
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
        tx_type: "0x2".into(),
        v: "0x0".into(),
        r: "0x0".into(),
        s: "0x0".into(),
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
        data: String,
    ) -> Result<ShellHash, ErrorObjectOwned> {
        // Decode hex payload: "0x" + hex-encoded JSON of SignedTransaction.
        let raw = data.strip_prefix("0x").unwrap_or(&data);
        let bytes = hex::decode(raw)
            .map_err(|e| internal_err(format!("invalid hex: {e}")))?;
        let signed_tx: SignedTransaction = serde_json::from_slice(&bytes)
            .map_err(|e| internal_err(format!("invalid tx JSON: {e}")))?;

        self.submit_tx(signed_tx)
    }

    async fn call(
        &self,
        tx: crate::types::CallRequest,
        _block: Option<String>,
    ) -> Result<String, ErrorObjectOwned> {
        let (output, _gas_used) = self.execute_call(&tx)?;
        Ok(hex_bytes(&output))
    }

    async fn estimate_gas(
        &self,
        tx: crate::types::CallRequest,
    ) -> Result<String, ErrorObjectOwned> {
        let (_output, gas_used) = self.execute_call(&tx)?;
        // Add a 20% buffer to the estimated gas, with a minimum of 21000.
        let estimate = std::cmp::max((gas_used as f64 * 1.2) as u64, 21_000);
        Ok(hex_u64(estimate))
    }

    async fn get_code(
        &self,
        address: Address,
        _block: Option<String>,
    ) -> Result<String, ErrorObjectOwned> {
        let ws = self.world_state.read();
        let code_hash = ws.get_code_hash(&address).map_err(internal_err)?;
        match code_hash {
            Some(hash) => {
                let code = self
                    .chain_store
                    .get_code(&hash)
                    .map_err(internal_err)?;
                match code {
                    Some(bytes) => Ok(hex_bytes(&bytes)),
                    None => Ok("0x".into()),
                }
            }
            None => Ok("0x".into()),
        }
    }

    async fn get_storage_at(
        &self,
        address: Address,
        position: String,
        _block: Option<String>,
    ) -> Result<String, ErrorObjectOwned> {
        let key_u256 = parse_hex_u256(&position)?;
        let key = ShellHash::from(alloy_primitives::B256::from(key_u256));
        let ws = self.world_state.read();
        let value = ws.get_storage(&address, &key).map_err(internal_err)?;
        // Return as zero-padded 32-byte hex string.
        Ok(format!("0x{}", hex::encode(value.as_bytes())))
    }

    async fn get_logs(
        &self,
        raw_filter: RawLogFilter,
    ) -> Result<Vec<RpcLogWithMeta>, ErrorObjectOwned> {
        // Resolve "latest" block number.
        let head = self
            .chain_store
            .get_head_block()
            .map_err(internal_err)?;
        let latest = head.map(|b| b.number()).unwrap_or(0);

        let filter = raw_filter.into_filter(latest);

        let from = filter.from_block.unwrap_or(latest);
        let to = filter.to_block.unwrap_or(latest);

        if from > to {
            return Ok(vec![]);
        }

        // Cap range to prevent DoS.
        if to - from + 1 > MAX_BLOCK_RANGE {
            return Err(ErrorObjectOwned::owned(
                -32005,
                format!(
                    "query returned more than {} blocks; cap the range",
                    MAX_BLOCK_RANGE
                ),
                None::<()>,
            ));
        }

        let mut results = Vec::new();

        for block_num in from..=to {
            let block = match self
                .chain_store
                .get_block_by_number(block_num)
                .map_err(internal_err)?
            {
                Some(b) => b,
                None => continue,
            };

            // Fast path: check block-level bloom filter.
            if !filter.matches_bloom(block.header.logs_bloom.as_ref()) {
                continue;
            }

            let block_hash = block.hash();

            let receipts = self
                .chain_store
                .get_receipts(&block_hash)
                .map_err(internal_err)?
                .unwrap_or_default();

            // Global log index across all receipts in this block.
            let mut global_log_index: u64 = 0;

            for (tx_idx, receipt) in receipts.iter().enumerate() {
                // Per-receipt bloom fast path.
                if receipt.logs_bloom.len() == BLOOM_SIZE
                    && !filter.matches_bloom(receipt.logs_bloom.as_ref())
                {
                    global_log_index += receipt.logs.len() as u64;
                    continue;
                }

                for log in &receipt.logs {
                    if filter.matches_log(log) {
                        results.push(RpcLogWithMeta {
                            address: log.address,
                            topics: log.topics.clone(),
                            data: hex_bytes(log.data.as_ref()),
                            block_number: hex_u64(block_num),
                            block_hash,
                            transaction_hash: receipt.tx_hash,
                            transaction_index: hex_u64(tx_idx as u64),
                            log_index: hex_u64(global_log_index),
                            removed: false,
                        });
                    }
                    global_log_index += 1;
                }
            }
        }

        Ok(results)
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

    async fn send_transaction(
        &self,
        tx: SignedTransaction,
    ) -> Result<ShellHash, ErrorObjectOwned> {
        self.submit_tx(tx)
    }

    async fn get_validators(&self) -> Result<Vec<Address>, ErrorObjectOwned> {
        let ws = self.world_state.read();
        ws.get_validators().map_err(internal_err)
    }

    async fn add_validator(&self, address: String) -> Result<bool, ErrorObjectOwned> {
        tracing::warn!("shell_addValidator called without authentication — will be gated in M3");

        let addr = parse_address(&address)?;

        let mut ws = self.world_state.write();
        let mut validators = ws.get_validators().map_err(internal_err)?;

        if validators.contains(&addr) {
            return Err(ErrorObjectOwned::owned(
                -32602,
                format!("validator {address} is already in the set"),
                None::<()>,
            ));
        }

        validators.push(addr);
        ws.set_validators(&validators).map_err(internal_err)?;
        // Compute state root so trie changes are flushed to the backing store.
        let _ = ws.state_root().map_err(internal_err)?;

        tracing::info!("validator added: {address}");
        Ok(true)
    }

    async fn remove_validator(&self, address: String) -> Result<bool, ErrorObjectOwned> {
        tracing::warn!(
            "shell_removeValidator called without authentication — will be gated in M3"
        );

        let addr = parse_address(&address)?;

        let mut ws = self.world_state.write();
        let mut validators = ws.get_validators().map_err(internal_err)?;

        let pos = validators.iter().position(|v| v == &addr).ok_or_else(|| {
            ErrorObjectOwned::owned(
                -32602,
                format!("validator {address} is not in the set"),
                None::<()>,
            )
        })?;

        if validators.len() == 1 {
            return Err(ErrorObjectOwned::owned(
                -32602,
                "cannot remove the last validator",
                None::<()>,
            ));
        }

        validators.remove(pos);
        ws.set_validators(&validators).map_err(internal_err)?;
        let _ = ws.state_root().map_err(internal_err)?;

        tracing::info!("validator removed: {address}");
        Ok(true)
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
        let tx_pool = Arc::new(TxPool::new(shell_mempool::MempoolConfig {
            chain_id: 42,
            ..shell_mempool::MempoolConfig::default()
        }));
        let (block_events, _) = tokio::sync::broadcast::channel(16);
        RpcHandler::new(chain_store, world_state, tx_pool, 42, None, block_events)
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

    #[tokio::test]
    async fn tx_response_includes_vrs_compat_fields() {
        let handler = setup();
        let block = make_genesis_block();
        let hash = block.hash();
        handler.chain_store.put_block(&block).unwrap();
        handler.chain_store.set_canonical(0, &hash).unwrap();
        handler.chain_store.set_head(&hash).unwrap();

        // Verify block is stored, then check RpcTransaction v/r/s fields.
        let _block = EthApiServer::get_block_by_number(&handler, "latest".into(), false)
            .await
            .unwrap()
            .unwrap();
        // Directly construct an RpcTransaction to check compat fields.
        let rpc_tx = tx_to_rpc(
            &shell_core::SignedTransaction::new(
                Address::from_public_key(b"test"),
                Transaction {
                    chain_id: 42,
                    nonce: 0,
                    max_fee_per_gas: 1_000_000_000,
                    max_priority_fee_per_gas: 100_000_000,
                    gas_limit: 21_000,
                    to: None,
                    value: U256::ZERO,
                    data: Bytes::default(),
                },
                shell_crypto::PQSignature::new(
                    shell_crypto::SignatureType::Dilithium3,
                    vec![],
                ),
            ),
            None,
            None,
            None,
        );
        assert_eq!(rpc_tx.v, "0x0");
        assert_eq!(rpc_tx.r, "0x0");
        assert_eq!(rpc_tx.s, "0x0");
        assert_eq!(rpc_tx.tx_type, "0x2");
    }

    #[tokio::test]
    async fn send_raw_transaction_decodes_hex_json() {
        let handler = setup();
        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let addr = Address::from_public_key(&pubkey);

        // Fund the sender so balance check passes.
        {
            let mut ws = handler.world_state.write();
            ws.add_balance(&addr, U256::from(100_000_000_000_000u64)).unwrap();
        }
        // Register pubkey so mempool can verify.
        handler.chain_store.put_pubkey(&addr, &pubkey).unwrap();

        let tx = Transaction {
            chain_id: 42,
            nonce: 0,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 100_000_000,
            gas_limit: 21_000,
            to: None,
            value: U256::ZERO,
            data: Bytes::default(),
        };

        let signature = signer.sign(tx.hash().0.as_slice()).unwrap();
        let signed = SignedTransaction::new(addr, tx, signature);

        let json_bytes = serde_json::to_vec(&signed).unwrap();
        let hex_payload = format!("0x{}", hex::encode(&json_bytes));

        let result = EthApiServer::send_raw_transaction(&handler, hex_payload).await;
        assert!(result.is_ok(), "send_raw_transaction failed: {:?}", result.err());

        assert_eq!(handler.tx_pool.len(), 1);
    }

    #[tokio::test]
    async fn shell_send_transaction() {
        let handler = setup();
        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let addr = Address::from_public_key(&pubkey);

        {
            let mut ws = handler.world_state.write();
            ws.add_balance(&addr, U256::from(100_000_000_000_000u64)).unwrap();
        }
        handler.chain_store.put_pubkey(&addr, &pubkey).unwrap();

        let tx = Transaction {
            chain_id: 42,
            nonce: 0,
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 100_000_000,
            gas_limit: 21_000,
            to: None,
            value: U256::ZERO,
            data: Bytes::default(),
        };
        let signature = signer.sign(tx.hash().0.as_slice()).unwrap();
        let signed = SignedTransaction::new(addr, tx, signature);

        let result = ShellApiServer::send_transaction(&handler, signed).await;
        assert!(result.is_ok());
        assert_eq!(handler.tx_pool.len(), 1);
    }

    #[tokio::test]
    async fn send_raw_transaction_rejects_invalid_hex() {
        let handler = setup();
        let result = EthApiServer::send_raw_transaction(&handler, "not-hex".into()).await;
        assert!(result.is_err());
    }

    // ── New RPC methods ──────────────────────────────────────────

    #[tokio::test]
    async fn get_code_no_contract_returns_0x() {
        let handler = setup();
        let addr = Address::from_public_key(b"test-address");
        let result = EthApiServer::get_code(&handler, addr, None).await.unwrap();
        assert_eq!(result, "0x");
    }

    #[tokio::test]
    async fn get_code_returns_stored_bytecode() {
        let handler = setup();
        let addr = Address::from_public_key(b"contract-addr");
        let code = b"\x60\x00\x60\x00\xf3"; // PUSH1 0 PUSH1 0 RETURN
        let code_hash = shell_primitives::keccak256(code);

        // Store code and set code hash on the account.
        handler.chain_store.put_code(&code_hash, code).unwrap();
        {
            let mut ws = handler.world_state.write();
            ws.set_account(
                &addr,
                &shell_core::Account {
                    pq_pubkey_hash: ShellHash::default(),
                    nonce: 0,
                    balance: U256::ZERO,
                    validation_code_hash: None,
                    code_hash: Some(code_hash),
                    storage_root: ShellHash::ZERO,
                },
            )
            .unwrap();
        }

        let result = EthApiServer::get_code(&handler, addr, None).await.unwrap();
        assert_eq!(result, format!("0x{}", hex::encode(code)));
    }

    #[tokio::test]
    async fn get_storage_at_empty_returns_zero() {
        let handler = setup();
        let addr = Address::from_public_key(b"test-address");
        let result = EthApiServer::get_storage_at(&handler, addr, "0x0".into(), None)
            .await
            .unwrap();
        // 32 zero bytes, hex-encoded.
        assert_eq!(
            result,
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        );
    }

    #[tokio::test]
    async fn get_storage_at_returns_stored_value() {
        let handler = setup();
        let addr = Address::from_public_key(b"storage-test");
        let slot = ShellHash::from(alloy_primitives::B256::from(U256::from(1)));
        let value = ShellHash::from(alloy_primitives::B256::from(U256::from(42)));

        {
            let mut ws = handler.world_state.write();
            ws.set_account(
                &addr,
                &shell_core::Account {
                    pq_pubkey_hash: ShellHash::default(),
                    nonce: 0,
                    balance: U256::ZERO,
                    validation_code_hash: None,
                    code_hash: None,
                    storage_root: ShellHash::ZERO,
                },
            )
            .unwrap();
            ws.set_storage(&addr, &slot, &value).unwrap();
        }

        let result = EthApiServer::get_storage_at(&handler, addr, "0x1".into(), None)
            .await
            .unwrap();
        assert_eq!(
            result,
            "0x000000000000000000000000000000000000000000000000000000000000002a"
        );
    }

    #[tokio::test]
    async fn eth_call_simple_transfer() {
        let handler = setup();
        let from = Address::from_public_key(b"caller-key");

        // Fund the caller.
        {
            let mut ws = handler.world_state.write();
            ws.add_balance(&from, U256::from(10_000_000_000u64)).unwrap();
        }

        let req = crate::types::CallRequest {
            from: Some(from),
            to: Some(Address::from([0x01; 20])),
            data: None,
            value: Some("0x3e8".into()), // 1000
            gas: Some("0x5208".into()),  // 21000
        };
        let result = EthApiServer::call(&handler, req, None).await;
        assert!(result.is_ok(), "eth_call failed: {:?}", result.err());
        // Transfer returns empty data.
        assert_eq!(result.unwrap(), "0x");
    }

    #[tokio::test]
    async fn eth_estimate_gas_simple_transfer() {
        let handler = setup();
        let from = Address::from_public_key(b"caller-key");

        {
            let mut ws = handler.world_state.write();
            ws.add_balance(&from, U256::from(10_000_000_000u64)).unwrap();
        }

        let req = crate::types::CallRequest {
            from: Some(from),
            to: Some(Address::from([0x01; 20])),
            data: None,
            value: Some("0x3e8".into()),
            gas: None,
        };
        let result = EthApiServer::estimate_gas(&handler, req).await;
        assert!(result.is_ok(), "estimateGas failed: {:?}", result.err());
        let gas_hex = result.unwrap();
        let gas = u64::from_str_radix(gas_hex.strip_prefix("0x").unwrap(), 16).unwrap();
        assert!(gas >= 21_000, "estimated gas too low: {gas}");
    }

    // ── eth_getLogs tests ────────────────────────────────────────

    /// Helper: store a block with receipts that contain logs and return the block hash.
    fn store_block_with_logs(
        handler: &RpcHandler<MemoryDb>,
        number: u64,
        logs_per_receipt: Vec<Vec<shell_core::Log>>,
    ) -> ShellHash {
        let bloom = shell_evm::bloom::logs_bloom(
            &logs_per_receipt.iter().flatten().cloned().collect::<Vec<_>>(),
        );

        let block = Block {
            header: BlockHeader {
                parent_hash: ShellHash::default(),
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::copy_from_slice(&bloom),
                number,
                gas_limit: 30_000_000,
                gas_used: 21_000 * logs_per_receipt.len() as u64,
                timestamp: 1_700_000_000 + number,
                extra_data: Bytes::default(),
                proposer: Address::from_public_key(b"proposer-key-data"),
                sig_aggregate_proof: None,
            },
            transactions: vec![],
            proposer_seal: None,
        };
        let hash = block.hash();
        handler.chain_store.put_block(&block).unwrap();
        handler.chain_store.set_canonical(number, &hash).unwrap();
        handler.chain_store.set_head(&hash).unwrap();

        let mut cumulative_gas = 0u64;
        let receipts: Vec<TransactionReceipt> = logs_per_receipt
            .into_iter()
            .enumerate()
            .map(|(i, logs)| {
                let receipt_bloom = shell_evm::bloom::logs_bloom(&logs);
                cumulative_gas += 21_000;
                TransactionReceipt {
                    tx_hash: ShellHash::from_slice(&[i as u8 + 1; 32]),
                    block_number: number,
                    tx_index: i as u32,
                    status: 1,
                    gas_used: 21_000,
                    cumulative_gas_used: cumulative_gas,
                    contract_address: None,
                    logs_bloom: Bytes::copy_from_slice(&receipt_bloom),
                    logs,
                }
            })
            .collect();

        handler.chain_store.put_receipts(&hash, &receipts).unwrap();
        hash
    }

    #[tokio::test]
    async fn get_logs_empty_range_returns_empty() {
        let handler = setup();
        let raw: crate::filter::RawLogFilter =
            serde_json::from_str(r#"{"fromBlock":"0x5","toBlock":"0x1"}"#).unwrap();
        let result = EthApiServer::get_logs(&handler, raw).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn get_logs_no_blocks_returns_empty() {
        let handler = setup();
        let raw: crate::filter::RawLogFilter =
            serde_json::from_str(r#"{"fromBlock":"0x0","toBlock":"0x0"}"#).unwrap();
        let result = EthApiServer::get_logs(&handler, raw).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn get_logs_matches_specific_address() {
        let handler = setup();
        let target = Address::from([0xAA; 20]);
        let other = Address::from([0xBB; 20]);

        let log_target =
            shell_core::Log::new(target, vec![], Bytes::new()).unwrap();
        let log_other =
            shell_core::Log::new(other, vec![], Bytes::new()).unwrap();

        store_block_with_logs(&handler, 0, vec![vec![log_target, log_other]]);

        let raw: crate::filter::RawLogFilter = serde_json::from_str(&format!(
            r#"{{"fromBlock":"0x0","toBlock":"0x0","address":"{}"}}"#,
            target,
        ))
        .unwrap();

        let result = EthApiServer::get_logs(&handler, raw).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].address, target);
        assert!(!result[0].removed);
    }

    #[tokio::test]
    async fn get_logs_topic_filtering() {
        let handler = setup();
        let topic_a = ShellHash::from_slice(&[0x11; 32]);
        let topic_b = ShellHash::from_slice(&[0x22; 32]);

        let log_a = shell_core::Log::new(
            Address::from([0x01; 20]),
            vec![topic_a],
            Bytes::new(),
        )
        .unwrap();
        let log_b = shell_core::Log::new(
            Address::from([0x01; 20]),
            vec![topic_b],
            Bytes::new(),
        )
        .unwrap();

        store_block_with_logs(&handler, 0, vec![vec![log_a, log_b]]);

        // Filter for topic_a only
        let raw: crate::filter::RawLogFilter = serde_json::from_str(&format!(
            r#"{{"fromBlock":"0x0","toBlock":"0x0","topics":["{}"]}}"#,
            topic_a,
        ))
        .unwrap();

        let result = EthApiServer::get_logs(&handler, raw).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].topics[0], topic_a);
    }

    #[tokio::test]
    async fn get_logs_bloom_fast_path_skips_block() {
        let handler = setup();
        // Block contains log from address 0xBB only.
        let other = Address::from([0xBB; 20]);
        let log = shell_core::Log::new(other, vec![], Bytes::new()).unwrap();
        store_block_with_logs(&handler, 0, vec![vec![log]]);

        // Query for address 0xAA — bloom should reject the block.
        let target = Address::from([0xAA; 20]);
        let raw: crate::filter::RawLogFilter = serde_json::from_str(&format!(
            r#"{{"fromBlock":"0x0","toBlock":"0x0","address":"{}"}}"#,
            target,
        ))
        .unwrap();

        let result = EthApiServer::get_logs(&handler, raw).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn get_logs_range_too_large_returns_error() {
        let handler = setup();
        let raw: crate::filter::RawLogFilter = serde_json::from_str(
            r#"{"fromBlock":"0x0","toBlock":"0x3e9"}"#, // 0..1001 = 1002 blocks
        )
        .unwrap();

        let result = EthApiServer::get_logs(&handler, raw).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message().contains("cap the range"));
    }

    #[tokio::test]
    async fn get_logs_metadata_fields_are_correct() {
        let handler = setup();
        let addr = Address::from([0xCC; 20]);
        let topic = ShellHash::from_slice(&[0xDD; 32]);
        let log = shell_core::Log::new(
            addr,
            vec![topic],
            Bytes::copy_from_slice(b"\x01\x02"),
        )
        .unwrap();
        let block_hash = store_block_with_logs(&handler, 1, vec![vec![log]]);

        let raw: crate::filter::RawLogFilter =
            serde_json::from_str(r#"{"fromBlock":"0x1","toBlock":"0x1"}"#).unwrap();
        let result = EthApiServer::get_logs(&handler, raw).await.unwrap();
        assert_eq!(result.len(), 1);
        let entry = &result[0];
        assert_eq!(entry.block_number, "0x1");
        assert_eq!(entry.block_hash, block_hash);
        assert_eq!(entry.transaction_index, "0x0");
        assert_eq!(entry.log_index, "0x0");
        assert_eq!(entry.data, "0x0102");
        assert!(!entry.removed);
    }

    #[tokio::test]
    async fn shell_get_validators_empty() {
        let handler = setup();
        let result = ShellApiServer::get_validators(&handler).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn shell_get_validators_with_data() {
        let db = Arc::new(MemoryDb::new());
        let chain_store = Arc::new(ChainStore::new(db.clone()));
        let ws = Arc::new(parking_lot::RwLock::new(WorldState::new(db)));
        let tx_pool = Arc::new(TxPool::new(shell_mempool::MempoolConfig {
            chain_id: 42,
            ..shell_mempool::MempoolConfig::default()
        }));
        let (block_events, _) = tokio::sync::broadcast::channel(16);
        let handler = RpcHandler::new(
            chain_store,
            Arc::clone(&ws),
            tx_pool,
            42,
            None,
            block_events,
        );

        let v1 = Address::from([0x11; 20]);
        let v2 = Address::from([0x22; 20]);
        {
            let mut w = ws.write();
            w.set_validators(&[v1, v2]).unwrap();
        }
        let result = ShellApiServer::get_validators(&handler).await.unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0], v1);
        assert_eq!(result[1], v2);
    }

    #[tokio::test]
    async fn shell_add_validator() {
        let handler = setup();
        let addr_hex = format!("0x{}", "ab".repeat(20));

        let ok = ShellApiServer::add_validator(&handler, addr_hex.clone())
            .await
            .unwrap();
        assert!(ok);

        let validators = ShellApiServer::get_validators(&handler).await.unwrap();
        assert_eq!(validators.len(), 1);
        assert_eq!(validators[0], Address::from([0xAB; 20]));
    }

    #[tokio::test]
    async fn shell_add_validator_duplicate_error() {
        let handler = setup();
        let addr_hex = format!("0x{}", "ab".repeat(20));

        ShellApiServer::add_validator(&handler, addr_hex.clone())
            .await
            .unwrap();
        let err = ShellApiServer::add_validator(&handler, addr_hex)
            .await
            .unwrap_err();
        assert!(err.message().contains("already in the set"));
    }

    #[tokio::test]
    async fn shell_remove_validator() {
        let handler = setup();
        let a1 = format!("0x{}", "aa".repeat(20));
        let a2 = format!("0x{}", "bb".repeat(20));

        ShellApiServer::add_validator(&handler, a1.clone())
            .await
            .unwrap();
        ShellApiServer::add_validator(&handler, a2.clone())
            .await
            .unwrap();

        let ok = ShellApiServer::remove_validator(&handler, a1.clone())
            .await
            .unwrap();
        assert!(ok);

        let validators = ShellApiServer::get_validators(&handler).await.unwrap();
        assert_eq!(validators.len(), 1);
        assert_eq!(validators[0], Address::from([0xBB; 20]));
    }

    #[tokio::test]
    async fn shell_remove_last_validator_error() {
        let handler = setup();
        let addr_hex = format!("0x{}", "cc".repeat(20));

        ShellApiServer::add_validator(&handler, addr_hex.clone())
            .await
            .unwrap();
        let err = ShellApiServer::remove_validator(&handler, addr_hex)
            .await
            .unwrap_err();
        assert!(err.message().contains("cannot remove the last validator"));
    }
}
