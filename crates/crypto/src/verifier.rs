use crate::{CryptoError, PQSignature, SignatureType};

/// Verification trait: stateless, used at every node for every transaction.
///
/// Takes `&self` (rather than being a pure associated function) so that
/// `dyn Verifier` works for Account Abstraction runtime dispatch.
/// Implementations like `DilithiumVerifier` are zero-sized types — `&self`
/// has no overhead.
pub trait Verifier: Send + Sync {
    /// Verify a signature against a public key and message.
    fn verify(
        &self,
        pubkey: &[u8],
        message: &[u8],
        signature: &PQSignature,
    ) -> Result<bool, CryptoError>;

    /// Which signature algorithm this verifier handles.
    fn sig_type(&self) -> SignatureType;
}
