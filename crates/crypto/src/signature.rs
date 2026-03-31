use serde::{Deserialize, Serialize};

/// Identifies which PQ signature algorithm was used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignatureType {
    Dilithium3,
    // Future: SphincsPlus, Custom(u8)
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
