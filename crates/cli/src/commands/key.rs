//! `shell-node key` — key generation and inspection.

use std::path::PathBuf;

use shell_crypto::{DilithiumSigner, MlDsaSigner, Signer};
use shell_keystore::{decrypt, decrypt_mldsa, encrypt, encrypt_mldsa, EncryptedKey};
use shell_primitives::Address;

use tracing::info;

use crate::password::{resolve_new_password, resolve_password, PasswordArgs};

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


/// Migrate a keystore to the current v1 sk-only format.
///
/// Useful if you have keystores produced by `shell-sdk < 0.6.0` where the ciphertext
/// stored both sk and pk (sk‖pk). The migration decrypts and re-encrypts using the
/// standard v1 format (sk-only ciphertext).
pub fn key_migrate(
    input: PathBuf,
    output: PathBuf,
    password_args: &PasswordArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let password = resolve_password("Enter keystore password", password_args)?;

    eprintln!("Reading keystore: {}", input.display());
    let json = std::fs::read_to_string(&input)?;
    let encrypted: EncryptedKey = serde_json::from_str(&json)?;
    let key_type = encrypted.key_type.clone();

    info!("Decrypting keystore (algorithm: {key_type})...");

    // Re-encrypt using the canonical v1 sk-only format (same password).
    info!("Re-encrypting in v1 sk-only format...");
    let new_encrypted = match key_type.as_str() {
        "mldsa65" => {
            let signer = decrypt_mldsa(&encrypted, password.as_bytes())
                .map_err(|e| format!("decryption failed: {e}"))?;
            encrypt_mldsa(&signer, password.as_bytes())?
        }
        _ => {
            let signer = decrypt(&encrypted, password.as_bytes())
                .map_err(|e| format!("decryption failed: {e}"))?;
            encrypt(&signer, password.as_bytes())?
        }
    };

    let new_json = serde_json::to_string_pretty(&new_encrypted)?;
    std::fs::write(&output, &new_json)?;

    let address = Address::parse(&new_encrypted.address)
        .map_err(|e| format!("invalid address in re-encrypted keystore: {e}"))?;
    eprintln!("✓ Migrated keystore written to {}", output.display());
    eprintln!("  Address:   {address}");
    eprintln!("  Algorithm: {}", new_encrypted.key_type);
    eprintln!("  Version:   {}", new_encrypted.version);

    Ok(())
}
