use serde::{Deserialize, Serialize};
use shell_primitives::{Address, Bytes, ShellHash};

/// Maximum number of indexed topics per EVM log (EVM spec limit).
pub const MAX_LOG_TOPICS: usize = 4;

/// An EVM event log entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Log {
    /// Address of the contract that emitted this log.
    pub address: Address,
    /// Indexed topic hashes (up to [`MAX_LOG_TOPICS`]).
    pub topics: Vec<ShellHash>,
    /// Non-indexed log data.
    pub data: Bytes,
}

impl Log {
    /// Create a new log entry, validating topic count ≤ [`MAX_LOG_TOPICS`].
    pub fn new(
        address: Address,
        topics: Vec<ShellHash>,
        data: Bytes,
    ) -> Result<Self, LogError> {
        if topics.len() > MAX_LOG_TOPICS {
            return Err(LogError::TooManyTopics {
                got: topics.len(),
                max: MAX_LOG_TOPICS,
            });
        }
        Ok(Self { address, topics, data })
    }
}

/// Errors related to log construction.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("too many topics: got {got}, max {max}")]
    TooManyTopics { got: usize, max: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_new_valid() {
        let log = Log::new(
            Address::default(),
            vec![ShellHash::ZERO; 4],
            Bytes::new(),
        );
        assert!(log.is_ok());
        assert_eq!(log.unwrap().topics.len(), 4);
    }

    #[test]
    fn log_new_empty_topics() {
        let log = Log::new(Address::default(), vec![], Bytes::new());
        assert!(log.is_ok());
    }

    #[test]
    fn log_new_too_many_topics() {
        let log = Log::new(
            Address::default(),
            vec![ShellHash::ZERO; 5],
            Bytes::new(),
        );
        assert!(log.is_err());
        let err = log.unwrap_err();
        assert!(err.to_string().contains("too many topics"));
    }
}
