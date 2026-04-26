//! Extension methods for persisting the wPoA `ValidatorSet` in `ChainStore`.

use shell_consensus::ValidatorSet;
use shell_storage::{ChainStore, KvStore, StorageError};

/// Storage key for the current validator set snapshot.
pub const VALIDATOR_SET_KEY: &[u8] = b"consensus/validator_set";

/// Extension trait for `ChainStore` that adds wPoA validator-set persistence.
pub trait ValidatorStoreExt {
    /// Persist the current validator set snapshot.
    fn put_validator_set(&self, vs: &ValidatorSet) -> Result<(), StorageError>;

    /// Load the persisted validator set, returning `None` if not yet stored.
    fn get_validator_set(&self) -> Result<Option<ValidatorSet>, StorageError>;
}

impl<S: KvStore> ValidatorStoreExt for ChainStore<S> {
    fn put_validator_set(&self, vs: &ValidatorSet) -> Result<(), StorageError> {
        let encoded = serde_json::to_vec(vs)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        self.store().put(VALIDATOR_SET_KEY, &encoded)
    }

    fn get_validator_set(&self) -> Result<Option<ValidatorSet>, StorageError> {
        match self.store().get(VALIDATOR_SET_KEY)? {
            Some(bytes) => {
                let vs = serde_json::from_slice(&bytes)
                    .map_err(|e| StorageError::Serialization(e.to_string()))?;
                Ok(Some(vs))
            }
            None => Ok(None),
        }
    }
}
