use serde::{Deserialize, Serialize};
use shell_primitives::{Address, Bytes, ShellHash, U256};
use shell_crypto::{PQSignature, SignatureType};
use alloy_rlp::Encodable;

/// An unsigned transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub chain_id: u64,
    pub nonce: u64,
    pub to: Address,
    pub value: U256,
    pub data: Bytes,
    pub gas_limit: u64,
    pub max_fee_per_gas: u64,
    pub max_priority_fee_per_gas: u64,
}

impl Encodable for Transaction {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        // Encode as an RLP list of fields
        let header = alloy_rlp::Header {
            list: true,
            payload_length: self.fields_len(),
        };
        header.encode(out);
        self.chain_id.encode(out);
        self.nonce.encode(out);
        self.to.encode(out);
        self.value.encode(out);
        self.data.encode(out);
        self.gas_limit.encode(out);
        self.max_fee_per_gas.encode(out);
        self.max_priority_fee_per_gas.encode(out);
    }

    fn length(&self) -> usize {
        let payload = self.fields_len();
        alloy_rlp::Header { list: true, payload_length: payload }.length() + payload
    }
}

impl Transaction {
    fn fields_len(&self) -> usize {
        self.chain_id.length()
            + self.nonce.length()
            + self.to.length()
            + self.value.length()
            + self.data.length()
            + self.gas_limit.length()
            + self.max_fee_per_gas.length()
            + self.max_priority_fee_per_gas.length()
    }

    /// Compute the signing hash (keccak256 of the RLP-encoded transaction).
    pub fn hash(&self) -> ShellHash {
        let mut buf = Vec::new();
        self.encode(&mut buf);
        shell_primitives::keccak256(&buf)
    }
}

/// A transaction with an attached PQ signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedTransaction {
    pub tx: Transaction,
    pub signature: PQSignature,
    /// Cached hash — computed from the unsigned tx, NOT including the signature.
    #[serde(skip)]
    tx_hash: Option<ShellHash>,
}

impl SignedTransaction {
    pub fn new(tx: Transaction, signature: PQSignature) -> Self {
        let tx_hash = Some(tx.hash());
        Self {
            tx,
            signature,
            tx_hash,
        }
    }

    /// Transaction hash (excludes signature data).
    pub fn hash(&self) -> ShellHash {
        self.tx_hash.unwrap_or_else(|| self.tx.hash())
    }

    pub fn sig_type(&self) -> SignatureType {
        self.signature.sig_type
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tx() -> Transaction {
        Transaction {
            chain_id: 1337,
            nonce: 0,
            to: Address::from([0x01; 20]),
            value: U256::from(1000),
            data: Bytes::new(),
            gas_limit: 21000,
            max_fee_per_gas: 20,
            max_priority_fee_per_gas: 1,
        }
    }

    #[test]
    fn tx_hash_deterministic() {
        let tx = sample_tx();
        assert_eq!(tx.hash(), tx.hash());
    }

    #[test]
    fn tx_hash_changes_with_nonce() {
        let tx1 = sample_tx();
        let mut tx2 = sample_tx();
        tx2.nonce = 1;
        assert_ne!(tx1.hash(), tx2.hash());
    }

    #[test]
    fn signed_tx_hash_excludes_signature() {
        let tx = sample_tx();
        let hash_before = tx.hash();

        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xAA; 100]);
        let signed = SignedTransaction::new(tx, sig);

        // The transaction hash should be the same regardless of signature
        assert_eq!(signed.hash(), hash_before);
    }

    #[test]
    fn tx_serde_roundtrip() {
        let tx = sample_tx();
        let json = serde_json::to_string(&tx).unwrap();
        let tx2: Transaction = serde_json::from_str(&json).unwrap();
        assert_eq!(tx, tx2);
    }
}
