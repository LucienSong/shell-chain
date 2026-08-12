//! Canonical STARK source-entry reconstruction.
//!
//! A single authoritative helper that converts a [`SignedTransaction`] into
//! the corresponding [`SigBatchEntry`] used by the prover and validators.
//! Every site that builds `SigBatchEntry` values – block production, block
//! import, frontier back-log seeding, and amendment validation – **must**
//! call these helpers so the entries are deterministic and identical
//! everywhere.
//!
//! # Mapping rules
//!
//! * `msg_hash` – the 32-byte sender signing hash.
//! * `pk_hash`  – for [`PubkeyMode::Embedded`], the first ≤32 bytes of the
//!   inline public key; for [`PubkeyMode::Reference`], the 20-
//!   byte sender address (32 bytes in v0.23.0+) zero-padded to 32 bytes.

use shell_core::PubkeyMode;

use super::{Block, SigBatchEntry, SignedTransaction};

/// Derive a [`SigBatchEntry`] from a single signed transaction.
///
/// This is the canonical definition of the tx → entry mapping.  All other
/// entry-building code must call this function instead of reimplementing
/// the mapping independently.
pub(crate) fn tx_to_sig_batch_entry(tx: &SignedTransaction) -> SigBatchEntry {
    let mut msg_hash = [0u8; 32];
    msg_hash.copy_from_slice(tx.sender_signing_hash().as_bytes());

    let pk_hash = match &tx.pubkey_mode {
        PubkeyMode::Embedded(pk) => {
            let mut h = [0u8; 32];
            let copy_len = pk.len().min(32);
            h[..copy_len].copy_from_slice(&pk[..copy_len]);
            h
        }
        PubkeyMode::Reference => {
            // Use sender address bytes as pk identifier for Reference-mode txs.
            // Addresses are 32 bytes in v0.23.0 (BLAKE3, not 20-byte Ethereum).
            let mut h = [0u8; 32];
            let addr = tx.from.0.as_slice();
            let copy_len = addr.len().min(32);
            h[..copy_len].copy_from_slice(&addr[..copy_len]);
            h
        }
    };

    SigBatchEntry { msg_hash, pk_hash }
}

/// Build the ordered [`SigBatchEntry`] list from a slice of transactions.
///
/// Returns an empty `Vec` for 0-tx inputs.  The caller is responsible for
/// deciding whether to dispatch an empty-entry task to the prover; empty
/// tasks must **not** be sent to `prove_sig_batch` – see the backlog and
/// `ProverService` for that guard.
pub(crate) fn entries_from_txs(txs: &[SignedTransaction]) -> Vec<SigBatchEntry> {
    txs.iter().map(tx_to_sig_batch_entry).collect()
}

/// Build the ordered [`SigBatchEntry`] list for all user transactions in a
/// block.
///
/// Equivalent to [`entries_from_txs`]`(&block.transactions)`.
pub(crate) fn block_to_sig_batch_entries(block: &Block) -> Vec<SigBatchEntry> {
    entries_from_txs(&block.transactions)
}

#[cfg(test)]
mod tests {
    use shell_core::Transaction;
    use shell_crypto::{PQSignature, SignatureType};
    use shell_primitives::{Address, Bytes, U256};

    use super::*;

    #[test]
    fn signature_entry_uses_sender_signing_hash_not_transaction_id() {
        let tx = Transaction {
            chain_id: 10,
            nonce: 1,
            to: Some(Address::from([0x11; 20])),
            value: U256::from(2u64),
            data: Bytes::new(),
            gas_limit: 21_000,
            max_fee_per_gas: 1,
            max_priority_fee_per_gas: 0,
            tx_type: 0,
            access_list: None,
            max_fee_per_blob_gas: None,
            blob_versioned_hashes: None,
        };
        let signed = SignedTransaction::new(
            Address::from([0x22; 20]),
            tx,
            PQSignature::new(SignatureType::Dilithium3, vec![0xAA; 32]),
        );

        let entry = tx_to_sig_batch_entry(&signed);
        assert_eq!(entry.msg_hash, *signed.sender_signing_hash().as_bytes());
        assert_ne!(entry.msg_hash, *signed.hash().as_bytes());
    }
}
