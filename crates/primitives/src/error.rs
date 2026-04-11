use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrimitivesError {
    #[error("invalid bech32 string: {0}")]
    Bech32(String),

    #[error("invalid hex string: {0}")]
    HexDecode(#[from] hex::FromHexError),

    #[error("invalid length: expected {expected}, got {got}")]
    InvalidLength { expected: usize, got: usize },

    #[error("invalid slice length: expected {expected}, got {got}")]
    InvalidSliceLength { expected: usize, got: usize },

    #[error("invalid address hrp: expected {expected}, got {got}")]
    InvalidAddressHrp { expected: &'static str, got: String },
}
