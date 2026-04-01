//! Shell-chain EVM integration layer.
//!
//! This crate bridges the shell-chain storage layer (WorldState + ChainStore)
//! with revm, providing:
//!
//! - [`ShellStateDb`]: implements `revm::Database` over WorldState + ChainStore
//! - (future) [`ShellEvm`]: transaction executor
//! - (future) PQ precompile provider

mod executor;
mod state_db;
mod tx_validation;

pub use executor::{ExecutorError, ShellEvm, TxExecutionResult};
pub use state_db::{ShellStateDb, StateDbError};
pub use tx_validation::{compute_intrinsic_gas, validate_tx, TxValidationError};
