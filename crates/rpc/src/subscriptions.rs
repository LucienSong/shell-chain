//! Ethereum PubSub (eth_subscribe / eth_unsubscribe) implementation.
//!
//! Supports two subscription types:
//! - `newHeads` — pushes new block headers when blocks are produced or imported.
//! - `logs` — pushes matching logs (filtered by address / topics).

use jsonrpsee::core::SubscriptionResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::PendingSubscriptionSink;
use jsonrpsee::SubscriptionMessage;
use shell_core::{BlockHeader, TransactionReceipt};
use shell_primitives::{Address, ShellHash};
use shell_storage::KvStore;
use tokio::sync::broadcast;

use crate::handler::RpcHandler;
use crate::types::{hex_bytes, hex_u64};

/// Parse a hex address string like "0xaaaa..." into an `Address`.
fn parse_address_hex(s: &str) -> Option<Address> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).ok()?;
    Address::try_from_slice(&bytes).ok()
}

/// Parse a hex hash string like "0x0000..." into a `ShellHash`.
fn parse_hash_hex(s: &str) -> Option<ShellHash> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(s).ok()?;
    ShellHash::try_from_slice(&bytes).ok()
}

// ---------------------------------------------------------------------------
// Block event broadcast type
// ---------------------------------------------------------------------------

/// Events broadcast from the node's block production / import pipeline.
#[derive(Debug, Clone)]
pub enum BlockEvent {
    /// A new block was produced or imported.
    NewBlock {
        header: BlockHeader,
        receipts: Vec<TransactionReceipt>,
    },
}

// ---------------------------------------------------------------------------
// Log filter (for `logs` subscriptions)
// ---------------------------------------------------------------------------

/// Simple log filter matching the subset of `eth_getLogs` filter params.
#[derive(Debug, Clone, Default)]
struct LogFilter {
    /// If non-empty, only logs from these addresses are included.
    addresses: Vec<Address>,
    /// Per-position topic filter. Each position may have a set of acceptable
    /// values (OR within position, AND across positions).
    topics: Vec<Vec<ShellHash>>,
}

impl LogFilter {
    fn from_value(v: &serde_json::Value) -> Self {
        let mut filter = LogFilter::default();

        if let Some(obj) = v.as_object() {
            // Parse address(es).
            if let Some(addr_val) = obj.get("address") {
                match addr_val {
                    serde_json::Value::String(s) => {
                        if let Some(addr) = parse_address_hex(s) {
                            filter.addresses.push(addr);
                        }
                    }
                    serde_json::Value::Array(arr) => {
                        for item in arr {
                            if let Some(s) = item.as_str() {
                                if let Some(addr) = parse_address_hex(s) {
                                    filter.addresses.push(addr);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Parse topics — array of (hash | hash[] | null).
            if let Some(serde_json::Value::Array(topics_arr)) = obj.get("topics") {
                for entry in topics_arr {
                    match entry {
                        serde_json::Value::Null => {
                            filter.topics.push(vec![]);
                        }
                        serde_json::Value::String(s) => {
                            if let Some(hash) = parse_hash_hex(s) {
                                filter.topics.push(vec![hash]);
                            } else {
                                filter.topics.push(vec![]);
                            }
                        }
                        serde_json::Value::Array(arr) => {
                            let hashes: Vec<ShellHash> = arr
                                .iter()
                                .filter_map(|v| parse_hash_hex(v.as_str()?))
                                .collect();
                            filter.topics.push(hashes);
                        }
                        _ => {
                            filter.topics.push(vec![]);
                        }
                    }
                }
            }
        }

        filter
    }

    /// Returns `true` if the given log matches this filter.
    fn matches(&self, log: &shell_core::Log) -> bool {
        // Address filter.
        if !self.addresses.is_empty() && !self.addresses.contains(&log.address) {
            return false;
        }

        // Topic filters.
        for (i, acceptable) in self.topics.iter().enumerate() {
            if acceptable.is_empty() {
                // null / wildcard — matches anything at this position.
                continue;
            }
            match log.topics.get(i) {
                Some(log_topic) => {
                    if !acceptable.contains(log_topic) {
                        return false;
                    }
                }
                None => return false,
            }
        }

        true
    }
}

// ---------------------------------------------------------------------------
// RPC trait definition
// ---------------------------------------------------------------------------

/// Ethereum PubSub RPC trait.
#[rpc(server, namespace = "eth")]
pub trait EthPubSub {
    /// Subscribe to live events (`newHeads` or `logs`).
    #[subscription(name = "subscribe" => "subscription", unsubscribe = "unsubscribe", item = serde_json::Value)]
    async fn subscribe(
        &self,
        sub_type: String,
        params: Option<serde_json::Value>,
    ) -> SubscriptionResult;
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

#[jsonrpsee::core::async_trait]
impl<S: KvStore + 'static> EthPubSubServer for RpcHandler<S> {
    async fn subscribe(
        &self,
        pending: PendingSubscriptionSink,
        sub_type: String,
        params: Option<serde_json::Value>,
    ) -> SubscriptionResult {
        let rx = self.block_event_sender().subscribe();

        match sub_type.as_str() {
            "newHeads" => {
                let sink = pending.accept().await?;
                tokio::spawn(forward_new_heads(rx, sink));
            }
            "logs" => {
                let filter = params
                    .as_ref()
                    .map(LogFilter::from_value)
                    .unwrap_or_default();
                let sink = pending.accept().await?;
                tokio::spawn(forward_logs(rx, sink, filter));
            }
            _ => {
                pending
                    .reject(jsonrpsee::types::ErrorObject::owned(
                        -32602,
                        format!("unsupported subscription type: {sub_type}"),
                        None::<()>,
                    ))
                    .await;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Background forwarding tasks
// ---------------------------------------------------------------------------

/// Serialize a `BlockHeader` into the JSON shape expected by `eth_subscription`
/// `newHeads` notifications.
fn header_to_json(header: &BlockHeader) -> serde_json::Value {
    serde_json::json!({
        "hash": header.hash(),
        "parentHash": header.parent_hash,
        "number": hex_u64(header.number),
        "timestamp": hex_u64(header.timestamp),
        "gasLimit": hex_u64(header.gas_limit),
        "gasUsed": hex_u64(header.gas_used),
        "miner": header.proposer,
        "stateRoot": header.state_root,
        "transactionsRoot": header.transactions_root,
        "receiptsRoot": header.receipts_root,
        "logsBloom": hex_bytes(header.logs_bloom.as_ref()),
        "extraData": hex_bytes(header.extra_data.as_ref()),
    })
}

/// Serialize a log entry with contextual block/tx metadata.
fn log_to_json(
    log: &shell_core::Log,
    block_header: &BlockHeader,
    tx_hash: &ShellHash,
    tx_index: u32,
    log_index: usize,
) -> serde_json::Value {
    serde_json::json!({
        "address": log.address,
        "topics": log.topics,
        "data": hex_bytes(log.data.as_ref()),
        "blockNumber": hex_u64(block_header.number),
        "blockHash": block_header.hash(),
        "transactionHash": tx_hash,
        "transactionIndex": hex_u64(tx_index as u64),
        "logIndex": hex_u64(log_index as u64),
        "removed": false,
    })
}

async fn forward_new_heads(
    mut rx: broadcast::Receiver<BlockEvent>,
    sink: jsonrpsee::SubscriptionSink,
) {
    let mut consecutive_lags: u32 = 0;
    loop {
        match rx.recv().await {
            Ok(BlockEvent::NewBlock { header, .. }) => {
                consecutive_lags = 0;
                let value = header_to_json(&header);
                let msg = SubscriptionMessage::from_json(&value)
                    .expect("header serialization cannot fail");
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                consecutive_lags += 1;
                tracing::warn!(skipped = n, consecutive_lags, "newHeads subscriber lagged");
                // F-042: auto-disconnect after 3 consecutive lags.
                if consecutive_lags >= 3 {
                    tracing::error!("newHeads subscriber too slow — disconnecting");
                    break;
                }
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn forward_logs(
    mut rx: broadcast::Receiver<BlockEvent>,
    sink: jsonrpsee::SubscriptionSink,
    filter: LogFilter,
) {
    let mut consecutive_lags: u32 = 0;
    loop {
        match rx.recv().await {
            Ok(BlockEvent::NewBlock { header, receipts }) => {
                consecutive_lags = 0;
                let mut global_log_index: usize = 0;
                for receipt in &receipts {
                    for log in &receipt.logs {
                        if filter.matches(log) {
                            let value = log_to_json(
                                log,
                                &header,
                                &receipt.tx_hash,
                                receipt.tx_index,
                                global_log_index,
                            );
                            let msg = SubscriptionMessage::from_json(&value)
                                .expect("log serialization cannot fail");
                            if sink.send(msg).await.is_err() {
                                return;
                            }
                        }
                        global_log_index += 1;
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                consecutive_lags += 1;
                tracing::warn!(skipped = n, consecutive_lags, "logs subscriber lagged");
                // F-042: auto-disconnect after 3 consecutive lags.
                if consecutive_lags >= 3 {
                    tracing::error!("logs subscriber too slow — disconnecting");
                    return;
                }
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use shell_core::Log;
    use shell_primitives::Bytes;

    fn sample_header(number: u64) -> BlockHeader {
        BlockHeader {
            parent_hash: ShellHash::ZERO,
            state_root: ShellHash::ZERO,
            transactions_root: ShellHash::ZERO,
            receipts_root: ShellHash::ZERO,
            logs_bloom: Bytes::new(),
            number,
            gas_limit: 30_000_000,
            gas_used: 21_000,
            timestamp: 1_700_000_000,
            extra_data: Bytes::new(),
            proposer: Address::ZERO,
            sig_aggregate_proof: None,
            base_fee_per_gas: 0,
        }
    }

    fn sample_receipt(addr: Address, topic: ShellHash) -> TransactionReceipt {
        TransactionReceipt {
            tx_hash: ShellHash::ZERO,
            block_number: 1,
            tx_index: 0,
            status: 1,
            gas_used: 21_000,
            cumulative_gas_used: 21_000,
            contract_address: None,
            logs_bloom: Bytes::new(),
            logs: vec![Log {
                address: addr,
                topics: vec![topic],
                data: Bytes::new(),
            }],
        }
    }

    #[test]
    fn log_filter_empty_matches_everything() {
        let filter = LogFilter::default();
        let log = Log {
            address: Address::from([0xAA; 20]),
            topics: vec![ShellHash::ZERO],
            data: Bytes::new(),
        };
        assert!(filter.matches(&log));
    }

    #[test]
    fn log_filter_address_match() {
        let addr = Address::from([0xAA; 20]);
        let filter = LogFilter {
            addresses: vec![addr],
            topics: vec![],
        };
        let log = Log {
            address: addr,
            topics: vec![],
            data: Bytes::new(),
        };
        assert!(filter.matches(&log));
    }

    #[test]
    fn log_filter_address_mismatch() {
        let filter = LogFilter {
            addresses: vec![Address::from([0xAA; 20])],
            topics: vec![],
        };
        let log = Log {
            address: Address::from([0xBB; 20]),
            topics: vec![],
            data: Bytes::new(),
        };
        assert!(!filter.matches(&log));
    }

    #[test]
    fn log_filter_topic_match() {
        let topic = shell_primitives::keccak256(b"Transfer(address,address,uint256)");
        let filter = LogFilter {
            addresses: vec![],
            topics: vec![vec![topic]],
        };
        let log = Log {
            address: Address::ZERO,
            topics: vec![topic],
            data: Bytes::new(),
        };
        assert!(filter.matches(&log));
    }

    #[test]
    fn log_filter_topic_mismatch() {
        let topic_a = shell_primitives::keccak256(b"Transfer");
        let topic_b = shell_primitives::keccak256(b"Approval");
        let filter = LogFilter {
            addresses: vec![],
            topics: vec![vec![topic_a]],
        };
        let log = Log {
            address: Address::ZERO,
            topics: vec![topic_b],
            data: Bytes::new(),
        };
        assert!(!filter.matches(&log));
    }

    #[test]
    fn log_filter_wildcard_position() {
        let topic_b = shell_primitives::keccak256(b"value");
        let filter = LogFilter {
            addresses: vec![],
            // First position is wildcard, second must match.
            topics: vec![vec![], vec![topic_b]],
        };
        let log = Log {
            address: Address::ZERO,
            topics: vec![shell_primitives::keccak256(b"anything"), topic_b],
            data: Bytes::new(),
        };
        assert!(filter.matches(&log));
    }

    #[test]
    fn log_filter_from_json() {
        let json = serde_json::json!({
            "address": "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "topics": [null, "0x0000000000000000000000000000000000000000000000000000000000000000"]
        });
        let filter = LogFilter::from_value(&json);
        assert_eq!(filter.addresses.len(), 1);
        assert_eq!(filter.topics.len(), 2);
        assert!(filter.topics[0].is_empty()); // null → wildcard
        assert_eq!(filter.topics[1].len(), 1);
    }

    #[test]
    fn header_to_json_roundtrip() {
        let header = sample_header(42);
        let json = header_to_json(&header);
        assert_eq!(json["number"], "0x2a");
        assert_eq!(json["gasUsed"], "0x5208");
    }

    #[tokio::test]
    async fn broadcast_channel_delivers_events() {
        let (tx, mut rx) = broadcast::channel::<BlockEvent>(16);
        let header = sample_header(1);

        tx.send(BlockEvent::NewBlock {
            header: header.clone(),
            receipts: vec![],
        })
        .unwrap();

        match rx.recv().await.unwrap() {
            BlockEvent::NewBlock {
                header: h,
                receipts: r,
            } => {
                assert_eq!(h.number, 1);
                assert!(r.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn broadcast_multiple_subscribers() {
        let (tx, _) = broadcast::channel::<BlockEvent>(16);
        let mut rx1 = tx.subscribe();
        let mut rx2 = tx.subscribe();

        tx.send(BlockEvent::NewBlock {
            header: sample_header(5),
            receipts: vec![],
        })
        .unwrap();

        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        match (e1, e2) {
            (
                BlockEvent::NewBlock { header: h1, .. },
                BlockEvent::NewBlock { header: h2, .. },
            ) => {
                assert_eq!(h1.number, 5);
                assert_eq!(h2.number, 5);
            }
        }
    }

    #[tokio::test]
    async fn logs_filter_selects_matching_receipts() {
        let addr = Address::from([0xCC; 20]);
        let topic = shell_primitives::keccak256(b"Transfer(address,address,uint256)");
        let filter = LogFilter {
            addresses: vec![addr],
            topics: vec![vec![topic]],
        };

        let matching_receipt = sample_receipt(addr, topic);
        let non_matching_receipt =
            sample_receipt(Address::from([0xDD; 20]), shell_primitives::keccak256(b"Other"));

        // The matching receipt's log should pass.
        assert!(filter.matches(&matching_receipt.logs[0]));
        // The non-matching receipt's log should NOT pass.
        assert!(!filter.matches(&non_matching_receipt.logs[0]));
    }
}
