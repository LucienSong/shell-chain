use alloy_primitives::B256;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use core::fmt;

/// 32-byte hash used throughout Shell-Chain.
///
/// Wraps [`B256`] for Ethereum-compatible semantics while keeping the door
/// open for additional Shell-specific behaviour.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShellHash(pub B256);

impl ShellHash {
    pub const ZERO: Self = Self(B256::ZERO);

    pub fn from_slice(slice: &[u8]) -> Self {
        Self(B256::from_slice(slice))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_ref()
    }
}

impl fmt::Debug for ShellHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ShellHash(0x{})", hex::encode(self.0))
    }
}

impl fmt::Display for ShellHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(self.0))
    }
}

impl From<B256> for ShellHash {
    fn from(b: B256) -> Self {
        Self(b)
    }
}

impl From<ShellHash> for B256 {
    fn from(h: ShellHash) -> Self {
        h.0
    }
}

impl From<[u8; 32]> for ShellHash {
    fn from(arr: [u8; 32]) -> Self {
        Self(B256::from(arr))
    }
}

impl AsRef<[u8]> for ShellHash {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl alloy_rlp::Encodable for ShellHash {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        self.0.as_slice().encode(out);
    }

    fn length(&self) -> usize {
        alloy_rlp::Encodable::length(&self.0.as_slice())
    }
}

impl alloy_rlp::Decodable for ShellHash {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let bytes = <[u8; 32]>::decode(buf)?;
        Ok(Self(B256::from(bytes)))
    }
}

// ── Hash functions ────────────────────────────────────────────

/// Keccak-256 hash (Ethereum-compatible).
pub fn keccak256(data: &[u8]) -> ShellHash {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    let result: [u8; 32] = hasher.finalize().into();
    ShellHash::from(result)
}

/// BLAKE3 hash (high-performance internal use).
pub fn blake3_hash(data: &[u8]) -> ShellHash {
    let h = blake3::hash(data);
    ShellHash::from(*h.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keccak256_empty() {
        // Well-known: keccak256("") = c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        let h = keccak256(b"");
        assert_eq!(
            h.to_string(),
            "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }

    #[test]
    fn keccak256_hello() {
        let h = keccak256(b"hello");
        // Matches Ethereum's keccak256("hello")
        assert_eq!(
            h.to_string(),
            "0x1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8"
        );
    }

    #[test]
    fn blake3_deterministic() {
        let a = blake3_hash(b"shell-chain");
        let b = blake3_hash(b"shell-chain");
        assert_eq!(a, b);
    }

    #[test]
    fn shell_hash_zero() {
        assert_eq!(ShellHash::ZERO.to_string(), format!("0x{}", "0".repeat(64)));
    }

    #[test]
    fn shell_hash_serde_roundtrip() {
        let h = keccak256(b"test");
        let json = serde_json::to_string(&h).unwrap();
        let h2: ShellHash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, h2);
    }

    #[test]
    fn shell_hash_rlp_roundtrip() {
        use alloy_rlp::{Decodable, Encodable};
        let h = keccak256(b"rlp-test");
        let mut buf = Vec::new();
        h.encode(&mut buf);
        let h2 = ShellHash::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(h, h2);
    }
}
