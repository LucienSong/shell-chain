use serde::{Deserialize, Serialize};
use alloy_rlp::Encodable;

/// Identifies which PQ signature algorithm was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignatureType {
    /// CRYSTALS-Dilithium3 (pre-FIPS, `pqcrypto-dilithium 0.5`).
    /// Based on the Round 3 submission, NOT the final FIPS 204 ML-DSA-65.
    Dilithium3,
    /// FIPS 204 ML-DSA-65. Reserved for future migration when a compliant
    /// Rust implementation is available and verified.
    MlDsa65,
}

impl SignatureType {
    pub fn as_u8(&self) -> u8 {
        match self {
            SignatureType::Dilithium3 => 0,
            SignatureType::MlDsa65 => 1,
        }
    }

    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(SignatureType::Dilithium3),
            1 => Some(SignatureType::MlDsa65),
            _ => None,
        }
    }
}

/// Container for a post-quantum signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PQSignature {
    pub sig_type: SignatureType,
    pub data: Vec<u8>,
}

impl PQSignature {
    pub fn new(sig_type: SignatureType, data: Vec<u8>) -> Self {
        Self { sig_type, data }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl Encodable for PQSignature {
    fn encode(&self, out: &mut dyn alloy_rlp::BufMut) {
        let header = alloy_rlp::Header {
            list: true,
            payload_length: self.fields_len(),
        };
        header.encode(out);
        self.sig_type.as_u8().encode(out);
        self.data.as_slice().encode(out);
    }

    fn length(&self) -> usize {
        let payload = self.fields_len();
        alloy_rlp::Header { list: true, payload_length: payload }.length() + payload
    }
}

impl alloy_rlp::Decodable for PQSignature {
    fn decode(buf: &mut &[u8]) -> alloy_rlp::Result<Self> {
        let header = alloy_rlp::Header::decode(buf)?;
        if !header.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let sig_type_u8 = u8::decode(buf)?;
        let sig_type = SignatureType::from_u8(sig_type_u8)
            .ok_or(alloy_rlp::Error::Custom("unknown signature type"))?;
        let data = alloy_rlp::Header::decode_bytes(buf, false)?.to_vec();
        Ok(Self { sig_type, data })
    }
}

impl PQSignature {
    fn fields_len(&self) -> usize {
        self.sig_type.as_u8().length() + self.data.as_slice().length()
    }
}
