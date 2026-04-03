use serde::{Deserialize, Serialize};
use shell_primitives::{Address, Bytes, ShellHash};
use shell_crypto::PQSignature;
use alloy_rlp::Encodable;

use crate::SignedTransaction;

/// Block header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockHeader {
    pub parent_hash: ShellHash,
    pub state_root: ShellHash,
    pub transactions_root: ShellHash,
    pub receipts_root: ShellHash,
    /// Bloom filter over all logs in this block (2048-bit / 256 bytes).
    /// Populated by EVM executor; empty during construction.
    pub logs_bloom: Bytes,
    pub number: u64,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub timestamp: u64,
    pub extra_data: Bytes,
    pub proposer: Address,
    /// Aggregated proof for batched signature verification (future use).
    pub sig_aggregate_proof: Option<Bytes>,
    /// EIP-1559 base fee per gas. 0 for the genesis block.
    pub base_fee_per_gas: u64,
}

impl Encodable for BlockHeader {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let header = alloy_rlp::Header {
            list: true,
            payload_length: self.fields_len(),
        };
        header.encode(out);
        self.parent_hash.encode(out);
        self.state_root.encode(out);
        self.transactions_root.encode(out);
        self.receipts_root.encode(out);
        self.logs_bloom.encode(out);
        self.number.encode(out);
        self.gas_limit.encode(out);
        self.gas_used.encode(out);
        self.timestamp.encode(out);
        self.extra_data.encode(out);
        self.proposer.encode(out);
        match &self.sig_aggregate_proof {
            Some(proof) => proof.encode(out),
            None => {
                let empty: &[u8] = &[];
                empty.encode(out);
            }
        }
        self.base_fee_per_gas.encode(out);
    }

    fn length(&self) -> usize {
        let payload = self.fields_len();
        alloy_rlp::Header { list: true, payload_length: payload }.length() + payload
    }
}

impl BlockHeader {
    fn fields_len(&self) -> usize {
        let proof_len = match &self.sig_aggregate_proof {
            Some(proof) => proof.length(),
            None => 1, // 0x80
        };
        self.parent_hash.length()
            + self.state_root.length()
            + self.transactions_root.length()
            + self.receipts_root.length()
            + self.logs_bloom.length()
            + self.number.length()
            + self.gas_limit.length()
            + self.gas_used.length()
            + self.timestamp.length()
            + self.extra_data.length()
            + self.proposer.length()
            + proof_len
            + self.base_fee_per_gas.length()
    }

    /// Compute the block hash (keccak256 of RLP-encoded header).
    pub fn hash(&self) -> ShellHash {
        let mut buf = Vec::new();
        self.encode(&mut buf);
        shell_primitives::keccak256(&buf)
    }

    pub fn is_genesis(&self) -> bool {
        self.number == 0 && self.parent_hash == ShellHash::ZERO
    }
}

/// A complete block: header + body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<SignedTransaction>,
    /// PoA proposer seal (PQ signature over the header hash).
    /// Stored outside the header to avoid circular hashing.
    pub proposer_seal: Option<PQSignature>,
}

impl Block {
    pub fn hash(&self) -> ShellHash {
        self.header.hash()
    }

    pub fn number(&self) -> u64 {
        self.header.number
    }

    pub fn tx_count(&self) -> usize {
        self.transactions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> BlockHeader {
        BlockHeader {
            parent_hash: ShellHash::ZERO,
            state_root: ShellHash::ZERO,
            transactions_root: ShellHash::ZERO,
            receipts_root: ShellHash::ZERO,
            logs_bloom: Bytes::new(),
            number: 0,
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1700000000,
            extra_data: Bytes::new(),
            proposer: Address::ZERO,
            sig_aggregate_proof: None,
            base_fee_per_gas: 0,
        }
    }

    #[test]
    fn genesis_block() {
        let header = sample_header();
        assert!(header.is_genesis());
    }

    #[test]
    fn non_genesis_block() {
        let mut header = sample_header();
        header.number = 1;
        header.parent_hash = shell_primitives::keccak256(b"parent");
        assert!(!header.is_genesis());
    }

    #[test]
    fn header_hash_deterministic() {
        let header = sample_header();
        assert_eq!(header.hash(), header.hash());
    }

    #[test]
    fn header_hash_changes_with_number() {
        let h1 = sample_header();
        let mut h2 = sample_header();
        h2.number = 1;
        assert_ne!(h1.hash(), h2.hash());
    }

    #[test]
    fn header_rlp_encodes() {
        let header = sample_header();
        let mut buf = Vec::new();
        header.encode(&mut buf);
        assert!(!buf.is_empty());
        // Hash from encoded bytes should be consistent
        let hash = shell_primitives::keccak256(&buf);
        assert_eq!(hash, header.hash());
    }

    #[test]
    fn block_basic() {
        let block = Block {
            header: sample_header(),
            transactions: vec![],
            proposer_seal: None,
        };
        assert_eq!(block.number(), 0);
        assert_eq!(block.tx_count(), 0);
    }

    #[test]
    fn block_serde_roundtrip() {
        let block = Block {
            header: sample_header(),
            transactions: vec![],
            proposer_seal: None,
        };
        let json = serde_json::to_string(&block).unwrap();
        let block2: Block = serde_json::from_str(&json).unwrap();
        assert_eq!(block.header, block2.header);
    }
}
