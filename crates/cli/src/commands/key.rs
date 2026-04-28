//! `shell-node key` — key generation and inspection.

use std::path::PathBuf;

use shell_crypto::{DilithiumSigner, MlDsaSigner, Signer};
use shell_keystore::{encrypt, encrypt_mldsa, EncryptedKey};
use shell_primitives::Address;

use tracing::info;

use crate::password::{resolve_new_password, PasswordArgs};

/// Generate a new keypair and encrypt it to a keystore file.
///
/// `algorithm` selects the PQ algorithm: `"dilithium3"` (default) or `"mldsa65"`.
pub fn key_generate(
    output: PathBuf,
    password_args: PasswordArgs,
    algorithm: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let password = resolve_new_password(&password_args)?;

    let (encrypted, pubkey_hex, address) = match algorithm.as_str() {
        "mldsa65" => {
            info!("Generating ML-DSA-65 (FIPS 204) keypair...");
            let signer = MlDsaSigner::generate();
            let address = Address::from_public_key(signer.public_key(), signer.sig_type().as_u8());
            info!("Encrypting with argon2id + XChaCha20-Poly1305...");
            let encrypted = encrypt_mldsa(&signer, password.as_bytes())?;
            let pubkey_hex = hex::encode(signer.public_key());
            (encrypted, pubkey_hex, address)
        }
        "dilithium3" | "" => {
            info!("Generating Dilithium3 keypair...");
            let signer = DilithiumSigner::generate();
            let address = Address::from_public_key(signer.public_key(), signer.sig_type().as_u8());
            info!("Encrypting with argon2id + XChaCha20-Poly1305...");
            let encrypted = encrypt(&signer, password.as_bytes())?;
            let pubkey_hex = hex::encode(signer.public_key());
            (encrypted, pubkey_hex, address)
        }
        other => {
            return Err(format!("unsupported algorithm: {other}; valid: dilithium3, mldsa65").into());
        }
    };

    let json = serde_json::to_string_pretty(&encrypted)?;
    std::fs::write(&output, &json)?;

    eprintln!("✓ Keystore written to {}", output.display());
    eprintln!("  Address:   {address}");
    eprintln!("  Algorithm: {}", encrypted.key_type);
    eprintln!(
        "  Public key: 0x{}...{}",
        &pubkey_hex[..16],
        &pubkey_hex[pubkey_hex.len() - 16..]
    );

    Ok(())
}

/// Display the address and public key from an existing keystore file.
pub fn key_inspect(path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let json = std::fs::read_to_string(&path)?;
    let encrypted: EncryptedKey = serde_json::from_str(&json)?;
    let address = Address::parse(&encrypted.address)
        .map_err(|e| format!("invalid keystore address '{}': {e}", encrypted.address))?;

    eprintln!("Keystore: {}", path.display());
    eprintln!("  Version:    {}", encrypted.version);
    eprintln!("  Algorithm:  {}", encrypted.key_type);
    eprintln!("  Address:    {address}");
    eprintln!("  KDF:        {}", encrypted.kdf);
    eprintln!("  Cipher:     {}", encrypted.cipher);
    eprintln!(
        "  Public key: 0x{}...{}",
        &encrypted.public_key[..16],
        &encrypted.public_key[encrypted.public_key.len() - 16..]
    );

    Ok(())
}

