use serde::{Deserialize, Serialize};
use shell_primitives::{Address, Bytes, ShellHash, U256};
use shell_crypto::{PQSignature, SignatureType};
use alloy_rlp::Encodable;
use std::sync::OnceLock;

/// An unsigned transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub chain_id: u64,
    pub nonce: u64,
    /// Recipient address. `None` means contract creation.
    pub to: Option<Address>,
    pub value: U256,
    pub data: Bytes,
    pub gas_limit: u64,
    pub max_fee_per_gas: u64,
    pub max_priority_fee_per_gas: u64,
}

impl Encodable for Transaction {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let header = alloy_rlp::Header {
            list: true,
            payload_length: self.fields_len(),
        };
        header.encode(out);
        self.chain_id.encode(out);
        self.nonce.encode(out);
        // Ethereum convention: None → empty bytes, Some → 20-byte address
        match &self.to {
            Some(addr) => addr.encode(out),
            None => {
                let empty: &[u8] = &[];
                empty.encode(out);
            }
        }
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
        let to_len = match &self.to {
            Some(addr) => addr.length(),
            None => 1, // RLP encoding of empty bytes
        };
        self.chain_id.length()
            + self.nonce.length()
            + to_len
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

    pub fn is_contract_creation(&self) -> bool {
        self.to.is_none()
    }
}

/// A transaction with an attached PQ signature.
///
/// PQ signatures (unlike ECDSA) do not allow public key recovery from the
/// signature alone. The sender must explicitly declare their address so
/// nodes can look up the account and verify the signature.
///
/// The optional `sender_pubkey` field implements the **Hybrid registration**
/// model: the first transaction from a new address carries the full PQ
/// public key (~1952 bytes for Dilithium3). Subsequent transactions omit it,
/// and the pubkey is read from the on-chain registry.
#[derive(Debug, Serialize, Deserialize)]
pub struct SignedTransaction {
    /// The sender's address (derived from their PQ public key).
    /// Required because PQ signatures are not recoverable.
    pub from: Address,
    pub tx: Transaction,
    pub signature: PQSignature,
    /// Optional full PQ public key for first-time registration.
    /// If present, the node registers it on-chain after verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_pubkey: Option<Vec<u8>>,
    /// Lazily cached hash — computed from the unsigned tx on first access.
    #[serde(skip)]
    tx_hash: OnceLock<ShellHash>,
}

impl Clone for SignedTransaction {
    fn clone(&self) -> Self {
        let lock = OnceLock::new();
        if let Some(&h) = self.tx_hash.get() {
            let _ = lock.set(h);
        }
        Self {
            from: self.from,
            tx: self.tx.clone(),
            signature: self.signature.clone(),
            sender_pubkey: self.sender_pubkey.clone(),
            tx_hash: lock,
        }
    }
}

impl PartialEq for SignedTransaction {
    fn eq(&self, other: &Self) -> bool {
        self.from == other.from
            && self.tx == other.tx
            && self.signature == other.signature
            && self.sender_pubkey == other.sender_pubkey
    }
}

impl Eq for SignedTransaction {}

impl SignedTransaction {
    pub fn new(from: Address, tx: Transaction, signature: PQSignature) -> Self {
        Self {
            from,
            tx,
            signature,
            sender_pubkey: None,
            tx_hash: OnceLock::new(),
        }
    }

    /// Create a signed transaction with an attached public key for
    /// first-time registration on the PQ pubkey registry.
    pub fn with_pubkey(
        from: Address,
        tx: Transaction,
        signature: PQSignature,
        pubkey: Vec<u8>,
    ) -> Self {
        Self {
            from,
            tx,
            signature,
            sender_pubkey: Some(pubkey),
            tx_hash: OnceLock::new(),
        }
    }

    /// Transaction hash (excludes signature data and sender).
    /// Cached after first computation via `OnceLock`.
    pub fn hash(&self) -> ShellHash {
        *self.tx_hash.get_or_init(|| self.tx.hash())
    }

    pub fn sig_type(&self) -> SignatureType {
        self.signature.sig_type
    }

    pub fn sender(&self) -> Address {
        self.from
    }
}

impl Encodable for SignedTransaction {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let header = alloy_rlp::Header {
            list: true,
            payload_length: self.fields_len(),
        };
        header.encode(out);
        self.from.encode(out);
        self.tx.encode(out);
        self.signature.encode(out);
        // Encode sender_pubkey: Some(bytes) → bytes, None → empty bytes
        match &self.sender_pubkey {
            Some(pk) => pk.as_slice().encode(out),
            None => {
                let empty: &[u8] = &[];
                empty.encode(out);
            }
        }
    }

    fn length(&self) -> usize {
        let payload = self.fields_len();
        alloy_rlp::Header { list: true, payload_length: payload }.length() + payload
    }
}

impl SignedTransaction {
    fn fields_len(&self) -> usize {
        let pk_len = match &self.sender_pubkey {
            Some(pk) => pk.as_slice().length(),
            None => 1, // RLP encoding of empty bytes
        };
        self.from.length() + self.tx.length() + self.signature.length() + pk_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tx() -> Transaction {
        Transaction {
            chain_id: 1337,
            nonce: 0,
            to: Some(Address::from([0x01; 20])),
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
        let from = Address::from([0x42; 20]);

        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xAA; 100]);
        let signed = SignedTransaction::new(from, tx, sig);

        assert_eq!(signed.hash(), hash_before);
        assert_eq!(signed.sender(), from);
    }

    #[test]
    fn contract_creation_tx() {
        let tx = Transaction {
            chain_id: 1337,
            nonce: 0,
            to: None,
            value: U256::ZERO,
            data: Bytes::from(vec![0x60, 0x80]),
            gas_limit: 1_000_000,
            max_fee_per_gas: 20,
            max_priority_fee_per_gas: 1,
        };
        assert!(tx.is_contract_creation());

        // Hash differs from a regular transfer
        let transfer = sample_tx();
        assert_ne!(tx.hash(), transfer.hash());
    }

    #[test]
    fn tx_serde_roundtrip() {
        let tx = sample_tx();
        let json = serde_json::to_string(&tx).unwrap();
        let tx2: Transaction = serde_json::from_str(&json).unwrap();
        assert_eq!(tx, tx2);
    }

    #[test]
    fn contract_creation_rlp_to_none_encoding() {
        // F-015: Verify to: None produces shorter RLP (0x80) vs 21-byte address
        let tx_with_to = sample_tx();
        let mut buf_with = Vec::new();
        tx_with_to.encode(&mut buf_with);

        let tx_none = Transaction {
            chain_id: 1337,
            nonce: 0,
            to: None,
            value: U256::from(1000),
            data: Bytes::new(),
            gas_limit: 21000,
            max_fee_per_gas: 20,
            max_priority_fee_per_gas: 1,
        };
        let mut buf_none = Vec::new();
        tx_none.encode(&mut buf_none);

        // to: None encodes as 0x80 (1 byte) vs to: Some → 21 bytes
        assert!(buf_none.len() < buf_with.len());
        // The hashes must differ
        assert_ne!(
            shell_primitives::keccak256(&buf_with),
            shell_primitives::keccak256(&buf_none),
        );
    }

    #[test]
    fn signed_tx_hash_cached_via_oncelock() {
        let tx = sample_tx();
        let from = Address::from([0x42; 20]);
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xAA; 100]);
        let signed = SignedTransaction::new(from, tx, sig);

        // First call computes and caches
        let h1 = signed.hash();
        // Second call returns cached value
        let h2 = signed.hash();
        assert_eq!(h1, h2);

        // Deserialized version also works (OnceLock starts empty)
        let json = serde_json::to_string(&signed).unwrap();
        let signed2: SignedTransaction = serde_json::from_str(&json).unwrap();
        assert_eq!(signed2.hash(), h1);
        assert_eq!(signed, signed2);
    }

    #[test]
    fn signed_tx_rlp_roundtrip() {
        let tx = sample_tx();
        let from = Address::from([0x42; 20]);
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xBB; 50]);
        let signed = SignedTransaction::new(from, tx, sig);

        let mut buf = Vec::new();
        signed.encode(&mut buf);
        assert!(!buf.is_empty());
    }
}
