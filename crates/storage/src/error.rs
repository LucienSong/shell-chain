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
}
