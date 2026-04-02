//! Keystore types and JSON format.

use serde::{Deserialize, Serialize};

/// Errors returned by keystore operations.
#[derive(Debug, thiserror::Error)]
pub enum KeystoreError {
    #[error("encryption failed: {0}")]
    Encryption(String),

    #[error("decryption failed (wrong password or corrupted data)")]
    Decryption,

    #[error("invalid key material: {0}")]
    InvalidKey(String),

    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("crypto: {0}")]
    Crypto(#[from] shell_crypto::CryptoError),
}

/// argon2id key derivation parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory cost in KiB.
    pub m_cost: u32,
    /// Time cost (iterations).
    pub t_cost: u32,
    /// Parallelism degree.
    pub p_cost: u32,
    /// Salt (hex-encoded).
    pub salt: String,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            m_cost: 65536,  // 64 MiB
            t_cost: 3,
            p_cost: 4,
            salt: String::new(),
        }
    }
}

/// XChaCha20-Poly1305 cipher parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CipherParams {
    /// Nonce (hex-encoded, 24 bytes).
    pub nonce: String,
}

/// Encrypted private key in JSON-serializable format.
///
/// Compatible with a PQ-adapted variant of the Web3 Secret Storage
/// definition. The `address` field uses the same keccak256(pubkey)[12:]
/// derivation as Ethereum but from a Dilithium3 public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedKey {
    /// Format version (always 1).
    pub version: u32,
    /// Shell-chain address derived from the public key.
    pub address: String,
    /// KDF algorithm identifier.
    pub kdf: String,
    /// KDF parameters.
    pub kdf_params: KdfParams,
    /// AEAD cipher identifier.
    pub cipher: String,
    /// Cipher parameters (nonce).
    pub cipher_params: CipherParams,
    /// Encrypted secret key (hex-encoded).
    pub ciphertext: String,
    /// Public key (hex-encoded) for address verification on decrypt.
    pub public_key: String,
}
