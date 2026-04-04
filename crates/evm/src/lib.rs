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
pub mod system_contracts;
pub mod tracer;
mod tx_validation;

pub use executor::{commit_evm_state, ExecutorError, ShellEvm, TxExecutionResult};
pub use precompiles::{ShellPrecompiles, PQ_DILITHIUM_VERIFY_GAS};
pub use state_db::{ShellStateDb, StateDbError};
pub use system_contracts::{
    encode_add_validator_calldata, encode_remove_validator_calldata, execute_system_contract,
    registry_address, system_contract_code_hash, SystemContractError, SYSTEM_CALL_BASE_GAS,
    SYSTEM_CALL_OP_GAS, VALIDATOR_REGISTRY_ADDR,
};
pub use tracer::{CallFrame, TraceResult, decode_revert_reason};
pub use tx_validation::{compute_intrinsic_gas, validate_tx, TxValidationError};
