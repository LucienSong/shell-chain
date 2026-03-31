use serde::{Deserialize, Serialize};
use shell_primitives::{Address, Bytes, ShellHash};

use crate::log::Log;

/// Result of executing a transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionReceipt {
    /// Transaction hash.
    pub tx_hash: ShellHash,
    /// Block number where this transaction was included.
    pub block_number: u64,
    /// Index of the transaction within the block.
    pub tx_index: u32,
    /// Whether the transaction succeeded (1) or reverted (0).
    pub status: u8,
    /// Gas consumed by this transaction.
    pub gas_used: u64,
    /// Cumulative gas used in the block up to and including this tx.
    pub cumulative_gas_used: u64,
    /// Contract address created, if any.
    pub contract_address: Option<Address>,
    /// Bloom filter for fast log filtering (2048-bit / 256 bytes).
    /// Populated by EVM executor; empty until execution.
    pub logs_bloom: Bytes,
    /// Event logs emitted during execution.
    pub logs: Vec<Log>,
}

impl TransactionReceipt {
    pub fn succeeded(&self) -> bool {
        self.status == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::keccak256;

    #[test]
    fn receipt_success_check() {
        let receipt = TransactionReceipt {
            tx_hash: keccak256(b"tx1"),
            block_number: 1,
            tx_index: 0,
            status: 1,
            gas_used: 21000,
            cumulative_gas_used: 21000,
            contract_address: None,
            logs_bloom: Bytes::new(),
            logs: vec![],
        };
        assert!(receipt.succeeded());
    }

    #[test]
    fn receipt_serde_roundtrip() {
        let receipt = TransactionReceipt {
            tx_hash: keccak256(b"tx"),
            block_number: 42,
            tx_index: 3,
            status: 0,
            gas_used: 50000,
            cumulative_gas_used: 100000,
            contract_address: Some(Address::from([0xAB; 20])),
            logs_bloom: Bytes::new(),
            logs: vec![Log {
                address: Address::from([0xCD; 20]),
                topics: vec![keccak256(b"Transfer(address,address,uint256)")],
                data: shell_primitives::Bytes::from(vec![1, 2, 3]),
            }],
        };
        let json = serde_json::to_string(&receipt).unwrap();
        let receipt2: TransactionReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(receipt, receipt2);
    }
}
