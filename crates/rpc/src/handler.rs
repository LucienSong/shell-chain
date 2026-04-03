//! RPC handler implementation backed by chain storage, world state, and mempool.

use std::sync::Arc;
use std::time::Instant;

use jsonrpsee::types::ErrorObjectOwned;

use shell_core::{Block, BlockHeader, SignedTransaction, Transaction};
use shell_evm::bloom::BLOOM_SIZE;
use shell_crypto::{DilithiumVerifier, Signer};
use shell_evm::{ShellEvm, ShellStateDb};
use shell_mempool::TxPool;
use shell_primitives::{Address, Bytes, ShellHash, U256};
use shell_storage::{ChainStore, KvStore, WorldState};

use crate::api::{EthApiServer, ShellApiServer, Web3ApiServer, NetApiServer};
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
    /// Optional signer for governance proposals (set when node is a validator).
    proposer_signer: Option<Arc<dyn Signer>>,
    /// Address of the proposer (derived from the signer's public key).
    proposer_address: Option<Address>,
    /// Timestamp when the RPC handler was created, used for uptime calculation.
    start_time: Instant,
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
            proposer_signer: self.proposer_signer.clone(),
            proposer_address: self.proposer_address,
            start_time: self.start_time,
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
            proposer_signer: None,
            proposer_address: None,
            start_time: Instant::now(),
        }
    }

    /// Set the proposer signer for governance RPCs.
    /// When set, enables `shell_proposeAddValidator` and `shell_proposeRemoveValidator`.
    pub fn with_proposer(mut self, signer: Arc<dyn Signer>, address: Address) -> Self {
        self.proposer_signer = Some(signer);
        self.proposer_address = Some(address);
        self
    }

    /// Returns a reference to the block event broadcast sender.
    pub fn block_event_sender(&self) -> &tokio::sync::broadcast::Sender<BlockEvent> {
        &self.block_events
    }

    /// Validate and submit a signed transaction to the mempool.
    /// On success, also forwards the transaction to the network broadcast channel
    /// (if one was provided) so peers can include it in their mempools.
    fn submit_tx(&self, signed_tx: SignedTransaction) -> Result<ShellHash, ErrorObjectOwned> {
        // EIP-1559: warn (and reject) if max_fee below current base_fee.
        if let Ok(Some(head)) = self.chain_store.get_head_block() {
            let current_base_fee = head.header.base_fee_per_gas;
            if current_base_fee > 0 && signed_tx.tx.max_fee_per_gas < current_base_fee {
                return Err(ErrorObjectOwned::owned(
                    -32000,
                    format!(
                        "max fee per gas ({}) below current base fee ({})",
                        signed_tx.tx.max_fee_per_gas, current_base_fee
                    ),
                    None::<()>,
                ));
            }
        }

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

    /// Build, sign, and submit a governance transaction to the ValidatorRegistry.
    /// Returns the transaction hash on success.
    fn propose_validator_tx(&self, calldata: Vec<u8>) -> Result<ShellHash, ErrorObjectOwned> {
        let signer = self.proposer_signer.as_ref().ok_or_else(|| {
            ErrorObjectOwned::owned(
                -32601,
                "node is not configured as a validator",
                None::<()>,
            )
        })?;
        let proposer_addr = self.proposer_address.ok_or_else(|| {
            ErrorObjectOwned::owned(
                -32601,
                "node is not configured as a validator",
                None::<()>,
            )
        })?;

        let nonce = {
            let ws = self.world_state.read();
            ws.get_nonce(&proposer_addr).map_err(internal_err)?
        };

        let tx = Transaction {
            chain_id: self.chain_id,
            nonce,
            to: Some(shell_evm::registry_address()),
            value: U256::ZERO,
            data: Bytes::copy_from_slice(&calldata),
            gas_limit: 100_000,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
        };

        let tx_hash = tx.hash();
        let signature = signer
            .sign(tx_hash.0.as_slice())
            .map_err(|e| internal_err(format!("signing failed: {e}")))?;

        let pubkey = signer.public_key().to_vec();
        let signed_tx = SignedTransaction::with_pubkey(
            proposer_addr,
            tx,
            signature,
            pubkey,
        );

        self.submit_tx(signed_tx)
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
            base_fee_per_gas: 0,
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
        base_fee_per_gas: hex_u64(block.header.base_fee_per_gas),
    }
}

/// Convert a SignedTransaction to an RpcTransaction response.
fn tx_to_rpc(
    tx: &SignedTransaction,
    block_hash: Option<ShellHash>,
    block_number: Option<u64>,
    tx_index: Option<u32>,
    base_fee: Option<u64>,
) -> RpcTransaction {
    // EIP-1559: mined txs report effective gas price; pending txs report max_fee
    let gas_price = match base_fee {
        Some(base) => shell_core::effective_gas_price(
            tx.tx.max_fee_per_gas,
            tx.tx.max_priority_fee_per_gas,
            base,
        ),
        None => tx.tx.max_fee_per_gas,
    };
    RpcTransaction {
        hash: tx.hash(),
        block_hash,
        block_number: block_number.map(hex_u64),
        transaction_index: tx_index.map(|i| hex_u64(i as u64)),
        from: tx.sender(),
        to: tx.tx.to,
        value: hex_u256(tx.tx.value),
        gas: hex_u64(tx.tx.gas_limit),
        gas_price: hex_u64(gas_price),
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

    async fn syncing(&self) -> Result<serde_json::Value, ErrorObjectOwned> {
        // Shell-chain has no sync protocol yet; always report "not syncing".
        Ok(serde_json::Value::Bool(false))
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
            return Ok(Some(tx_to_rpc(&pending_tx, None, None, None, None)));
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
                        Some(block.header.base_fee_per_gas),
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
        // Return the base fee from the latest block, or INITIAL_BASE_FEE if no blocks exist.
        let base_fee = match self.chain_store.get_head_block() {
            Ok(Some(head)) if head.header.base_fee_per_gas > 0 => head.header.base_fee_per_gas,
            _ => shell_core::INITIAL_BASE_FEE,
        };
        Ok(hex_u64(base_fee))
    }

    async fn max_priority_fee_per_gas(&self) -> Result<String, ErrorObjectOwned> {
        // PoA chain: no fee market competition, priority fee is always 0.
        Ok(hex_u64(0))
    }

    async fn fee_history(
        &self,
        block_count: String,
        newest_block: String,
        _reward_percentiles: Option<Vec<f64>>,
    ) -> Result<serde_json::Value, ErrorObjectOwned> {
        let latest = match parse_block_number(&newest_block)? {
            Some(n) => n,
            None => {
                // "latest" — get head block number
                match self.chain_store.get_head_block() {
                    Ok(Some(head)) => head.header.number,
                    _ => 0,
                }
            }
        };

        let count = parse_hex_u64(&block_count)?.min(1024);

        let oldest = latest.saturating_sub(count.saturating_sub(1));

        let mut base_fee_per_gas = Vec::new();
        let mut gas_used_ratio = Vec::new();

        for num in oldest..=latest {
            match self.chain_store.get_block_by_number(num) {
                Ok(Some(block)) => {
                    let h = &block.header;
                    base_fee_per_gas.push(hex_u64(h.base_fee_per_gas));
                    let ratio = if h.gas_limit > 0 {
                        h.gas_used as f64 / h.gas_limit as f64
                    } else {
                        0.0
                    };
                    gas_used_ratio.push(ratio);
                }
                _ => {
                    base_fee_per_gas.push(hex_u64(0));
                    gas_used_ratio.push(0.0);
                }
            }
        }

        // Append next block's predicted base fee (one more entry than gas_used_ratio).
        if let Ok(Some(head)) = self.chain_store.get_block_by_number(latest) {
            let next = shell_core::fee::calculate_base_fee(
                head.header.gas_used,
                head.header.gas_limit,
                head.header.base_fee_per_gas,
            );
            base_fee_per_gas.push(hex_u64(next));
        } else {
            base_fee_per_gas.push(hex_u64(shell_core::INITIAL_BASE_FEE));
        }

        Ok(serde_json::json!({
            "oldestBlock": hex_u64(oldest),
            "baseFeePerGas": base_fee_per_gas,
            "gasUsedRatio": gas_used_ratio,
            "reward": []
        }))
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

    async fn add_validator(&self, _address: String) -> Result<bool, ErrorObjectOwned> {
        // DISABLED (F-039/F-040): Direct WorldState mutation via RPC causes
        // split-brain — validator changes must go through a system contract
        // transaction so all nodes compute the same state_root deterministically.
        // Use shell_proposeAddValidator instead.
        Err(ErrorObjectOwned::owned(
            -32601,
            "shell_addValidator is disabled: use shell_proposeAddValidator instead",
            None::<()>,
        ))
    }

    async fn remove_validator(&self, _address: String) -> Result<bool, ErrorObjectOwned> {
        // DISABLED (F-039/F-040): See add_validator rationale.
        // Use shell_proposeRemoveValidator instead.
        Err(ErrorObjectOwned::owned(
            -32601,
            "shell_removeValidator is disabled: use shell_proposeRemoveValidator instead",
            None::<()>,
        ))
    }

    async fn encode_add_validator(&self, address: String) -> Result<String, ErrorObjectOwned> {
        let addr = parse_address(&address)?;
        let calldata = shell_evm::encode_add_validator_calldata(&addr);
        Ok(format!("0x{}", hex::encode(calldata)))
    }

    async fn encode_remove_validator(&self, address: String) -> Result<String, ErrorObjectOwned> {
        let addr = parse_address(&address)?;
        let calldata = shell_evm::encode_remove_validator_calldata(&addr);
        Ok(format!("0x{}", hex::encode(calldata)))
    }

    async fn propose_add_validator(&self, address: String) -> Result<String, ErrorObjectOwned> {
        let addr = parse_address(&address)?;
        let calldata = shell_evm::encode_add_validator_calldata(&addr);
        let hash = self.propose_validator_tx(calldata)?;
        Ok(format!("0x{}", hex::encode(hash.0)))
    }

    async fn propose_remove_validator(&self, address: String) -> Result<String, ErrorObjectOwned> {
        let addr = parse_address(&address)?;
        let calldata = shell_evm::encode_remove_validator_calldata(&addr);
        let hash = self.propose_validator_tx(calldata)?;
        Ok(format!("0x{}", hex::encode(hash.0)))
    }

    async fn get_validator_status(
        &self,
        address: Address,
    ) -> Result<serde_json::Value, ErrorObjectOwned> {
        let ws = self.world_state.read();
        let validators = ws.get_validators().map_err(internal_err)?;
        let is_validator = validators.contains(&address);
        Ok(serde_json::json!({
            "address": address,
            "isValidator": is_validator,
        }))
    }

    async fn get_governance_info(&self) -> Result<serde_json::Value, ErrorObjectOwned> {
        let ws = self.world_state.read();
        let validators = ws.get_validators().map_err(internal_err)?;
        Ok(serde_json::json!({
            "validatorCount": validators.len(),
            "validators": validators,
            "systemContractAddress": shell_evm::registry_address(),
            "proposalGasLimit": 100_000,
        }))
    }

    async fn estimate_governance_gas(&self, operation: String) -> Result<String, ErrorObjectOwned> {
        let gas = match operation.as_str() {
            "addValidator" | "removeValidator" => {
                shell_evm::SYSTEM_CALL_BASE_GAS + shell_evm::SYSTEM_CALL_OP_GAS
            }
            "getValidators" | "isValidator" => shell_evm::SYSTEM_CALL_BASE_GAS,
            _ => {
                return Err(ErrorObjectOwned::owned(
                    -32602,
                    format!("unknown governance operation: {operation}"),
                    None::<()>,
                ));
            }
        };
        Ok(hex_u64(gas))
    }

    async fn get_node_info(&self) -> Result<serde_json::Value, ErrorObjectOwned> {
        let head = self.chain_store.get_head_block().map_err(internal_err)?;
        let block_height = head.as_ref().map(|b| b.number()).unwrap_or(0);
        let base_fee = match &head {
            Some(h) if h.header.base_fee_per_gas > 0 => h.header.base_fee_per_gas,
            _ => shell_core::INITIAL_BASE_FEE,
        };

        Ok(serde_json::json!({
            "version": "ShellChain/v0.1.0/rust",
            "chainId": self.chain_id,
            "blockHeight": block_height,
            "peerCount": 0,
            "txPoolSize": self.tx_pool.len(),
            "isMining": self.proposer_signer.is_some(),
            "uptime": self.start_time.elapsed().as_secs(),
            "baseFee": hex_u64(base_fee),
        }))
    }

    async fn get_network_stats(&self) -> Result<serde_json::Value, ErrorObjectOwned> {
        Ok(serde_json::json!({
            "peerCount": 0,
            "protocolVersion": "shell/1.0.0",
            "listeningAddress": "/ip4/0.0.0.0/tcp/30303",
            "protocols": ["gossipsub", "kademlia", "mdns"],
        }))
    }

    async fn get_chain_stats(&self) -> Result<serde_json::Value, ErrorObjectOwned> {
        let head = self.chain_store.get_head_block().map_err(internal_err)?;
        let block_height = head.as_ref().map(|b| b.number()).unwrap_or(0);
        let base_fee = match &head {
            Some(h) if h.header.base_fee_per_gas > 0 => h.header.base_fee_per_gas,
            _ => shell_core::INITIAL_BASE_FEE,
        };

        let mut total_txs: u64 = 0;
        let mut gas_used_total = U256::ZERO;
        let mut avg_block_time: f64 = 0.0;

        if block_height > 0 {
            for n in 0..=block_height {
                if let Ok(Some(blk)) = self.chain_store.get_block_by_number(n) {
                    total_txs += blk.transactions.len() as u64;
                    gas_used_total += U256::from(blk.header.gas_used);
                }
            }

            let window = std::cmp::min(block_height, 10);
            if window >= 1 {
                if let (Ok(Some(recent)), Ok(Some(older))) = (
                    self.chain_store.get_block_by_number(block_height),
                    self.chain_store.get_block_by_number(block_height - window),
                ) {
                    let dt = recent.header.timestamp.saturating_sub(older.header.timestamp);
                    avg_block_time = dt as f64 / window as f64;
                }
            }
        }

        Ok(serde_json::json!({
            "blockHeight": block_height,
            "totalTransactions": total_txs,
            "avgBlockTime": avg_block_time,
            "gasUsedTotal": hex_u256(gas_used_total),
            "latestBaseFee": hex_u64(base_fee),
        }))
    }
}

#[jsonrpsee::core::async_trait]
impl<S: KvStore + 'static> Web3ApiServer for RpcHandler<S> {
    async fn client_version(&self) -> Result<String, ErrorObjectOwned> {
        Ok("ShellChain/v0.1.0/rust".to_string())
    }

    async fn sha3(&self, data: String) -> Result<String, ErrorObjectOwned> {
        let raw = data.strip_prefix("0x").unwrap_or(&data);
        // Limit input to 32 KB to prevent DoS via large allocations.
        const MAX_HEX_LEN: usize = 32 * 1024 * 2; // 32 KB decoded = 64 KB hex
        if raw.len() > MAX_HEX_LEN {
            return Err(internal_err("input too large (max 32 KB)"));
        }
        let bytes = hex::decode(raw)
            .map_err(|e| internal_err(format!("invalid hex: {e}")))?;
        let hash = shell_primitives::keccak256(&bytes);
        Ok(format!("0x{}", hex::encode(hash.0)))
    }
}

#[jsonrpsee::core::async_trait]
impl<S: KvStore + 'static> NetApiServer for RpcHandler<S> {
    async fn version(&self) -> Result<String, ErrorObjectOwned> {
        Ok(self.chain_id.to_string())
    }

    async fn listening(&self) -> Result<bool, ErrorObjectOwned> {
        Ok(true)
    }

    async fn peer_count(&self) -> Result<String, ErrorObjectOwned> {
        // No peer tracking yet; report 0 peers.
        Ok(hex_u64(0))
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
                base_fee_per_gas: 0,
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
        // No blocks stored → returns INITIAL_BASE_FEE (1 gwei)
        assert_eq!(result, "0x3b9aca00");
    }

    #[tokio::test]
    async fn gas_price_returns_latest_base_fee() {
        let handler = setup();
        let mut block = make_genesis_block();
        block.header.base_fee_per_gas = 2_000_000_000; // 2 gwei
        block.header.number = 1;
        let hash = block.hash();
        handler.chain_store.put_block(&block).unwrap();
        handler.chain_store.set_canonical(1, &hash).unwrap();
        handler.chain_store.set_head(&hash).unwrap();

        let result = EthApiServer::gas_price(&handler).await.unwrap();
        assert_eq!(result, "0x77359400"); // 2 gwei
    }

    #[tokio::test]
    async fn max_priority_fee_per_gas_returns_zero() {
        let handler = setup();
        let result = EthApiServer::max_priority_fee_per_gas(&handler).await.unwrap();
        assert_eq!(result, "0x0");
    }

    #[tokio::test]
    async fn fee_history_returns_base_fees() {
        let handler = setup();
        let mut block = make_genesis_block();
        block.header.base_fee_per_gas = 1_000_000_000;
        block.header.number = 0;
        let hash = block.hash();
        handler.chain_store.put_block(&block).unwrap();
        handler.chain_store.set_canonical(0, &hash).unwrap();
        handler.chain_store.set_head(&hash).unwrap();

        let result = EthApiServer::fee_history(&handler, "0x1".into(), "latest".into(), None)
            .await
            .unwrap();
        let base_fees = result["baseFeePerGas"].as_array().unwrap();
        // Should have 2 entries: block 0 + predicted next block
        assert_eq!(base_fees.len(), 2);
        assert_eq!(base_fees[0].as_str().unwrap(), "0x3b9aca00");
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
                base_fee_per_gas: 0,
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
    async fn shell_add_validator_disabled() {
        let handler = setup();
        let addr_hex = format!("0x{}", "ab".repeat(20));

        let err = ShellApiServer::add_validator(&handler, addr_hex)
            .await
            .unwrap_err();
        assert!(err.message().contains("disabled"));
    }

    #[tokio::test]
    async fn shell_remove_validator_disabled() {
        let handler = setup();
        let addr_hex = format!("0x{}", "cc".repeat(20));

        let err = ShellApiServer::remove_validator(&handler, addr_hex)
            .await
            .unwrap_err();
        assert!(err.message().contains("disabled"));
    }

    // ── Governance proposal RPCs ─────────────────────────────────

    fn setup_with_proposer() -> (RpcHandler<MemoryDb>, DilithiumSigner, Address) {
        let db = Arc::new(MemoryDb::new());
        let chain_store = Arc::new(ChainStore::new(db.clone()));
        let world_state = Arc::new(parking_lot::RwLock::new(WorldState::new(db)));
        let tx_pool = Arc::new(TxPool::new(shell_mempool::MempoolConfig {
            chain_id: 42,
            ..shell_mempool::MempoolConfig::default()
        }));
        let (block_events, _) = tokio::sync::broadcast::channel(16);

        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let addr = Address::from_public_key(&pubkey);

        let handler = RpcHandler::new(
            chain_store.clone(),
            world_state,
            tx_pool,
            42,
            None,
            block_events,
        )
        .with_proposer(Arc::new(DilithiumSigner::from_bytes(
            signer.public_key(),
            signer.secret_key_bytes(),
        ).unwrap()), addr);

        // Register pubkey so mempool signature verification passes.
        handler.chain_store.put_pubkey(&addr, &pubkey).unwrap();

        (handler, signer, addr)
    }

    #[tokio::test]
    async fn propose_add_validator_no_signer_returns_error() {
        let handler = setup();
        let target = format!("0x{}", "ab".repeat(20));
        let err = ShellApiServer::propose_add_validator(&handler, target)
            .await
            .unwrap_err();
        assert!(err.message().contains("not configured as a validator"));
    }

    #[tokio::test]
    async fn propose_remove_validator_no_signer_returns_error() {
        let handler = setup();
        let target = format!("0x{}", "ab".repeat(20));
        let err = ShellApiServer::propose_remove_validator(&handler, target)
            .await
            .unwrap_err();
        assert!(err.message().contains("not configured as a validator"));
    }

    #[tokio::test]
    async fn propose_add_validator_creates_correct_tx() {
        let (handler, _signer, _addr) = setup_with_proposer();
        let target = format!("0x{}", "ab".repeat(20));
        let result = ShellApiServer::propose_add_validator(&handler, target.clone())
            .await;
        assert!(result.is_ok(), "proposeAddValidator failed: {:?}", result.err());

        // Verify a transaction was inserted into the mempool.
        assert_eq!(handler.tx_pool.len(), 1);

        // Verify the transaction has the correct calldata.
        let target_addr = parse_address(&target).unwrap();
        let expected_calldata = shell_evm::encode_add_validator_calldata(&target_addr);
        let pending = handler.tx_pool.pending(100);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tx.data.as_ref(), expected_calldata.as_slice());
        assert_eq!(pending[0].tx.to, Some(shell_evm::registry_address()));
        assert_eq!(pending[0].tx.value, U256::ZERO);
        assert_eq!(pending[0].tx.chain_id, 42);
        assert_eq!(pending[0].tx.nonce, 0);
    }

    #[tokio::test]
    async fn propose_remove_validator_creates_correct_tx() {
        let (handler, _signer, _addr) = setup_with_proposer();
        let target = format!("0x{}", "cc".repeat(20));
        let result = ShellApiServer::propose_remove_validator(&handler, target.clone())
            .await;
        assert!(result.is_ok(), "proposeRemoveValidator failed: {:?}", result.err());

        assert_eq!(handler.tx_pool.len(), 1);

        let target_addr = parse_address(&target).unwrap();
        let expected_calldata = shell_evm::encode_remove_validator_calldata(&target_addr);
        let pending = handler.tx_pool.pending(100);
        assert_eq!(pending[0].tx.data.as_ref(), expected_calldata.as_slice());
    }

    #[tokio::test]
    async fn propose_add_validator_uses_correct_nonce() {
        let (handler, _signer, addr) = setup_with_proposer();

        // Set the proposer nonce to 5.
        {
            let mut ws = handler.world_state.write();
            for _ in 0..5 {
                ws.increment_nonce(&addr).unwrap();
            }
        }

        let target = format!("0x{}", "ab".repeat(20));
        let result = ShellApiServer::propose_add_validator(&handler, target)
            .await;
        assert!(result.is_ok(), "proposeAddValidator failed: {:?}", result.err());

        let pending = handler.tx_pool.pending(100);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].tx.nonce, 5);
    }

    #[tokio::test]
    async fn propose_add_validator_returns_tx_hash_hex() {
        let (handler, _signer, _addr) = setup_with_proposer();
        let target = format!("0x{}", "ab".repeat(20));
        let result = ShellApiServer::propose_add_validator(&handler, target)
            .await
            .unwrap();
        // Must be a hex string starting with 0x, 32 bytes = 66 chars.
        assert!(result.starts_with("0x"));
        assert_eq!(result.len(), 66);
    }

    // ── web3_* tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn web3_client_version() {
        let handler = setup();
        let result = Web3ApiServer::client_version(&handler).await.unwrap();
        assert_eq!(result, "ShellChain/v0.1.0/rust");
    }

    #[tokio::test]
    async fn web3_sha3_known_vector() {
        let handler = setup();
        // keccak256("") = c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        let result = Web3ApiServer::sha3(&handler, "0x".to_string()).await.unwrap();
        assert_eq!(
            result,
            "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }

    #[tokio::test]
    async fn web3_sha3_hello() {
        let handler = setup();
        let input = format!("0x{}", hex::encode(b"hello"));
        let result = Web3ApiServer::sha3(&handler, input).await.unwrap();
        let expected = shell_primitives::keccak256(b"hello");
        assert_eq!(result, format!("0x{}", hex::encode(expected.0)));
    }

    // ── net_* tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn net_version_returns_chain_id_decimal() {
        let handler = setup();
        // setup() uses chain_id = 42
        let result = NetApiServer::version(&handler).await.unwrap();
        assert_eq!(result, "42");
    }

    #[tokio::test]
    async fn net_listening_returns_true() {
        let handler = setup();
        let result = NetApiServer::listening(&handler).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn net_peer_count_returns_hex() {
        let handler = setup();
        let result = NetApiServer::peer_count(&handler).await.unwrap();
        assert_eq!(result, "0x0");
    }

    // ── eth_syncing test ──────────────────────────────────────────────

    #[tokio::test]
    async fn eth_syncing_returns_false() {
        let handler = setup();
        let result = EthApiServer::syncing(&handler).await.unwrap();
        assert_eq!(result, serde_json::Value::Bool(false));
    }

    // ── shell_getValidatorStatus tests ────────────────────────────────

    #[tokio::test]
    async fn get_validator_status_not_validator() {
        let handler = setup();
        let addr = Address::from_public_key(b"some-random-key");
        let result = ShellApiServer::get_validator_status(&handler, addr)
            .await
            .unwrap();
        assert_eq!(result["isValidator"], false);
        assert!(result["address"].is_string());
    }

    #[tokio::test]
    async fn get_validator_status_is_validator() {
        let handler = setup();
        let addr = Address::from_public_key(b"validator-key-1");
        {
            let mut ws = handler.world_state.write();
            ws.set_validators(&[addr]).unwrap();
        }
        let result = ShellApiServer::get_validator_status(&handler, addr)
            .await
            .unwrap();
        assert_eq!(result["isValidator"], true);
    }

    // ── shell_getGovernanceInfo tests ─────────────────────────────────

    #[tokio::test]
    async fn get_governance_info_empty() {
        let handler = setup();
        let result = ShellApiServer::get_governance_info(&handler).await.unwrap();
        assert_eq!(result["validatorCount"], 0);
        assert_eq!(result["validators"], serde_json::json!([]));
        assert_eq!(result["proposalGasLimit"], 100_000);
        assert!(result["systemContractAddress"].is_string());
    }

    #[tokio::test]
    async fn get_governance_info_with_validators() {
        let handler = setup();
        let v1 = Address::from_public_key(b"validator-key-1");
        let v2 = Address::from_public_key(b"validator-key-2");
        {
            let mut ws = handler.world_state.write();
            ws.set_validators(&[v1, v2]).unwrap();
        }
        let result = ShellApiServer::get_governance_info(&handler).await.unwrap();
        assert_eq!(result["validatorCount"], 2);
        assert_eq!(result["validators"].as_array().unwrap().len(), 2);
    }

    // ── shell_estimateGovernanceGas tests ─────────────────────────────

    #[tokio::test]
    async fn estimate_governance_gas_add_validator() {
        let handler = setup();
        let result = ShellApiServer::estimate_governance_gas(&handler, "addValidator".into())
            .await
            .unwrap();
        // 21000 + 5000 = 26000 = 0x6590
        assert_eq!(result, "0x6590");
    }

    #[tokio::test]
    async fn estimate_governance_gas_remove_validator() {
        let handler = setup();
        let result = ShellApiServer::estimate_governance_gas(&handler, "removeValidator".into())
            .await
            .unwrap();
        assert_eq!(result, "0x6590");
    }

    #[tokio::test]
    async fn estimate_governance_gas_view_ops() {
        let handler = setup();
        let result = ShellApiServer::estimate_governance_gas(&handler, "getValidators".into())
            .await
            .unwrap();
        // 21000 = 0x5208
        assert_eq!(result, "0x5208");

        let result = ShellApiServer::estimate_governance_gas(&handler, "isValidator".into())
            .await
            .unwrap();
        assert_eq!(result, "0x5208");
    }

    #[tokio::test]
    async fn estimate_governance_gas_unknown_op() {
        let handler = setup();
        let result = ShellApiServer::estimate_governance_gas(&handler, "badOp".into()).await;
        assert!(result.is_err());
    }

    // ── shell_encodeAddValidator / encodeRemoveValidator tests ────────

    #[tokio::test]
    async fn encode_add_validator_returns_correct_hex() {
        let handler = setup();
        let target = Address::from([0xAB; 20]);
        let hex_addr = format!("0x{}", hex::encode(target.as_bytes()));

        let result = ShellApiServer::encode_add_validator(&handler, hex_addr)
            .await
            .unwrap();

        let expected = shell_evm::encode_add_validator_calldata(&target);
        assert_eq!(result, format!("0x{}", hex::encode(expected)));
        // Must start with the selector
        assert!(result.starts_with("0x"));
        // 4-byte selector + 32-byte param = 36 bytes = 72 hex chars + "0x"
        assert_eq!(result.len(), 74);
    }

    #[tokio::test]
    async fn encode_remove_validator_returns_correct_hex() {
        let handler = setup();
        let target = Address::from([0xCD; 20]);
        let hex_addr = format!("0x{}", hex::encode(target.as_bytes()));

        let result = ShellApiServer::encode_remove_validator(&handler, hex_addr)
            .await
            .unwrap();

        let expected = shell_evm::encode_remove_validator_calldata(&target);
        assert_eq!(result, format!("0x{}", hex::encode(expected)));
        assert_eq!(result.len(), 74);
    }

    #[tokio::test]
    async fn get_governance_info_has_system_contract_address() {
        let handler = setup();
        let result = ShellApiServer::get_governance_info(&handler).await.unwrap();
        let addr_str = result["systemContractAddress"].as_str().unwrap();
        let expected = format!("{}", shell_evm::registry_address());
        assert_eq!(addr_str, expected);
    }

    #[tokio::test]
    async fn get_validator_status_reflects_changes() {
        let handler = setup();
        let addr = Address::from_public_key(b"dynamic-val");

        // Initially not a validator
        let result = ShellApiServer::get_validator_status(&handler, addr)
            .await
            .unwrap();
        assert_eq!(result["isValidator"], false);

        // Set as validator
        {
            let mut ws = handler.world_state.write();
            ws.set_validators(&[addr]).unwrap();
        }

        let result = ShellApiServer::get_validator_status(&handler, addr)
            .await
            .unwrap();
        assert_eq!(result["isValidator"], true);
    }

    #[tokio::test]
    async fn encode_add_validator_rejects_bad_address() {
        let handler = setup();
        let result = ShellApiServer::encode_add_validator(&handler, "not-hex".into()).await;
        assert!(result.is_err());
    }

    // ── shell_getNodeInfo ──────────────────────────────────────────

    #[tokio::test]
    async fn get_node_info_returns_all_fields() {
        let handler = setup();
        let result = ShellApiServer::get_node_info(&handler).await.unwrap();

        assert_eq!(result["version"], "ShellChain/v0.1.0/rust");
        assert_eq!(result["chainId"], 42);
        assert_eq!(result["blockHeight"], 0);
        assert_eq!(result["peerCount"], 0);
        assert!(result["txPoolSize"].is_u64());
        assert_eq!(result["isMining"], false);
        assert!(result["uptime"].is_u64());
        assert!(result["baseFee"].is_string());
    }

    #[tokio::test]
    async fn get_node_info_reflects_block_height() {
        let handler = setup();
        let block = make_genesis_block();
        let hash = block.hash();
        handler.chain_store.put_block(&block).unwrap();
        handler.chain_store.set_canonical(0, &hash).unwrap();
        handler.chain_store.set_head(&hash).unwrap();

        let result = ShellApiServer::get_node_info(&handler).await.unwrap();
        assert_eq!(result["blockHeight"], 0);
        assert_eq!(result["chainId"], 42);
    }

    #[tokio::test]
    async fn get_node_info_mining_true_with_proposer() {
        let handler = setup();
        let signer = DilithiumSigner::generate();
        let addr = Address::from_public_key(&signer.public_key());
        let handler = handler.with_proposer(Arc::new(signer), addr);

        let result = ShellApiServer::get_node_info(&handler).await.unwrap();
        assert_eq!(result["isMining"], true);
    }

    // ── shell_getNetworkStats ──────────────────────────────────────

    #[tokio::test]
    async fn get_network_stats_returns_all_fields() {
        let handler = setup();
        let result = ShellApiServer::get_network_stats(&handler).await.unwrap();

        assert_eq!(result["peerCount"], 0);
        assert_eq!(result["protocolVersion"], "shell/1.0.0");
        assert_eq!(result["listeningAddress"], "/ip4/0.0.0.0/tcp/30303");
        let protocols = result["protocols"].as_array().unwrap();
        assert_eq!(protocols.len(), 3);
        assert!(protocols.contains(&serde_json::json!("gossipsub")));
        assert!(protocols.contains(&serde_json::json!("kademlia")));
        assert!(protocols.contains(&serde_json::json!("mdns")));
    }

    // ── shell_getChainStats ────────────────────────────────────────

    #[tokio::test]
    async fn get_chain_stats_empty_chain() {
        let handler = setup();
        let result = ShellApiServer::get_chain_stats(&handler).await.unwrap();

        assert_eq!(result["blockHeight"], 0);
        assert_eq!(result["totalTransactions"], 0);
        assert_eq!(result["avgBlockTime"], 0.0);
        assert!(result["gasUsedTotal"].is_string());
        assert!(result["latestBaseFee"].is_string());
    }

    #[tokio::test]
    async fn get_chain_stats_with_blocks() {
        let handler = setup();

        let genesis = make_genesis_block();
        let genesis_hash = genesis.hash();
        handler.chain_store.put_block(&genesis).unwrap();
        handler.chain_store.set_canonical(0, &genesis_hash).unwrap();
        handler.chain_store.set_head(&genesis_hash).unwrap();

        let block1 = Block {
            header: BlockHeader {
                parent_hash: genesis_hash,
                state_root: ShellHash::default(),
                transactions_root: ShellHash::default(),
                receipts_root: ShellHash::default(),
                logs_bloom: Bytes::default(),
                number: 1,
                gas_limit: 30_000_000,
                gas_used: 21_000,
                timestamp: 1_700_000_003,
                extra_data: Bytes::default(),
                proposer: Address::from_public_key(b"proposer-key-data"),
                sig_aggregate_proof: None,
                base_fee_per_gas: 1_000_000_000,
            },
            transactions: vec![],
            proposer_seal: None,
        };
        let hash1 = block1.hash();
        handler.chain_store.put_block(&block1).unwrap();
        handler.chain_store.set_canonical(1, &hash1).unwrap();
        handler.chain_store.set_head(&hash1).unwrap();

        let result = ShellApiServer::get_chain_stats(&handler).await.unwrap();
        assert_eq!(result["blockHeight"], 1);
        assert_eq!(result["totalTransactions"], 0);
        assert_eq!(result["avgBlockTime"], 3.0);
        assert_eq!(result["gasUsedTotal"], "0x5208"); // 21000
        assert!(result["latestBaseFee"].is_string());
    }
}
