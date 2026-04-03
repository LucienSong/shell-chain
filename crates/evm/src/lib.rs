//! Shell-chain EVM integration layer.
//!
//! This crate bridges the shell-chain storage layer (WorldState + ChainStore)
//! with revm, providing:
//!
//! - [`ShellStateDb`]: implements `revm::Database` over WorldState + ChainStore
//! - [`ShellEvm`]: transaction executor (Shanghai spec)
//! - [`ShellPrecompiles`]: PQ precompile provider (ecrecover disabled, PQ_DILITHIUM_VERIFY at 0x0100)
//! - [`validate_tx`]: PQ signature verification + hybrid pubkey registration

pub mod bloom;
mod executor;
mod precompiles;
mod state_db;
mod tx_validation;

pub use executor::{commit_evm_state, ExecutorError, ShellEvm, TxExecutionResult};
pub use precompiles::{ShellPrecompiles, PQ_DILITHIUM_VERIFY_GAS};
pub use state_db::{ShellStateDb, StateDbError};
pub use tx_validation::{compute_intrinsic_gas, validate_tx, TxValidationError};
