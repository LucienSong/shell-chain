use serde::{Deserialize, Serialize};
use shell_primitives::{ShellHash, U256};
use alloy_rlp::Encodable;

/// Account with native Account Abstraction support.
///
/// Every account can optionally specify custom validation logic via
/// `validation_code_hash`, enabling signature scheme upgrades without
/// a hard fork.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Hash of the PQ public key used for default signature verification.
    pub pq_pubkey_hash: ShellHash,
    /// Transaction count.
    pub nonce: u64,
    /// Balance in native token (wei-equivalent).
    pub balance: U256,
    /// Custom validation logic code hash (None = default Dilithium).
    /// Enables Account Abstraction: users can upgrade their signature scheme.
    pub validation_code_hash: Option<ShellHash>,
    /// Contract code hash (None = externally owned account).
    pub code_hash: Option<ShellHash>,
    /// Root of the account's storage trie.
    pub storage_root: ShellHash,
}

impl Account {
    /// Create a new externally-owned account with default Dilithium validation.
    pub fn new_eoa(pq_pubkey_hash: ShellHash, balance: U256) -> Self {
        Self {
            pq_pubkey_hash,
            nonce: 0,
            balance,
            validation_code_hash: None,
            code_hash: None,
            storage_root: ShellHash::ZERO,
        }
    }

    pub fn is_contract(&self) -> bool {
        self.code_hash.is_some()
    }

    pub fn has_custom_validation(&self) -> bool {
        self.validation_code_hash.is_some()
    }
}

fn encode_optional_hash(hash: &Option<ShellHash>, out: &mut dyn alloy_rlp::BufMut) {
    match hash {
        Some(h) => h.encode(out),
        None => {
            let empty: &[u8] = &[];
            empty.encode(out);
        }
    }
}

fn optional_hash_len(hash: &Option<ShellHash>) -> usize {
    match hash {
        Some(h) => h.length(),
        None => 1, // 0x80
    }
}

impl Encodable for Account {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let header = alloy_rlp::Header {
            list: true,
            payload_length: self.fields_len(),
        };
        header.encode(out);
        self.pq_pubkey_hash.encode(out);
        self.nonce.encode(out);
        self.balance.encode(out);
        encode_optional_hash(&self.validation_code_hash, out);
        encode_optional_hash(&self.code_hash, out);
        self.storage_root.encode(out);
    }

    fn length(&self) -> usize {
        let payload = self.fields_len();
        alloy_rlp::Header { list: true, payload_length: payload }.length() + payload
    }
}

impl Account {
    fn fields_len(&self) -> usize {
        self.pq_pubkey_hash.length()
            + self.nonce.length()
            + self.balance.length()
            + optional_hash_len(&self.validation_code_hash)
            + optional_hash_len(&self.code_hash)
            + self.storage_root.length()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::keccak256;

    #[test]
    fn new_eoa() {
        let pubkey_hash = keccak256(b"dilithium-pubkey");
        let acct = Account::new_eoa(pubkey_hash, U256::from(1000));
        assert!(!acct.is_contract());
        assert!(!acct.has_custom_validation());
        assert_eq!(acct.nonce, 0);
    }

    #[test]
    fn serde_roundtrip() {
        let acct = Account::new_eoa(keccak256(b"test"), U256::from(42));
        let json = serde_json::to_string(&acct).unwrap();
        let acct2: Account = serde_json::from_str(&json).unwrap();
        assert_eq!(acct, acct2);
    }

    #[test]
    fn account_rlp_roundtrip() {
        let acct = Account::new_eoa(keccak256(b"rlp-test"), U256::from(999));
        let mut buf = Vec::new();
        acct.encode(&mut buf);
        assert!(!buf.is_empty());
        // Hash is deterministic
        let h1 = keccak256(&buf);
        let mut buf2 = Vec::new();
        acct.encode(&mut buf2);
        assert_eq!(h1, keccak256(&buf2));
    }

    #[test]
    fn account_with_custom_validation_rlp() {
        let mut acct = Account::new_eoa(keccak256(b"aa-test"), U256::from(0));
        acct.validation_code_hash = Some(keccak256(b"custom-validator"));
        acct.code_hash = Some(keccak256(b"contract-code"));

        let mut buf = Vec::new();
        acct.encode(&mut buf);

        // Should be longer than a plain EOA (validation + code hashes present)
        let plain = Account::new_eoa(keccak256(b"aa-test"), U256::from(0));
        let mut buf_plain = Vec::new();
        plain.encode(&mut buf_plain);
        assert!(buf.len() > buf_plain.len());
    }
}
