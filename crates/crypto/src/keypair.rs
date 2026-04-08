use crate::SignatureType;
use shell_primitives::Address;

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
        let address = Address::from_public_key(&public_key, sig_type.as_u8());
        Self {
            public_key,
            address,
            sig_type,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DilithiumSigner, Signer, SphincsSigner};

    #[test]
    fn new_derives_address_from_pubkey() {
        let pubkey = vec![1u8; 32];
        let kp = KeyPair::new(pubkey.clone(), SignatureType::Dilithium3);

        assert_eq!(kp.public_key, pubkey);
        assert_eq!(kp.sig_type, SignatureType::Dilithium3);
        assert_eq!(
            kp.address,
            Address::from_public_key(&pubkey, SignatureType::Dilithium3.as_u8())
        );
    }

    #[test]
    fn address_deterministic_for_same_pubkey() {
        let pubkey = vec![42u8; 64];
        let kp1 = KeyPair::new(pubkey.clone(), SignatureType::Dilithium3);
        let kp2 = KeyPair::new(pubkey, SignatureType::Dilithium3);
        assert_eq!(kp1.address, kp2.address);
    }

    #[test]
    fn different_pubkeys_yield_different_addresses() {
        let kp1 = KeyPair::new(vec![1u8; 32], SignatureType::Dilithium3);
        let kp2 = KeyPair::new(vec![2u8; 32], SignatureType::Dilithium3);
        assert_ne!(kp1.address, kp2.address);
    }

    #[test]
    fn sig_type_preserved() {
        let kp_dil = KeyPair::new(vec![0u8; 16], SignatureType::Dilithium3);
        assert_eq!(kp_dil.sig_type, SignatureType::Dilithium3);

        let kp_sph = KeyPair::new(vec![0u8; 16], SignatureType::SphincsSha2256f);
        assert_eq!(kp_sph.sig_type, SignatureType::SphincsSha2256f);
    }

    #[test]
    fn address_matches_dilithium_signer() {
        let signer = DilithiumSigner::generate();
        let kp = KeyPair::new(signer.public_key().to_vec(), signer.sig_type());

        let expected = Address::from_public_key(signer.public_key(), signer.sig_type().as_u8());
        assert_eq!(kp.address, expected);
    }

    #[test]
    fn address_matches_sphincs_signer() {
        let signer = SphincsSigner::generate();
        let kp = KeyPair::new(signer.public_key().to_vec(), signer.sig_type());

        let expected = Address::from_public_key(signer.public_key(), signer.sig_type().as_u8());
        assert_eq!(kp.address, expected);
    }

    #[test]
    fn clone_produces_equal_copy() {
        let kp = KeyPair::new(vec![7u8; 48], SignatureType::Dilithium3);
        let cloned = kp.clone();
        assert_eq!(kp.public_key, cloned.public_key);
        assert_eq!(kp.address, cloned.address);
        assert_eq!(kp.sig_type, cloned.sig_type);
    }

    #[test]
    fn empty_pubkey_does_not_panic() {
        let kp = KeyPair::new(vec![], SignatureType::Dilithium3);
        assert_eq!(kp.public_key, Vec::<u8>::new());
        // Address is still derived (from empty hash)
        assert_eq!(
            kp.address,
            Address::from_public_key(&[], SignatureType::Dilithium3.as_u8())
        );
    }

    #[test]
    fn debug_format() {
        let kp = KeyPair::new(vec![0xAB; 4], SignatureType::Dilithium3);
        let debug = format!("{:?}", kp);
        assert!(debug.contains("KeyPair"));
    }
}
