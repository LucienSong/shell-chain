//! Core transaction pool implementation.

use std::collections::{BTreeMap, HashMap};

use parking_lot::RwLock;

use shell_core::SignedTransaction;
use shell_crypto::Verifier;
use shell_primitives::{Address, ShellHash};

use crate::{MempoolConfig, MempoolError};

/// Thread-safe transaction pool.
///
/// Accepts validated transactions, orders them by priority fee, enforces
/// per-sender nonce ordering, and provides block-building APIs.
///
/// # Ordering
///
/// Transactions are globally ordered by `(max_priority_fee_per_gas DESC, nonce ASC)`.
/// Within a single sender queue, transactions are strictly nonce-ordered.
///
/// # Thread Safety
///
/// All public methods acquire an internal `RwLock`. The pool is `Send + Sync`.
pub struct TxPool {
    config: MempoolConfig,
    inner: RwLock<PoolInner>,
}

/// Internal mutable state behind the lock.
struct PoolInner {
    /// All transactions by hash for O(1) lookup.
    by_hash: HashMap<ShellHash, PoolEntry>,
    /// Per-sender queues ordered by nonce.
    by_sender: HashMap<Address, BTreeMap<u64, ShellHash>>,
    /// Global ordering index: (priority_fee DESC, arrival_seq ASC) → tx hash.
    /// Uses negated priority_fee for natural BTreeMap ascending order.
    by_priority: BTreeMap<PriorityKey, ShellHash>,
    /// Monotonic counter for FIFO tie-breaking at equal fee levels.
    seq: u64,
}

/// Entry in the pool holding the transaction and metadata.
struct PoolEntry {
    tx: SignedTransaction,
    priority_key: PriorityKey,
}

/// Composite ordering key: higher fee first, then earlier arrival first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PriorityKey {
    /// Negated priority fee so BTreeMap ascending = highest fee first.
    neg_priority_fee: i128,
    /// Monotonic sequence number for FIFO within same fee tier.
    seq: u64,
}

impl TxPool {
    /// Create a new transaction pool with the given configuration.
    pub fn new(config: MempoolConfig) -> Self {
        Self {
            config,
            inner: RwLock::new(PoolInner {
                by_hash: HashMap::new(),
                by_sender: HashMap::new(),
                by_priority: BTreeMap::new(),
                seq: 0,
            }),
        }
    }

    /// Insert a signed transaction into the pool after lightweight validation.
    ///
    /// Performs: chain ID check, gas price floor, duplicate detection,
    /// signature verification, address derivation, and capacity enforcement.
    ///
    /// Does NOT check on-chain nonce or balance (that is the executor's job).
    pub fn insert<V: Verifier>(
        &self,
        tx: SignedTransaction,
        verifier: &V,
        known_pubkeys: &dyn Fn(&Address) -> Option<Vec<u8>>,
    ) -> Result<ShellHash, MempoolError> {
        // --- Stateless checks (before acquiring lock) ---
        self.validate_stateless(&tx, verifier, known_pubkeys)?;

        let hash = tx.hash();
        let sender = tx.sender();
        let nonce = tx.tx.nonce;
        let priority_fee = tx.tx.max_priority_fee_per_gas;

        // --- Stateful checks (under write lock) ---
        let mut inner = self.inner.write();

        // Duplicate check
        if inner.by_hash.contains_key(&hash) {
            return Err(MempoolError::Duplicate { hash });
        }

        // Per-sender limit
        let sender_count = inner
            .by_sender
            .get(&sender)
            .map_or(0, |q| q.len());
        if sender_count >= self.config.max_per_sender {
            return Err(MempoolError::SenderQueueFull {
                sender,
                count: sender_count,
            });
        }

        // Nonce-too-low: reject if sender already has a tx at this nonce
        if let Some(sender_q) = inner.by_sender.get(&sender) {
            if sender_q.contains_key(&nonce) {
                // Replacement not supported yet — reject duplicate nonce
                let highest = sender_q.keys().next_back().copied().unwrap_or(0);
                return Err(MempoolError::NonceTooLow {
                    got: nonce,
                    pending: highest,
                });
            }
        }

        // Pool full — evict lowest priority tx
        if inner.by_hash.len() >= self.config.max_pool_size {
            // Last entry = lowest priority (highest neg_fee, highest seq)
            if let Some((&evict_key, _)) = inner.by_priority.last_key_value() {
                // Only evict if incoming tx has higher priority
                let incoming_neg = -(priority_fee as i128);
                if incoming_neg >= evict_key.neg_priority_fee {
                    return Err(MempoolError::PoolFull {
                        capacity: self.config.max_pool_size,
                    });
                }
                // Evict the worst tx
                if let Some(evict_hash) = inner.by_priority.remove(&evict_key) {
                    Self::remove_entry(&mut inner, &evict_hash);
                }
            } else {
                return Err(MempoolError::PoolFull {
                    capacity: self.config.max_pool_size,
                });
            }
        }

        // --- Insert ---
        let seq = inner.seq;
        inner.seq += 1;

        let priority_key = PriorityKey {
            neg_priority_fee: -(priority_fee as i128),
            seq,
        };

        inner.by_priority.insert(priority_key, hash);
        inner
            .by_sender
            .entry(sender)
            .or_default()
            .insert(nonce, hash);
        inner.by_hash.insert(
            hash,
            PoolEntry {
                tx,
                priority_key,
            },
        );

        Ok(hash)
    }

    /// Remove a transaction from the pool by hash.
    ///
    /// Returns `true` if the transaction was found and removed.
    pub fn remove(&self, hash: &ShellHash) -> bool {
        let mut inner = self.inner.write();
        Self::remove_entry(&mut inner, hash)
    }

    /// Remove a batch of transactions (e.g., after block inclusion).
    pub fn remove_batch(&self, hashes: &[ShellHash]) {
        let mut inner = self.inner.write();
        for hash in hashes {
            Self::remove_entry(&mut inner, hash);
        }
    }

    /// Get a transaction by hash.
    pub fn get(&self, hash: &ShellHash) -> Option<SignedTransaction> {
        let inner = self.inner.read();
        inner.by_hash.get(hash).map(|e| e.tx.clone())
    }

    /// Check if a transaction is in the pool.
    pub fn contains(&self, hash: &ShellHash) -> bool {
        self.inner.read().by_hash.contains_key(hash)
    }

    /// Number of transactions currently in the pool.
    pub fn len(&self) -> usize {
        self.inner.read().by_hash.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.read().by_hash.is_empty()
    }

    /// Number of pending transactions for a specific sender.
    pub fn sender_count(&self, sender: &Address) -> usize {
        let inner = self.inner.read();
        inner.by_sender.get(sender).map_or(0, |q| q.len())
    }

    /// Collect the best transactions for block building, up to `limit`.
    ///
    /// Returns transactions ordered by priority fee (highest first).
    /// Within a sender, transactions are nonce-ordered.
    pub fn pending(&self, limit: usize) -> Vec<SignedTransaction> {
        let inner = self.inner.read();
        let mut result = Vec::with_capacity(limit.min(inner.by_hash.len()));

        for (_key, hash) in inner.by_priority.iter() {
            if result.len() >= limit {
                break;
            }
            if let Some(entry) = inner.by_hash.get(hash) {
                result.push(entry.tx.clone());
            }
        }
        result
    }

    /// Collect all pending transaction hashes for a specific sender,
    /// ordered by nonce ascending.
    pub fn sender_txs(&self, sender: &Address) -> Vec<ShellHash> {
        let inner = self.inner.read();
        inner
            .by_sender
            .get(sender)
            .map(|q| q.values().copied().collect())
            .unwrap_or_default()
    }

    // --- Private helpers ---

    /// Lightweight validation performed before acquiring the pool lock.
    fn validate_stateless<V: Verifier>(
        &self,
        tx: &SignedTransaction,
        verifier: &V,
        known_pubkeys: &dyn Fn(&Address) -> Option<Vec<u8>>,
    ) -> Result<(), MempoolError> {
        // Chain ID
        if tx.tx.chain_id != self.config.chain_id {
            return Err(MempoolError::ChainIdMismatch {
                expected: self.config.chain_id,
                got: tx.tx.chain_id,
            });
        }

        // Minimum gas price
        if tx.tx.max_fee_per_gas < self.config.min_gas_price {
            return Err(MempoolError::GasPriceTooLow {
                got: tx.tx.max_fee_per_gas,
                min: self.config.min_gas_price,
            });
        }

        // Resolve public key: from tx itself, or from known-pubkey lookup
        let pubkey = if let Some(ref pk) = tx.sender_pubkey {
            pk.clone()
        } else if let Some(pk) = known_pubkeys(&tx.sender()) {
            pk
        } else {
            return Err(MempoolError::PubkeyRequired {
                sender: tx.sender(),
            });
        };

        // Address derivation check
        let derived = Address::from_public_key(&pubkey);
        if derived != tx.sender() {
            return Err(MempoolError::AddressMismatch {
                from: tx.sender(),
                derived,
            });
        }

        // Signature verification
        let msg = tx.tx.hash();
        let valid = verifier.verify(&pubkey, msg.as_bytes(), &tx.signature)?;
        if !valid {
            return Err(MempoolError::InvalidSignature(
                "PQ signature verification failed".into(),
            ));
        }

        Ok(())
    }

    /// Remove a single entry from all indexes. Caller holds write lock.
    fn remove_entry(inner: &mut PoolInner, hash: &ShellHash) -> bool {
        if let Some(entry) = inner.by_hash.remove(hash) {
            let sender = entry.tx.sender();
            let nonce = entry.tx.tx.nonce;

            // Remove from priority index
            inner.by_priority.remove(&entry.priority_key);

            // Remove from sender queue
            if let Some(sender_q) = inner.by_sender.get_mut(&sender) {
                sender_q.remove(&nonce);
                if sender_q.is_empty() {
                    inner.by_sender.remove(&sender);
                }
            }
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_core::Transaction;
    use shell_crypto::{DilithiumSigner, DilithiumVerifier, Signer};
    use shell_primitives::Bytes;

    fn make_config() -> MempoolConfig {
        MempoolConfig {
            max_pool_size: 10,
            max_per_sender: 4,
            chain_id: 42,
            min_gas_price: 1,
        }
    }

    /// Create a signed transaction from a fresh keypair.
    fn make_signed_tx(nonce: u64, priority_fee: u64) -> (SignedTransaction, Vec<u8>) {
        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let from = Address::from_public_key(&pubkey);

        let tx = Transaction {
            chain_id: 42,
            nonce,
            to: Some(Address::from_public_key(b"recipient-placeholder-key-data-for-address")),
            value: Default::default(),
            data: Bytes::default(),
            gas_limit: 21_000,
            max_fee_per_gas: priority_fee + 10,
            max_priority_fee_per_gas: priority_fee,
        };

        let sig = signer.sign(tx.hash().as_bytes()).unwrap();
        let signed = SignedTransaction::with_pubkey(from, tx, sig, pubkey.clone());
        (signed, pubkey)
    }

    /// Convenience: create a signed tx from an existing signer for multi-nonce tests.
    fn make_signed_tx_with_signer(
        signer: &DilithiumSigner,
        pubkey: &[u8],
        nonce: u64,
        priority_fee: u64,
    ) -> SignedTransaction {
        let from = Address::from_public_key(pubkey);
        let tx = Transaction {
            chain_id: 42,
            nonce,
            to: Some(Address::from_public_key(b"recipient-placeholder-key-data-for-address")),
            value: Default::default(),
            data: Bytes::default(),
            gas_limit: 21_000,
            max_fee_per_gas: priority_fee + 10,
            max_priority_fee_per_gas: priority_fee,
        };
        let sig = signer.sign(tx.hash().as_bytes()).unwrap();
        SignedTransaction::with_pubkey(from, tx, sig, pubkey.to_vec())
    }

    fn no_known_pubkeys(_addr: &Address) -> Option<Vec<u8>> {
        None
    }

    #[test]
    fn insert_and_get() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;
        let (tx, _pk) = make_signed_tx(0, 100);
        let hash = tx.hash();

        let result = pool.insert(tx, &verifier, &no_known_pubkeys);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), hash);
        assert_eq!(pool.len(), 1);
        assert!(pool.contains(&hash));
        assert!(pool.get(&hash).is_some());
    }

    #[test]
    fn reject_duplicate() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;
        let (tx, _pk) = make_signed_tx(0, 100);

        pool.insert(tx.clone(), &verifier, &no_known_pubkeys).unwrap();
        let err = pool.insert(tx, &verifier, &no_known_pubkeys).unwrap_err();
        assert!(matches!(err, MempoolError::Duplicate { .. }));
    }

    #[test]
    fn reject_wrong_chain_id() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;

        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let from = Address::from_public_key(&pubkey);
        let tx = Transaction {
            chain_id: 999, // wrong
            nonce: 0,
            to: None,
            value: Default::default(),
            data: Bytes::default(),
            gas_limit: 21_000,
            max_fee_per_gas: 100,
            max_priority_fee_per_gas: 50,
        };
        let sig = signer.sign(tx.hash().as_bytes()).unwrap();
        let signed = SignedTransaction::with_pubkey(from, tx, sig, pubkey);

        let err = pool.insert(signed, &verifier, &no_known_pubkeys).unwrap_err();
        assert!(matches!(err, MempoolError::ChainIdMismatch { .. }));
    }

    #[test]
    fn reject_gas_price_too_low() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;

        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let from = Address::from_public_key(&pubkey);
        let tx = Transaction {
            chain_id: 42,
            nonce: 0,
            to: None,
            value: Default::default(),
            data: Bytes::default(),
            gas_limit: 21_000,
            max_fee_per_gas: 0, // below min_gas_price=1
            max_priority_fee_per_gas: 0,
        };
        let sig = signer.sign(tx.hash().as_bytes()).unwrap();
        let signed = SignedTransaction::with_pubkey(from, tx, sig, pubkey);

        let err = pool.insert(signed, &verifier, &no_known_pubkeys).unwrap_err();
        assert!(matches!(err, MempoolError::GasPriceTooLow { .. }));
    }

    #[test]
    fn reject_invalid_signature() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;

        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let from = Address::from_public_key(&pubkey);
        let tx = Transaction {
            chain_id: 42,
            nonce: 0,
            to: None,
            value: Default::default(),
            data: Bytes::default(),
            gas_limit: 21_000,
            max_fee_per_gas: 100,
            max_priority_fee_per_gas: 50,
        };
        // Sign a different message to produce invalid sig
        let bad_sig = signer.sign(b"wrong-message").unwrap();
        let signed = SignedTransaction::with_pubkey(from, tx, bad_sig, pubkey);

        let err = pool.insert(signed, &verifier, &no_known_pubkeys).unwrap_err();
        assert!(matches!(err, MempoolError::InvalidSignature(_)));
    }

    #[test]
    fn reject_address_mismatch() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;

        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let wrong_from = Address::from_public_key(b"different-key-bytes");
        let tx = Transaction {
            chain_id: 42,
            nonce: 0,
            to: None,
            value: Default::default(),
            data: Bytes::default(),
            gas_limit: 21_000,
            max_fee_per_gas: 100,
            max_priority_fee_per_gas: 50,
        };
        let sig = signer.sign(tx.hash().as_bytes()).unwrap();
        let signed = SignedTransaction::with_pubkey(wrong_from, tx, sig, pubkey);

        let err = pool.insert(signed, &verifier, &no_known_pubkeys).unwrap_err();
        assert!(matches!(err, MempoolError::AddressMismatch { .. }));
    }

    #[test]
    fn reject_missing_pubkey() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;

        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let from = Address::from_public_key(&pubkey);
        let tx = Transaction {
            chain_id: 42,
            nonce: 0,
            to: None,
            value: Default::default(),
            data: Bytes::default(),
            gas_limit: 21_000,
            max_fee_per_gas: 100,
            max_priority_fee_per_gas: 50,
        };
        let sig = signer.sign(tx.hash().as_bytes()).unwrap();
        // No pubkey attached, and lookup returns None
        let signed = SignedTransaction::new(from, tx, sig);

        let err = pool.insert(signed, &verifier, &no_known_pubkeys).unwrap_err();
        assert!(matches!(err, MempoolError::PubkeyRequired { .. }));
    }

    #[test]
    fn remove_transaction() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;
        let (tx, _pk) = make_signed_tx(0, 100);
        let hash = pool.insert(tx, &verifier, &no_known_pubkeys).unwrap();

        assert!(pool.remove(&hash));
        assert!(!pool.contains(&hash));
        assert_eq!(pool.len(), 0);
        assert!(!pool.remove(&hash)); // already gone
    }

    #[test]
    fn remove_batch() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;

        let (tx1, _) = make_signed_tx(0, 100);
        let (tx2, _) = make_signed_tx(0, 200);
        let h1 = pool.insert(tx1, &verifier, &no_known_pubkeys).unwrap();
        let h2 = pool.insert(tx2, &verifier, &no_known_pubkeys).unwrap();

        pool.remove_batch(&[h1, h2]);
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn pending_ordered_by_priority_fee() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;

        let (tx_low, _) = make_signed_tx(0, 10);
        let (tx_mid, _) = make_signed_tx(0, 50);
        let (tx_high, _) = make_signed_tx(0, 100);

        pool.insert(tx_low, &verifier, &no_known_pubkeys).unwrap();
        pool.insert(tx_mid, &verifier, &no_known_pubkeys).unwrap();
        pool.insert(tx_high, &verifier, &no_known_pubkeys).unwrap();

        let pending = pool.pending(10);
        assert_eq!(pending.len(), 3);
        // Highest priority fee first
        assert_eq!(pending[0].tx.max_priority_fee_per_gas, 100);
        assert_eq!(pending[1].tx.max_priority_fee_per_gas, 50);
        assert_eq!(pending[2].tx.max_priority_fee_per_gas, 10);
    }

    #[test]
    fn per_sender_nonce_ordering() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;

        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();

        // Insert nonces out of order
        let tx2 = make_signed_tx_with_signer(&signer, &pubkey, 2, 50);
        let tx0 = make_signed_tx_with_signer(&signer, &pubkey, 0, 50);
        let tx1 = make_signed_tx_with_signer(&signer, &pubkey, 1, 50);

        pool.insert(tx2, &verifier, &no_known_pubkeys).unwrap();
        pool.insert(tx0, &verifier, &no_known_pubkeys).unwrap();
        pool.insert(tx1, &verifier, &no_known_pubkeys).unwrap();

        let sender = Address::from_public_key(&pubkey);
        let sender_hashes = pool.sender_txs(&sender);
        assert_eq!(sender_hashes.len(), 3);
        assert_eq!(pool.sender_count(&sender), 3);
    }

    #[test]
    fn sender_queue_full() {
        let config = MempoolConfig {
            max_per_sender: 2,
            ..make_config()
        };
        let pool = TxPool::new(config);
        let verifier = DilithiumVerifier;

        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();

        let tx0 = make_signed_tx_with_signer(&signer, &pubkey, 0, 50);
        let tx1 = make_signed_tx_with_signer(&signer, &pubkey, 1, 50);
        let tx2 = make_signed_tx_with_signer(&signer, &pubkey, 2, 50);

        pool.insert(tx0, &verifier, &no_known_pubkeys).unwrap();
        pool.insert(tx1, &verifier, &no_known_pubkeys).unwrap();
        let err = pool.insert(tx2, &verifier, &no_known_pubkeys).unwrap_err();
        assert!(matches!(err, MempoolError::SenderQueueFull { .. }));
    }

    #[test]
    fn pool_full_evicts_lowest_priority() {
        let config = MempoolConfig {
            max_pool_size: 2,
            ..make_config()
        };
        let pool = TxPool::new(config);
        let verifier = DilithiumVerifier;

        let (tx_low, _) = make_signed_tx(0, 10);
        let (tx_mid, _) = make_signed_tx(0, 50);
        let low_hash = tx_low.hash();

        pool.insert(tx_low, &verifier, &no_known_pubkeys).unwrap();
        pool.insert(tx_mid, &verifier, &no_known_pubkeys).unwrap();

        // Pool is full. Insert a higher priority tx — should evict tx_low.
        let (tx_high, _) = make_signed_tx(0, 100);
        pool.insert(tx_high, &verifier, &no_known_pubkeys).unwrap();

        assert_eq!(pool.len(), 2);
        assert!(!pool.contains(&low_hash)); // evicted
    }

    #[test]
    fn pool_full_rejects_low_priority() {
        let config = MempoolConfig {
            max_pool_size: 2,
            ..make_config()
        };
        let pool = TxPool::new(config);
        let verifier = DilithiumVerifier;

        let (tx1, _) = make_signed_tx(0, 50);
        let (tx2, _) = make_signed_tx(0, 100);
        pool.insert(tx1, &verifier, &no_known_pubkeys).unwrap();
        pool.insert(tx2, &verifier, &no_known_pubkeys).unwrap();

        // Try to insert a tx with lower priority than worst in pool
        let (tx_too_low, _) = make_signed_tx(0, 5);
        let err = pool.insert(tx_too_low, &verifier, &no_known_pubkeys).unwrap_err();
        assert!(matches!(err, MempoolError::PoolFull { .. }));
    }

    #[test]
    fn known_pubkey_lookup() {
        let pool = TxPool::new(make_config());
        let verifier = DilithiumVerifier;

        let signer = DilithiumSigner::generate();
        let pubkey = signer.public_key().to_vec();
        let from = Address::from_public_key(&pubkey);
        let tx = Transaction {
            chain_id: 42,
            nonce: 0,
            to: None,
            value: Default::default(),
            data: Bytes::default(),
            gas_limit: 21_000,
            max_fee_per_gas: 100,
            max_priority_fee_per_gas: 50,
        };
        let sig = signer.sign(tx.hash().as_bytes()).unwrap();
        // NO pubkey in transaction — rely on lookup
        let signed = SignedTransaction::new(from, tx, sig);

        let pk_clone = pubkey.clone();
        let lookup = move |addr: &Address| -> Option<Vec<u8>> {
            if *addr == Address::from_public_key(&pk_clone) {
                Some(pk_clone.clone())
            } else {
                None
            }
        };

        let result = pool.insert(signed, &verifier, &lookup);
        assert!(result.is_ok());
    }

    #[test]
    fn empty_pool() {
        let pool = TxPool::new(make_config());
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
        assert_eq!(pool.pending(10).len(), 0);
    }
}
