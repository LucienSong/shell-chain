use shell_primitives::{Address, ShellHash, U256};
use thiserror::Error;

/// Errors that can occur during mempool operations.
#[derive(Debug, Error)]
pub enum MempoolError {
    #[error("pool is full ({capacity} transactions)")]
    PoolFull { capacity: usize },

    #[error("sender {sender} has too many pending transactions ({count})")]
    SenderQueueFull { sender: Address, count: usize },

    #[error("duplicate transaction {hash}")]
    Duplicate { hash: ShellHash },

    #[error("chain ID mismatch: expected {expected}, got {got}")]
    ChainIdMismatch { expected: u64, got: u64 },

    #[error("gas price {got} below minimum {min}")]
    GasPriceTooLow { got: u64, min: u64 },

    #[error("nonce {got} too low, sender has pending nonce >= {pending}")]
    NonceTooLow { got: u64, pending: u64 },

    #[error("insufficient balance: need {needed}, have {have}")]
    InsufficientBalance { needed: U256, have: U256 },

    #[error("replacement fee too low: need >{required}, got {got}")]
    ReplacementFeeTooLow { got: u64, required: u64 },

    #[error("invalid signature: {0}")]
    InvalidSignature(String),

    #[error("pubkey required for first transaction from {sender}")]
    PubkeyRequired { sender: Address },

    #[error("address mismatch: from={from}, derived={derived}")]
    AddressMismatch { from: Address, derived: Address },

    #[error("crypto error: {0}")]
    Crypto(#[from] shell_crypto::CryptoError),

    #[error("invalid transaction: {0}")]
    InvalidTransaction(String),
}
