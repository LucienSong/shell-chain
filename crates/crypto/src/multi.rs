use crate::{
    CryptoError, DilithiumVerifier, PQSignature, SignatureType, SphincsVerifier, Verifier,
};

/// Multi-algorithm verifier that dispatches to the correct backend
/// based on the [`SignatureType`] embedded in each [`PQSignature`].
///
/// Zero-sized type — both inner verifiers are ZSTs, so `MultiVerifier`
/// itself has no runtime cost.
#[derive(Debug, Clone, Copy, Default)]
pub struct MultiVerifier;

impl Verifier for MultiVerifier {
    fn verify(
        &self,
        pubkey: &[u8],
        message: &[u8],
        signature: &PQSignature,
    ) -> Result<bool, CryptoError> {
        match signature.sig_type {
            SignatureType::Dilithium3 => DilithiumVerifier.verify(pubkey, message, signature),
            SignatureType::SphincsSha2256f => SphincsVerifier.verify(pubkey, message, signature),
            other => Err(CryptoError::UnsupportedSignatureType(other)),
        }
    }

    /// `MultiVerifier` handles all supported algorithms; returns
    /// `Dilithium3` as the canonical default for the trait method.
    /// Use [`MultiVerifier::detect_algorithm`] to inspect a specific
    /// signature's algorithm tag.
    fn sig_type(&self) -> SignatureType {
        SignatureType::Dilithium3
    }
}

impl MultiVerifier {
    /// Detect the algorithm used by a given signature by reading its
    /// embedded `sig_type` tag byte.
    ///
    /// This is the correct way to determine which PQ algorithm was used
    /// for a specific signature, rather than relying on `Verifier::sig_type()`
    /// which returns a static default.
    pub fn detect_algorithm(signature: &PQSignature) -> SignatureType {
        signature.sig_type
    }
}

#[cfg(feature = "batch")]
impl crate::BatchVerifier for MultiVerifier {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DilithiumSigner, Signer, SphincsSigner};

    #[test]
    fn multi_verifies_dilithium() {
        let signer = DilithiumSigner::generate();
        let sig = signer.sign(b"multi-dil").unwrap();
        let mv = MultiVerifier;
        assert!(mv.verify(signer.public_key(), b"multi-dil", &sig).unwrap());
    }

    #[test]
    fn multi_verifies_sphincs() {
        let signer = SphincsSigner::generate();
        let sig = signer.sign(b"multi-sph").unwrap();
        let mv = MultiVerifier;
        assert!(mv.verify(signer.public_key(), b"multi-sph", &sig).unwrap());
    }

    #[test]
    fn multi_rejects_unknown_algorithm() {
        let sig = PQSignature::new(SignatureType::MlDsa65, vec![0u8; 64]);
        let mv = MultiVerifier;
        let result = mv.verify(&[0u8; 32], b"test", &sig);
        assert!(result.is_err());
    }

    #[test]
    fn multi_rejects_wrong_message() {
        let signer = DilithiumSigner::generate();
        let sig = signer.sign(b"correct").unwrap();
        let mv = MultiVerifier;
        let valid = mv.verify(signer.public_key(), b"wrong", &sig).unwrap();
        assert!(!valid);
    }

    #[test]
    fn multi_rejects_wrong_key() {
        let signer1 = DilithiumSigner::generate();
        let signer2 = DilithiumSigner::generate();
        let sig = signer1.sign(b"test").unwrap();
        let mv = MultiVerifier;
        let valid = mv.verify(signer2.public_key(), b"test", &sig).unwrap();
        assert!(!valid);
    }

    #[test]
    fn multi_mixed_validator_set() {
        let dil_signer = DilithiumSigner::generate();
        let sph_signer = SphincsSigner::generate();
        let mv = MultiVerifier;

        let msg = b"block-42";
        let dil_sig = dil_signer.sign(msg).unwrap();
        let sph_sig = sph_signer.sign(msg).unwrap();

        assert!(mv.verify(dil_signer.public_key(), msg, &dil_sig).unwrap());
        assert!(mv.verify(sph_signer.public_key(), msg, &sph_sig).unwrap());

        // Cross-key must fail.
        assert!(
            !mv.verify(sph_signer.public_key(), msg, &dil_sig).is_ok()
                || !mv.verify(sph_signer.public_key(), msg, &dil_sig).unwrap()
        );
    }

    #[test]
    fn multi_verifier_is_zero_sized() {
        assert_eq!(std::mem::size_of::<MultiVerifier>(), 0);
    }

    #[test]
    fn detect_algorithm_dilithium() {
        let signer = DilithiumSigner::generate();
        let sig = signer.sign(b"detect-dil").unwrap();
        assert_eq!(
            MultiVerifier::detect_algorithm(&sig),
            SignatureType::Dilithium3
        );
    }

    #[test]
    fn detect_algorithm_sphincs() {
        let signer = SphincsSigner::generate();
        let sig = signer.sign(b"detect-sph").unwrap();
        assert_eq!(
            MultiVerifier::detect_algorithm(&sig),
            SignatureType::SphincsSha2256f
        );
    }
}
