use serde::{Deserialize, Serialize};
use shell_primitives::{ShellHash, U256};

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
}
