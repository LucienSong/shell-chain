//! Transaction validation pipeline.
//!
//! Performs pre-EVM checks on incoming signed transactions:
//! 1. **PQ signature verification** — verifies Dilithium3 signature
//! 2. **Address derivation check** — ensures `from` matches pubkey
//! 3. **Pubkey hybrid registration** — registers pubkey on first use
//! 4. **Nonce check** — tx.nonce must equal account.nonce
//! 5. **Balance check** — sender must afford gas_limit × max_fee_per_gas + value

use shell_core::SignedTransaction;
use shell_crypto::Verifier;
use shell_primitives::{Address, U256};
use shell_storage::{ChainStore, KvStore, StorageError, WorldState};

/// Errors returned during transaction validation.
#[derive(Debug, thiserror::Error)]
pub enum TxValidationError {
    #[error("pubkey not found: no sender_pubkey in tx and no registered pubkey on-chain")]
    PubkeyNotFound,

    #[error("address mismatch: from={from} but pubkey derives {derived}")]
    AddressMismatch { from: Address, derived: Address },

    #[error("signature verification failed")]
    SignatureInvalid,

    #[error("nonce mismatch: expected {expected}, got {got}")]
    NonceMismatch { expected: u64, got: u64 },

    #[error("insufficient balance: need {needed}, have {have}")]
    InsufficientBalance { needed: U256, have: U256 },

    #[error("chain_id mismatch: expected {expected}, got {got}")]
    ChainIdMismatch { expected: u64, got: u64 },

    #[error("gas limit below intrinsic: {0}")]
    GasTooLow(u64),

    #[error("crypto error: {0}")]
    Crypto(#[from] shell_crypto::CryptoError),

    #[error("storage: {0}")]
    Storage(#[from] StorageError),
}

/// Minimum gas for a plain transfer (no data).
const INTRINSIC_GAS_TX: u64 = 21_000;
/// Per-byte cost for non-zero calldata.
const GAS_PER_NONZERO_BYTE: u64 = 16;
/// Per-byte cost for zero calldata.
const GAS_PER_ZERO_BYTE: u64 = 4;
/// Extra gas for contract creation.
const GAS_CONTRACT_CREATION: u64 = 32_000;

/// Validate a signed transaction before EVM execution.
///
/// This function performs the full pre-execution validation pipeline:
///
/// 1. **Chain ID** — must match the expected chain ID
/// 2. **Intrinsic gas** — gas_limit must cover base cost + calldata
/// 3. **PQ pubkey resolution** — resolves via tx field or on-chain registry
/// 4. **Address derivation** — `from` must equal `keccak256(pubkey)[12..]`
/// 5. **Signature verification** — Dilithium3 sig over tx hash
/// 6. **Pubkey registration** — if first time, writes pubkey to ChainStore
/// 7. **Nonce** — must equal account's current nonce
/// 8. **Balance** — must afford `gas_limit * max_fee_per_gas + value`
///
/// Returns the resolved public key bytes on success (needed by the executor
/// to know whether registration occurred).
pub fn validate_tx<S: KvStore + 'static, V: Verifier>(
    signed_tx: &SignedTransaction,
    world_state: &WorldState<S>,
    chain_store: &ChainStore<S>,
    verifier: &V,
    expected_chain_id: u64,
) -> Result<Vec<u8>, TxValidationError> {
    let tx = &signed_tx.tx;

    // 1. Chain ID check
    if tx.chain_id != expected_chain_id {
        return Err(TxValidationError::ChainIdMismatch {
            expected: expected_chain_id,
            got: tx.chain_id,
        });
    }

    // 2. Intrinsic gas check
    let intrinsic = compute_intrinsic_gas(tx.data.as_ref(), tx.is_contract_creation());
    if tx.gas_limit < intrinsic {
        return Err(TxValidationError::GasTooLow(tx.gas_limit));
    }

    // 3. Resolve PQ public key (hybrid model)
    let pubkey = resolve_pubkey(signed_tx, chain_store)?;

    // 4. Address derivation: from must match keccak256(pubkey)[12..]
    let derived = Address::from_public_key(&pubkey);
    if signed_tx.from != derived {
        return Err(TxValidationError::AddressMismatch {
            from: signed_tx.from,
            derived,
        });
    }

    // 5. Signature verification
    let tx_hash = signed_tx.hash();
    let valid = verifier.verify(&pubkey, tx_hash.as_bytes(), &signed_tx.signature)?;
    if !valid {
        return Err(TxValidationError::SignatureInvalid);
    }

    // 6. Register pubkey if this is the first transaction (sender_pubkey present)
    if signed_tx.sender_pubkey.is_some() {
        // Only register if not already registered
        if chain_store.get_pubkey(&signed_tx.from)?.is_none() {
            chain_store.put_pubkey(&signed_tx.from, &pubkey)?;
        }
    }

    // 7. Nonce check
    let account_nonce = world_state.get_nonce(&signed_tx.from)?;
    if tx.nonce != account_nonce {
        return Err(TxValidationError::NonceMismatch {
            expected: account_nonce,
            got: tx.nonce,
        });
    }

    // 8. Balance check: sender must afford gas_limit * max_fee_per_gas + value
    //    Use checked arithmetic to prevent overflow panic (debug) / wrapping (release).
    let max_gas_cost = U256::from(tx.gas_limit).checked_mul(U256::from(tx.max_fee_per_gas));
    let needed = match max_gas_cost.and_then(|c| c.checked_add(tx.value)) {
        Some(n) => n,
        None => {
            // Overflow means the required amount exceeds U256::MAX — always insufficient.
            return Err(TxValidationError::InsufficientBalance {
                needed: U256::MAX,
                have: world_state.get_balance(&signed_tx.from)?,
            });
        }
    };
    let balance = world_state.get_balance(&signed_tx.from)?;
    if balance < needed {
        return Err(TxValidationError::InsufficientBalance {
            needed,
            have: balance,
        });
    }

    Ok(pubkey)
}

/// Resolve the public key for signature verification.
///
/// Hybrid model:
/// - If `sender_pubkey` is in the tx, use it (first-time registration)
/// - Otherwise, look up the on-chain registry
fn resolve_pubkey<S: KvStore>(
    signed_tx: &SignedTransaction,
    chain_store: &ChainStore<S>,
) -> Result<Vec<u8>, TxValidationError> {
    if let Some(pk) = &signed_tx.sender_pubkey {
        return Ok(pk.clone());
    }
    match chain_store.get_pubkey(&signed_tx.from)? {
        Some(pk) => Ok(pk),
        None => Err(TxValidationError::PubkeyNotFound),
    }
}

/// Compute intrinsic gas cost for a transaction.
///
/// Base cost (21,000) + calldata cost (4/byte zero, 16/byte nonzero) +
/// contract creation surcharge (32,000).
pub fn compute_intrinsic_gas(data: &[u8], is_create: bool) -> u64 {
    let mut gas = INTRINSIC_GAS_TX;
    if is_create {
        gas += GAS_CONTRACT_CREATION;
    }
    for &byte in data {
        if byte == 0 {
            gas += GAS_PER_ZERO_BYTE;
        } else {
            gas += GAS_PER_NONZERO_BYTE;
        }
    }
    gas
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_core::Transaction;
    use shell_crypto::{
        DilithiumSigner, DilithiumVerifier, PQSignature, Signer, SignatureType,
    };
    use shell_primitives::{Bytes, ShellHash};
    use shell_storage::MemoryDb;
    use std::sync::Arc;

    fn test_chain_id() -> u64 {
        1337
    }

    fn make_signer() -> DilithiumSigner {
        DilithiumSigner::generate()
    }

    fn setup_stores() -> (WorldState<MemoryDb>, ChainStore<MemoryDb>) {
        let ws = WorldState::new(Arc::new(MemoryDb::new()));
        let cs = ChainStore::new(Arc::new(MemoryDb::new()));
        (ws, cs)
    }

    fn fund_account(ws: &mut WorldState<MemoryDb>, addr: &Address, balance: U256) {
        use shell_core::Account;
        let account = Account {
            pq_pubkey_hash: ShellHash::ZERO,
            nonce: 0,
            balance,
            validation_code_hash: None,
            code_hash: None,
            storage_root: ShellHash::ZERO,
        };
        ws.set_account(addr, &account).unwrap();
    }

    fn simple_transfer(chain_id: u64, nonce: u64) -> Transaction {
        Transaction {
            chain_id,
            nonce,
            to: Some(Address::from([0x01; 20])),
            value: U256::from(100),
            data: Bytes::new(),
            gas_limit: 21_000,
            max_fee_per_gas: 10,
            max_priority_fee_per_gas: 1,
        }
    }

    fn sign_tx(
        signer: &DilithiumSigner,
        tx: Transaction,
        include_pubkey: bool,
    ) -> SignedTransaction {
        let from = Address::from_public_key(signer.public_key());
        let tx_hash = tx.hash();
        let sig = signer.sign(tx_hash.as_bytes()).unwrap();
        if include_pubkey {
            SignedTransaction::with_pubkey(from, tx, sig, signer.public_key().to_vec())
        } else {
            SignedTransaction::new(from, tx, sig)
        }
    }

    // ── Intrinsic gas ─────────────────────────────────────────

    #[test]
    fn intrinsic_gas_plain_transfer() {
        assert_eq!(compute_intrinsic_gas(&[], false), 21_000);
    }

    #[test]
    fn intrinsic_gas_with_data() {
        let data = vec![0x00, 0xFF, 0x00, 0x42];
        // 21000 + 4 + 16 + 4 + 16 = 21040
        assert_eq!(compute_intrinsic_gas(&data, false), 21_040);
    }

    #[test]
    fn intrinsic_gas_contract_creation() {
        assert_eq!(compute_intrinsic_gas(&[], true), 21_000 + 32_000);
    }

    // ── Happy path ────────────────────────────────────────────

    #[test]
    fn validate_first_tx_with_pubkey() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();
        let from = Address::from_public_key(signer.public_key());
        fund_account(&mut ws, &from, U256::from(1_000_000));

        let tx = simple_transfer(test_chain_id(), 0);
        let signed = sign_tx(&signer, tx, true);

        let verifier = DilithiumVerifier;
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(result.is_ok());

        // Pubkey should now be registered
        let registered = cs.get_pubkey(&from).unwrap();
        assert!(registered.is_some());
        assert_eq!(registered.unwrap(), signer.public_key());
    }

    #[test]
    fn validate_subsequent_tx_from_registry() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();
        let from = Address::from_public_key(signer.public_key());
        fund_account(&mut ws, &from, U256::from(1_000_000));

        // Pre-register pubkey
        cs.put_pubkey(&from, signer.public_key()).unwrap();

        // Tx without sender_pubkey
        let tx = simple_transfer(test_chain_id(), 0);
        let signed = sign_tx(&signer, tx, false);

        let verifier = DilithiumVerifier;
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(result.is_ok());
    }

    // ── Failure cases ─────────────────────────────────────────

    #[test]
    fn validate_wrong_chain_id() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();
        let from = Address::from_public_key(signer.public_key());
        fund_account(&mut ws, &from, U256::from(1_000_000));

        let tx = simple_transfer(9999, 0); // wrong chain_id
        let signed = sign_tx(&signer, tx, true);

        let verifier = DilithiumVerifier;
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(matches!(result, Err(TxValidationError::ChainIdMismatch { .. })));
    }

    #[test]
    fn validate_gas_too_low() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();
        let from = Address::from_public_key(signer.public_key());
        fund_account(&mut ws, &from, U256::from(1_000_000));

        let mut tx = simple_transfer(test_chain_id(), 0);
        tx.gas_limit = 100; // way too low
        let signed = sign_tx(&signer, tx, true);

        let verifier = DilithiumVerifier;
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(matches!(result, Err(TxValidationError::GasTooLow(_))));
    }

    #[test]
    fn validate_no_pubkey_anywhere() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();
        let from = Address::from_public_key(signer.public_key());
        fund_account(&mut ws, &from, U256::from(1_000_000));

        // No sender_pubkey and not registered
        let tx = simple_transfer(test_chain_id(), 0);
        let signed = sign_tx(&signer, tx, false);

        let verifier = DilithiumVerifier;
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(matches!(result, Err(TxValidationError::PubkeyNotFound)));
    }

    #[test]
    fn validate_address_mismatch() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();

        // Use a wrong from address
        let wrong_from = Address::from([0xFF; 20]);
        fund_account(&mut ws, &wrong_from, U256::from(1_000_000));

        let tx = simple_transfer(test_chain_id(), 0);
        let tx_hash = tx.hash();
        let sig = signer.sign(tx_hash.as_bytes()).unwrap();
        let signed = SignedTransaction::with_pubkey(
            wrong_from,
            tx,
            sig,
            signer.public_key().to_vec(),
        );

        let verifier = DilithiumVerifier;
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(matches!(result, Err(TxValidationError::AddressMismatch { .. })));
    }

    #[test]
    fn validate_bad_signature() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();
        let from = Address::from_public_key(signer.public_key());
        fund_account(&mut ws, &from, U256::from(1_000_000));

        let tx = simple_transfer(test_chain_id(), 0);
        let bad_sig = PQSignature::new(SignatureType::Dilithium3, vec![0xDE; 100]);
        let signed = SignedTransaction::with_pubkey(
            from,
            tx,
            bad_sig,
            signer.public_key().to_vec(),
        );

        let verifier = DilithiumVerifier;
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(matches!(result, Err(TxValidationError::SignatureInvalid)));
    }

    #[test]
    fn validate_nonce_mismatch() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();
        let from = Address::from_public_key(signer.public_key());
        fund_account(&mut ws, &from, U256::from(1_000_000));

        let tx = simple_transfer(test_chain_id(), 5); // nonce should be 0
        let signed = sign_tx(&signer, tx, true);

        let verifier = DilithiumVerifier;
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(matches!(result, Err(TxValidationError::NonceMismatch { expected: 0, got: 5 })));
    }

    #[test]
    fn validate_insufficient_balance() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();
        let from = Address::from_public_key(signer.public_key());
        fund_account(&mut ws, &from, U256::from(1)); // only 1 wei

        let tx = simple_transfer(test_chain_id(), 0); // needs 21000*10 + 100 = 210100
        let signed = sign_tx(&signer, tx, true);

        let verifier = DilithiumVerifier;
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(matches!(result, Err(TxValidationError::InsufficientBalance { .. })));
    }

    #[test]
    fn validate_overflow_gas_cost_does_not_panic() {
        let signer = make_signer();
        let (mut ws, cs) = setup_stores();
        let from = Address::from_public_key(signer.public_key());
        fund_account(&mut ws, &from, U256::MAX);

        // Craft a tx where gas_limit * max_fee_per_gas + value overflows U256
        let tx = Transaction {
            chain_id: test_chain_id(),
            nonce: 0,
            to: Some(Address::from([0x01; 20])),
            value: U256::MAX, // near-max value
            data: Bytes::new(),
            gas_limit: u64::MAX,
            max_fee_per_gas: u64::MAX,
            max_priority_fee_per_gas: 0,
        };
        let signed = sign_tx(&signer, tx, true);

        let verifier = DilithiumVerifier;
        // Must not panic — should return InsufficientBalance with needed = U256::MAX
        let result = validate_tx(&signed, &ws, &cs, &verifier, test_chain_id());
        assert!(
            matches!(result, Err(TxValidationError::InsufficientBalance { .. })),
            "overflow should be caught, got: {:?}",
            result
        );
    }
}
