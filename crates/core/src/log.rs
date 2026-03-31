use serde::{Deserialize, Serialize};
use shell_primitives::{Address, Bytes, ShellHash};

/// An EVM event log entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Log {
    /// Address of the contract that emitted this log.
    pub address: Address,
    /// Indexed topic hashes (up to 4).
    pub topics: Vec<ShellHash>,
    /// Non-indexed log data.
    pub data: Bytes,
}
