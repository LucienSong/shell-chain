//! Generate ML-DSA-65 cross-language test vectors.
//!
//! Outputs a JSON fixture file that the SDK test suite can consume to verify
//! that Rust (fips204 crate) and TypeScript (@noble/post-quantum) produce
//! byte-identical results when given the same deterministic inputs.
//!
//! Usage:
//!   cargo run -p shell-crypto --bin gen_mldsa_vectors -- <output_path>
//!
//! The generated fixture is committed under:
//!   shell-chain/crates/crypto/tests/fixtures/mldsa65-cross-lang.json
//!   shell-sdk/tests/fixtures/mldsa65-cross-lang.json  (symlink or copy)

use std::path::PathBuf;

use fips204::ml_dsa_65;
use fips204::traits::{KeyGen, SerDes, Signer as FipsSigner, Verifier as FipsVerifier};
use serde_json::{json, Value};

fn bytes_to_hex(b: &[u8]) -> String {
    b.iter().map(|byte| format!("{:02x}", byte)).collect()
}

fn main() {
    // ── Deterministic key generation ────────────────────────────────────────
    // ξ (xi) seed: all 0x42 bytes.  Both Rust (keygen_from_seed) and Noble
    // (ml_dsa65.keygen) accept a 32-byte ξ seed directly per FIPS 204 §5.1.
    let keygen_seed = [0x42u8; 32];
    let (pk, sk) = ml_dsa_65::KG::keygen_from_seed(&keygen_seed);

    let pk_bytes = pk.into_bytes();
    let sk_bytes = sk.into_bytes();

    // Reconstruct from bytes for signing (arrays are non-Copy)
    let sk_for_sign =
        ml_dsa_65::PrivateKey::try_from_bytes(sk_bytes).expect("valid private key bytes");
    let pk_for_verify =
        ml_dsa_65::PublicKey::try_from_bytes(pk_bytes).expect("valid public key bytes");

    // ── Deterministic signing ────────────────────────────────────────────────
    // sign_seed = all-zero bytes → this is the `rnd` (ρ′ input) per FIPS 204 §6.2.
    // Noble: { extraEntropy: false } passes the same rnd = [0; 32].
    let sign_seed = [0x00u8; 32];
    let message = b"shell cross-lang test";

    let sig = sk_for_sign
        .try_sign_with_seed(&sign_seed, message, &[])
        .expect("signing should not fail");

    // Self-verify — assert fixture is valid before writing
    assert!(
        pk_for_verify.verify(message, &sig, &[]),
        "Rust self-verify failed — fixture would be invalid"
    );

    // ── Build JSON fixture ───────────────────────────────────────────────────
    let fixture: Value = json!({
        "description": "ML-DSA-65 (FIPS 204) cross-language sign/verify vectors",
        "algorithm": "ML-DSA-65",
        "standard": "FIPS 204",
        "keygen_seed_hex": bytes_to_hex(&keygen_seed),
        "public_key_hex": bytes_to_hex(&pk_bytes),
        "secret_key_hex": bytes_to_hex(&sk_bytes),
        "message_hex": bytes_to_hex(message),
        "message_utf8": String::from_utf8_lossy(message).as_ref(),
        "rust_vector": {
            "description": "Signed by fips204 crate with sign_seed=[0x00;32] (rnd), ctx=[]",
            "sign_seed_hex": bytes_to_hex(&sign_seed),
            "context_hex": "",
            "signature_hex": bytes_to_hex(&sig),
            "signature_len": sig.len(),
        },
        "key_sizes": {
            "public_key_len": pk_bytes.len(),
            "secret_key_len": sk_bytes.len(),
            "signature_len": sig.len(),
        }
    });

    let json_str = serde_json::to_string_pretty(&fixture).expect("JSON serialization failed");

    // ── Write output ─────────────────────────────────────────────────────────
    let output_path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tests/fixtures/mldsa65-cross-lang.json"));

    std::fs::create_dir_all(output_path.parent().unwrap_or(std::path::Path::new(".")))
        .expect("create output dir");
    std::fs::write(&output_path, &json_str).expect("write fixture file");

    println!("Written: {}", output_path.display());
    println!(
        "  pk_len={} sk_len={} sig_len={}",
        pk_bytes.len(),
        sk_bytes.len(),
        sig.len()
    );
    println!("  keygen_seed: {}", bytes_to_hex(&keygen_seed));
    println!("  pk[0..32]:   {}...", bytes_to_hex(&pk_bytes[..32]));
    println!("  sig[0..32]:  {}...", bytes_to_hex(&sig[..32]));
}
