use serde::{Deserialize, Serialize};
use core::fmt;

/// Variable-length byte container used across Shell-Chain.
#[derive(Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bytes(pub alloy_primitives::Bytes);

impl Bytes {
    pub fn new() -> Self {
        Self(alloy_primitives::Bytes::new())
    }

    pub fn from_static(s: &'static [u8]) -> Self {
        Self(alloy_primitives::Bytes::from_static(s))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Construct from a byte slice (infallible, variable-length).
    pub fn try_from_slice(slice: &[u8]) -> Self {
        Self(alloy_primitives::Bytes::copy_from_slice(slice))
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bytes(0x{})", hex::encode(&self.0))
    }
}

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{}", hex::encode(&self.0))
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(v: Vec<u8>) -> Self {
        Self(alloy_primitives::Bytes::from(v))
    }
}

impl From<&[u8]> for Bytes {
    fn from(s: &[u8]) -> Self {
        Self(alloy_primitives::Bytes::copy_from_slice(s))
    }
}

impl From<alloy_primitives::Bytes> for Bytes {
    fn from(b: alloy_primitives::Bytes) -> Self {
        Self(b)
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl alloy_rlp::Encodable for Bytes {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let slice: &[u8] = self.0.as_ref();
        slice.encode(out);
    }

    fn length(&self) -> usize {
        let slice: &[u8] = self.0.as_ref();
        alloy_rlp::Encodable::length(&slice)
    }
}

impl alloy_rlp::Decodable for Bytes {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let raw = alloy_rlp::Header::decode_bytes(buf, false)?;
        Ok(Self(alloy_primitives::Bytes::copy_from_slice(raw)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_basic() {
        let b = Bytes::from(vec![1, 2, 3]);
        assert_eq!(b.len(), 3);
        assert!(!b.is_empty());
        assert_eq!(b.as_ref(), &[1, 2, 3]);
    }

    #[test]
    fn bytes_empty() {
        let b = Bytes::new();
        assert!(b.is_empty());
    }

    #[test]
    fn bytes_display() {
        let b = Bytes::from(vec![0xDE, 0xAD]);
        assert_eq!(b.to_string(), "0xdead");
    }

    #[test]
    fn bytes_serde_roundtrip() {
        let b = Bytes::from(vec![0xCA, 0xFE]);
        let json = serde_json::to_string(&b).unwrap();
        let b2: Bytes = serde_json::from_str(&json).unwrap();
        assert_eq!(b, b2);
    }

    #[test]
    fn bytes_rlp_roundtrip() {
        use alloy_rlp::{Decodable, Encodable};
        let b = Bytes::from(vec![1, 2, 3, 4, 5]);
        let mut buf = Vec::new();
        b.encode(&mut buf);
        let b2 = Bytes::decode(&mut buf.as_slice()).unwrap();
        assert_eq!(b, b2);
    }
}
