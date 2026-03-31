use serde::{Deserialize, Serialize};
use core::fmt;

use crate::ShellHash;

/// 20-byte address, identical layout to Ethereum addresses.
///
/// Derived from PQ public keys via `keccak256(pubkey)[12..]`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Address(pub alloy_primitives::Address);

impl Address {
    pub const ZERO: Self = Self(alloy_primitives::Address::ZERO);

    pub fn from_slice(slice: &[u8]) -> Self {
        Self(alloy_primitives::Address::from_slice(slice))
    }

    /// Derive an address from a raw public key: `keccak256(pubkey)[12..]`
    pub fn from_public_key(pubkey: &[u8]) -> Self {
        let hash = crate::keccak256(pubkey);
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&hash.as_bytes()[12..]);
        Self(alloy_primitives::Address::from(addr))
    }

    pub fn as_bytes(&self) -> &[u8; 20] {
        self.0.as_ref()
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address(0x{})", hex::encode(self.0))
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl From<[u8; 20]> for Address {
    fn from(arr: [u8; 20]) -> Self {
        Self(alloy_primitives::Address::from(arr))
    }
}

impl From<alloy_primitives::Address> for Address {
    fn from(a: alloy_primitives::Address) -> Self {
        Self(a)
    }
}

impl From<Address> for alloy_primitives::Address {
    fn from(a: Address) -> Self {
        a.0
    }
}

impl AsRef<[u8]> for Address {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl From<ShellHash> for Address {
    fn from(hash: ShellHash) -> Self {
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&hash.as_bytes()[12..]);
        Self(alloy_primitives::Address::from(addr))
    }
}

impl alloy_rlp::Encodable for Address {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let bytes: [u8; 20] = self.0.into_array();
        bytes.as_slice().encode(out);
    }

    fn length(&self) -> usize {
        let bytes: [u8; 20] = self.0.into_array();
        alloy_rlp::Encodable::length(&bytes.as_slice())
    }
}

impl alloy_rlp::Decodable for Address {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let bytes = <[u8; 20]>::decode(buf)?;
        Ok(Self(alloy_primitives::Address::from(bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_from_public_key() {
        let fake_pubkey = [0xABu8; 64];
        let addr = Address::from_public_key(&fake_pubkey);
        assert_eq!(addr.as_bytes().len(), 20);
        // Deterministic
        assert_eq!(addr, Address::from_public_key(&fake_pubkey));
    }

    #[test]
    fn address_display() {
        let addr = Address::from([0x01; 20]);
        assert_eq!(addr.to_string(), format!("0x{}", "01".repeat(20)));
    }

    #[test]
    fn address_from_hash() {
        let hash = crate::keccak256(b"some-pubkey-data");
        let addr = Address::from(hash);
        assert_eq!(addr.as_bytes(), &hash.as_bytes()[12..]);
    }

    #[test]
    fn address_serde_roundtrip() {
        let addr = Address::from([0xDE; 20]);
        let json = serde_json::to_string(&addr).unwrap();
        let addr2: Address = serde_json::from_str(&json).unwrap();
        assert_eq!(addr, addr2);
    }

    #[test]
    fn address_rlp_roundtrip() {
        use alloy_rlp::{Decodable, Encodable};
        let addr = Address::from([0x42; 20]);
        let mut buf = Vec::new();
        addr.encode(&mut buf);
        let addr2 = Address::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(addr, addr2);
    }
}
