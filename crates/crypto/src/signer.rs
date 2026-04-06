use crate::{CryptoError, PQSignature, SignatureType};

/// Signing trait: holds a private key, used at the wallet / client side.
pub trait Signer: Send + Sync {
    /// Sign an arbitrary message.
    fn sign(&self, message: &[u8]) -> Result<PQSignature, CryptoError>;

    /// Return the raw public key bytes.
    fn public_key(&self) -> &[u8];

    /// Which signature algorithm this signer uses.
    fn sig_type(&self) -> SignatureType;
}
