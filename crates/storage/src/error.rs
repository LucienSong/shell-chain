use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("key not found")]
    NotFound,

    #[error("database error: {0}")]
    Database(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("trie error: {0}")]
    Trie(String),

    #[error("codec error: {0}")]
    Codec(String),

    #[error("state error: {0}")]
    State(String),
}
