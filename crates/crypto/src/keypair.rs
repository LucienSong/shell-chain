use shell_primitives::Address;
use crate::SignatureType;

/// A generic key pair container (public key + address).
///
/// The private key is held by the concrete `Signer` implementation and
/// is zeroized on drop. This struct only stores the public portion.
#[derive(Debug, Clone)]
pub struct KeyPair {
    pub public_key: Vec<u8>,
    pub address: Address,
    pub sig_type: SignatureType,
}

impl KeyPair {
    pub fn new(public_key: Vec<u8>, sig_type: SignatureType) -> Self {
        let address = Address::from_public_key(&public_key);
        Self {
            public_key,
            address,
            sig_type,
        }
    }
}
