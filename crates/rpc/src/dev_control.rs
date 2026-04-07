use std::sync::Arc;

/// Runtime control surface for dev-only `evm_*` RPC methods.
///
/// Implemented by the node so the RPC layer can trigger block production,
/// time manipulation, and snapshot/revert without duplicating consensus logic.
pub trait DevRpcControl: Send + Sync {
    /// Mine one or more blocks immediately.
    fn mine_blocks(&self, blocks: u64) -> Result<(), String>;

    /// Set the exact timestamp to use for the next produced block.
    fn set_next_block_timestamp(&self, timestamp: u64) -> Result<u64, String>;

    /// Increase the virtual clock offset used for future blocks.
    fn increase_time(&self, seconds: u64) -> Result<u64, String>;

    /// Capture a snapshot of the current chain/world-state/mempool state.
    fn snapshot(&self) -> Result<String, String>;

    /// Revert to a previously captured snapshot.
    fn revert(&self, snapshot_id: &str) -> Result<bool, String>;
}

pub type DynDevRpcControl = Arc<dyn DevRpcControl>;
