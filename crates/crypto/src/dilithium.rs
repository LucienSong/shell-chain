use pqcrypto_dilithium::dilithium3;
use pqcrypto_traits::sign::{
    DetachedSignature, PublicKey, SecretKey,
};

use crate::{
    CryptoError, KeyPair, PQSignature, SignatureType, Signer, Verifier,
};

// ── Signer ───────────────────────────────────────────────────

/// CRYSTALS-Dilithium3 signer (NIST Level 3, 128-bit PQ security).
///
/// Stores key material as raw bytes wrapped in `Zeroizing` to ensure
/// secret key is zeroed on drop, even though pqcrypto's SecretKey type
/// does not implement Zeroize.
pub struct DilithiumSigner {
    secret_key_bytes: zeroize::Zeroizing<Vec<u8>>,
    public_key_bytes: Vec<u8>,
}

impl DilithiumSigner {
    /// Generate a fresh Dilithium3 key pair.
    ///
    /// Uses `pqcrypto-dilithium`'s internal CSPRNG (`randombytes` / system RNG).
    /// See: <https://github.com/pqcrypto/pqcrypto/>
    pub fn generate() -> Self {
        let (pk, sk) = dilithium3::keypair();
        Self {
            secret_key_bytes: zeroize::Zeroizing::new(sk.as_bytes().to_vec()),
            public_key_bytes: pk.as_bytes().to_vec(),
        }
    }

    /// Reconstruct from raw key bytes.
    pub fn from_bytes(
        public_key: &[u8],
        secret_key: &[u8],
    ) -> Result<Self, CryptoError> {
        // Validate by attempting to parse
        dilithium3::PublicKey::from_bytes(public_key).map_err(|_| {
            CryptoError::InvalidPublicKeyLength {
                expected: dilithium3::public_key_bytes(),
                got: public_key.len(),
            }
        })?;
        dilithium3::SecretKey::from_bytes(secret_key).map_err(|_| {
            CryptoError::InvalidSecretKeyLength {
                expected: dilithium3::secret_key_bytes(),
                got: secret_key.len(),
            }
        })?;
        Ok(Self {
            secret_key_bytes: zeroize::Zeroizing::new(secret_key.to_vec()),
            public_key_bytes: public_key.to_vec(),
        })
    }

    /// Export the public half as a [`KeyPair`].
    pub fn key_pair(&self) -> KeyPair {
        KeyPair::new(
            self.public_key_bytes.clone(),
            SignatureType::Dilithium3,
        )
    }

    fn secret_key(&self) -> dilithium3::SecretKey {
        // Safe: bytes were validated at construction time
        dilithium3::SecretKey::from_bytes(&self.secret_key_bytes)
            .expect("secret key bytes validated at construction")
    }
}

impl Signer for DilithiumSigner {
    fn sign(&self, message: &[u8]) -> Result<PQSignature, CryptoError> {
        let sk = self.secret_key();
        let sig = dilithium3::detached_sign(message, &sk);
        Ok(PQSignature::new(
            SignatureType::Dilithium3,
            sig.as_bytes().to_vec(),
        ))
    }

    fn public_key(&self) -> &[u8] {
        &self.public_key_bytes
    }

    fn sig_type(&self) -> SignatureType {
        SignatureType::Dilithium3
    }
}

// ── Verifier ─────────────────────────────────────────────────

/// Stateless Dilithium3 verifier (zero-sized type).
#[derive(Debug, Clone, Copy, Default)]
pub struct DilithiumVerifier;

impl Verifier for DilithiumVerifier {
    fn verify(
        &self,
        pubkey: &[u8],
        message: &[u8],
        signature: &PQSignature,
    ) -> Result<bool, CryptoError> {
        if signature.sig_type != SignatureType::Dilithium3 {
            return Err(CryptoError::UnsupportedSignatureType(signature.sig_type));
        }

        let pk = dilithium3::PublicKey::from_bytes(pubkey).map_err(|_| {
            CryptoError::InvalidPublicKeyLength {
                expected: dilithium3::public_key_bytes(),
                got: pubkey.len(),
            }
        })?;

        let sig = dilithium3::DetachedSignature::from_bytes(&signature.data)
            .map_err(|_| CryptoError::InvalidSignatureLength {
                expected: dilithium3::signature_bytes(),
                got: signature.data.len(),
            })?;

        let valid = dilithium3::verify_detached_signature(&sig, message, &pk).is_ok();
        Ok(valid)
    }

    fn sig_type(&self) -> SignatureType {
        SignatureType::Dilithium3
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_primitives::Address;

    #[test]
    fn generate_and_sign_verify() {
        let signer = DilithiumSigner::generate();
        let message = b"hello shell-chain";

        let sig = signer.sign(message).unwrap();
        assert_eq!(sig.sig_type, SignatureType::Dilithium3);
        assert!(!sig.is_empty());

        let verifier = DilithiumVerifier;
        let valid = verifier
            .verify(signer.public_key(), message, &sig)
            .unwrap();
        assert!(valid);
    }

    #[test]
    fn verify_wrong_message_fails() {
        let signer = DilithiumSigner::generate();
        let sig = signer.sign(b"correct message").unwrap();

        let verifier = DilithiumVerifier;
        let valid = verifier
            .verify(signer.public_key(), b"wrong message", &sig)
            .unwrap();
        assert!(!valid);
    }

    #[test]
    fn verify_wrong_key_fails() {
        let signer1 = DilithiumSigner::generate();
        let signer2 = DilithiumSigner::generate();
        let sig = signer1.sign(b"test").unwrap();

        let verifier = DilithiumVerifier;
        let valid = verifier
            .verify(signer2.public_key(), b"test", &sig)
            .unwrap();
        assert!(!valid);
    }

    #[test]
    fn address_derivation() {
        let signer = DilithiumSigner::generate();
        let kp = signer.key_pair();
        assert_eq!(kp.address.as_bytes().len(), 20);
        // Deterministic: same pubkey → same address
        let addr2 = Address::from_public_key(signer.public_key());
        assert_eq!(kp.address, addr2);
    }

    #[test]
    fn from_bytes_roundtrip() {
        let signer = DilithiumSigner::generate();
        let pk = signer.public_key().to_vec();
        let sk = signer.secret_key_bytes.to_vec();

        let signer2 = DilithiumSigner::from_bytes(&pk, &sk).unwrap();
        assert_eq!(signer.public_key(), signer2.public_key());

        // Sign with reconstructed signer, verify with original pubkey
        let sig = signer2.sign(b"roundtrip").unwrap();
        let verifier = DilithiumVerifier;
        assert!(verifier.verify(&pk, b"roundtrip", &sig).unwrap());
    }

    #[test]
    fn signature_serde_roundtrip() {
        let signer = DilithiumSigner::generate();
        let sig = signer.sign(b"serde test").unwrap();

        let json = serde_json::to_string(&sig).unwrap();
        let sig2: PQSignature = serde_json::from_str(&json).unwrap();
        assert_eq!(sig, sig2);
    }

    #[test]
    fn invalid_pubkey_length() {
        let verifier = DilithiumVerifier;
        let bad_sig = PQSignature::new(SignatureType::Dilithium3, vec![0u8; 100]);
        let result = verifier.verify(&[0u8; 10], b"test", &bad_sig);
        assert!(result.is_err());
    }

    #[test]
    fn dilithium_verifier_is_zero_sized() {
        assert_eq!(std::mem::size_of::<DilithiumVerifier>(), 0);
    }
}
