use std::sync::Arc;

use alloy_rlp::{Decodable, Encodable};
use shell_core::Account;
use shell_primitives::{keccak256, Address, ShellHash, U256};

use crate::{KvStore, MerkleTrie, StorageError};

/// Manages the world state (all accounts and their storage).
///
/// Accounts are stored in a Merkle Patricia Trie keyed by `keccak256(address)`.
/// Each account may have its own storage sub-trie whose nodes share the same
/// underlying [`KvStore`].
pub struct WorldState<S: KvStore + 'static> {
    account_trie: MerkleTrie<S>,
    store: Arc<S>,
}

impl<S: KvStore + 'static> WorldState<S> {
    /// Create a new empty world state.
    pub fn new(store: Arc<S>) -> Self {
        Self {
            account_trie: MerkleTrie::new(Arc::clone(&store)),
            store,
        }
    }

    /// Open world state at an existing state root.
    pub fn at_root(store: Arc<S>, state_root: &ShellHash) -> Result<Self, StorageError> {
        let trie = MerkleTrie::at_root(Arc::clone(&store), state_root.as_bytes())?;
        Ok(Self {
            account_trie: trie,
            store,
        })
    }

    fn account_key(address: &Address) -> Vec<u8> {
        keccak256(address.as_bytes()).as_bytes().to_vec()
    }

    /// Retrieve an account by address. Returns `None` if the account does not exist.
    pub fn get_account(&self, address: &Address) -> Result<Option<Account>, StorageError> {
        let key = Self::account_key(address);
        match self.account_trie.get(&key)? {
            Some(data) => {
                let account = Account::decode(&mut &data[..])
                    .map_err(|e| StorageError::Codec(e.to_string()))?;
                Ok(Some(account))
            }
            None => Ok(None),
        }
    }

    /// Write an account to the state trie.
    pub fn set_account(
        &mut self,
        address: &Address,
        account: &Account,
    ) -> Result<(), StorageError> {
        let key = Self::account_key(address);
        let mut buf = Vec::new();
        account.encode(&mut buf);
        self.account_trie.insert(&key, &buf)
    }

    fn get_or_default(&self, address: &Address) -> Result<Account, StorageError> {
        Ok(self
            .get_account(address)?
            .unwrap_or_else(|| Account::new_eoa(ShellHash::ZERO, U256::ZERO)))
    }

    // ── Balance helpers ────────────────────────────────────────

    pub fn get_balance(&self, address: &Address) -> Result<U256, StorageError> {
        Ok(self.get_or_default(address)?.balance)
    }

    pub fn add_balance(&mut self, address: &Address, amount: U256) -> Result<(), StorageError> {
        let mut account = self.get_or_default(address)?;
        account.balance = account
            .balance
            .checked_add(amount)
            .ok_or_else(|| StorageError::State("balance overflow".into()))?;
        self.set_account(address, &account)
    }

    pub fn sub_balance(&mut self, address: &Address, amount: U256) -> Result<(), StorageError> {
        let mut account = self.get_or_default(address)?;
        if account.balance < amount {
            return Err(StorageError::State("insufficient balance".into()));
        }
        account.balance -= amount;
        self.set_account(address, &account)
    }

    // ── Nonce helpers ──────────────────────────────────────────

    pub fn get_nonce(&self, address: &Address) -> Result<u64, StorageError> {
        Ok(self.get_or_default(address)?.nonce)
    }

    pub fn increment_nonce(&mut self, address: &Address) -> Result<(), StorageError> {
        let mut account = self.get_or_default(address)?;
        account.nonce = account
            .nonce
            .checked_add(1)
            .ok_or_else(|| StorageError::State("nonce overflow".into()))?;
        self.set_account(address, &account)
    }

    // ── Contract storage ───────────────────────────────────────

    /// Read a value from an account's storage trie.
    pub fn get_storage(
        &self,
        address: &Address,
        key: &ShellHash,
    ) -> Result<ShellHash, StorageError> {
        let account = match self.get_account(address)? {
            Some(a) => a,
            None => return Ok(ShellHash::ZERO),
        };
        if account.storage_root == ShellHash::ZERO {
            return Ok(ShellHash::ZERO);
        }
        let storage_trie =
            MerkleTrie::at_root(Arc::clone(&self.store), account.storage_root.as_bytes())?;
        let storage_key = keccak256(key.as_bytes());
        match storage_trie.get(storage_key.as_bytes())? {
            Some(data) => {
                ShellHash::try_from_slice(&data).map_err(|e| StorageError::Codec(e.to_string()))
            }
            None => Ok(ShellHash::ZERO),
        }
    }

    /// Write a value to an account's storage trie.
    /// Writing `ShellHash::ZERO` removes the key.
    pub fn set_storage(
        &mut self,
        address: &Address,
        key: &ShellHash,
        value: &ShellHash,
    ) -> Result<(), StorageError> {
        let mut account = self.get_or_default(address)?;

        let mut storage_trie = if account.storage_root == ShellHash::ZERO {
            MerkleTrie::new(Arc::clone(&self.store))
        } else {
            MerkleTrie::at_root(Arc::clone(&self.store), account.storage_root.as_bytes())?
        };

        let storage_key = keccak256(key.as_bytes());
        if *value == ShellHash::ZERO {
            storage_trie.remove(storage_key.as_bytes())?;
        } else {
            storage_trie.insert(storage_key.as_bytes(), value.as_bytes())?;
        }

        let new_root = storage_trie.root_hash()?;
        account.storage_root = ShellHash::from(new_root);
        self.set_account(address, &account)
    }

    // ── Code helpers ───────────────────────────────────────────

    pub fn get_code_hash(&self, address: &Address) -> Result<Option<ShellHash>, StorageError> {
        Ok(self.get_or_default(address)?.code_hash)
    }

    pub fn set_code_hash(
        &mut self,
        address: &Address,
        code_hash: ShellHash,
    ) -> Result<(), StorageError> {
        let mut account = self.get_or_default(address)?;
        account.code_hash = Some(code_hash);
        self.set_account(address, &account)
    }

    // ── State root ─────────────────────────────────────────────

    /// Compute and return the current state root hash.
    pub fn state_root(&mut self) -> Result<ShellHash, StorageError> {
        let root = self.account_trie.root_hash()?;
        Ok(ShellHash::from(root))
    }

    /// Check whether an account exists in the state.
    pub fn exists(&self, address: &Address) -> Result<bool, StorageError> {
        Ok(self.get_account(address)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryDb;

    fn test_store() -> Arc<MemoryDb> {
        Arc::new(MemoryDb::new())
    }

    fn test_address(seed: &[u8]) -> Address {
        Address::from_public_key(keccak256(seed).as_bytes())
    }

    #[test]
    fn empty_state_has_deterministic_root() {
        let store = test_store();
        let mut ws1 = WorldState::new(Arc::clone(&store));

        let store2 = test_store();
        let mut ws2 = WorldState::new(store2);

        assert_eq!(ws1.state_root().unwrap(), ws2.state_root().unwrap());
    }

    #[test]
    fn get_nonexistent_account_returns_none() {
        let store = test_store();
        let ws = WorldState::new(store);
        let addr = test_address(b"nobody");
        assert!(ws.get_account(&addr).unwrap().is_none());
    }

    #[test]
    fn set_and_get_account() {
        let store = test_store();
        let mut ws = WorldState::new(store);
        let addr = test_address(b"alice");
        let acct = Account::new_eoa(keccak256(b"alice-pk"), U256::from(1000));

        ws.set_account(&addr, &acct).unwrap();
        let loaded = ws.get_account(&addr).unwrap().unwrap();
        assert_eq!(loaded.balance, U256::from(1000));
        assert_eq!(loaded.nonce, 0);
    }

    #[test]
    fn balance_add_and_sub() {
        let store = test_store();
        let mut ws = WorldState::new(store);
        let addr = test_address(b"bob");

        ws.add_balance(&addr, U256::from(500)).unwrap();
        assert_eq!(ws.get_balance(&addr).unwrap(), U256::from(500));

        ws.sub_balance(&addr, U256::from(200)).unwrap();
        assert_eq!(ws.get_balance(&addr).unwrap(), U256::from(300));
    }

    #[test]
    fn sub_balance_insufficient_fails() {
        let store = test_store();
        let mut ws = WorldState::new(store);
        let addr = test_address(b"broke");

        ws.add_balance(&addr, U256::from(100)).unwrap();
        let err = ws.sub_balance(&addr, U256::from(200)).unwrap_err();
        assert!(matches!(err, StorageError::State(_)));
    }

    #[test]
    fn nonce_increment() {
        let store = test_store();
        let mut ws = WorldState::new(store);
        let addr = test_address(b"carol");

        assert_eq!(ws.get_nonce(&addr).unwrap(), 0);
        ws.increment_nonce(&addr).unwrap();
        ws.increment_nonce(&addr).unwrap();
        assert_eq!(ws.get_nonce(&addr).unwrap(), 2);
    }

    #[test]
    fn state_root_changes_with_accounts() {
        let store = test_store();
        let mut ws = WorldState::new(store);
        let root_empty = ws.state_root().unwrap();

        let addr = test_address(b"dave");
        ws.add_balance(&addr, U256::from(42)).unwrap();
        let root_with_account = ws.state_root().unwrap();

        assert_ne!(root_empty, root_with_account);
    }

    #[test]
    fn state_root_deterministic() {
        let store1 = test_store();
        let mut ws1 = WorldState::new(store1);
        let store2 = test_store();
        let mut ws2 = WorldState::new(store2);

        let addr = test_address(b"eve");
        ws1.add_balance(&addr, U256::from(100)).unwrap();
        ws2.add_balance(&addr, U256::from(100)).unwrap();

        assert_eq!(ws1.state_root().unwrap(), ws2.state_root().unwrap());
    }

    #[test]
    fn reopen_at_root() {
        let store = test_store();
        let mut ws = WorldState::new(Arc::clone(&store));
        let addr = test_address(b"frank");
        ws.add_balance(&addr, U256::from(777)).unwrap();
        let root = ws.state_root().unwrap();

        let ws2 = WorldState::at_root(store, &root).unwrap();
        assert_eq!(ws2.get_balance(&addr).unwrap(), U256::from(777));
    }

    #[test]
    fn contract_storage_set_and_get() {
        let store = test_store();
        let mut ws = WorldState::new(store);
        let addr = test_address(b"contract");

        let slot = keccak256(b"slot-0");
        let value = keccak256(b"value-0");

        ws.set_storage(&addr, &slot, &value).unwrap();
        assert_eq!(ws.get_storage(&addr, &slot).unwrap(), value);
    }

    #[test]
    fn contract_storage_delete() {
        let store = test_store();
        let mut ws = WorldState::new(store);
        let addr = test_address(b"contract2");

        let slot = keccak256(b"slot-1");
        let value = keccak256(b"value-1");

        ws.set_storage(&addr, &slot, &value).unwrap();
        ws.set_storage(&addr, &slot, &ShellHash::ZERO).unwrap();
        assert_eq!(ws.get_storage(&addr, &slot).unwrap(), ShellHash::ZERO);
    }

    #[test]
    fn exists_check() {
        let store = test_store();
        let mut ws = WorldState::new(store);
        let addr = test_address(b"ghost");

        assert!(!ws.exists(&addr).unwrap());
        ws.add_balance(&addr, U256::from(1)).unwrap();
        assert!(ws.exists(&addr).unwrap());
    }
}
