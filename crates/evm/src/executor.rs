//! EVM executor: executes transactions via revm and produces receipts.
//!
//! [`ShellEvm`] wraps the revm EVM with shell-chain's state bridge and
//! provides a high-level API for executing individual transactions and
//! full blocks.

use alloy_primitives::{Bytes as AlBytes, B256, U256};
use revm::context::result::ExecutionResult;
use revm::context::{BlockEnv, CfgEnv, Context, Evm, TxEnv};
use revm::handler::instructions::EthInstructions;
use revm::handler::{ExecuteEvm, MainnetContext};
use revm::primitives::hardfork::SpecId;
use revm::primitives::{TxKind, KECCAK_EMPTY};
use revm::state::EvmState;
use shell_core::{Account, BlockHeader, TransactionReceipt};
use shell_primitives::{Address as ShellAddress, ShellHash};
use shell_storage::{ChainStore, KvStore, StorageError, WorldState};

use crate::precompiles::ShellPrecompiles;
use crate::state_db::{ShellStateDb, StateDbError};

/// Errors returned during EVM execution.
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    #[error("evm: {0}")]
    Evm(String),

    #[error("state db: {0}")]
    StateDb(#[from] StateDbError),

    #[error("storage: {0}")]
    Storage(#[from] StorageError),
}

/// Result of executing a single transaction.
pub struct TxExecutionResult {
    /// Transaction receipt for inclusion in the block.
    pub receipt: TransactionReceipt,
    /// State changes produced by this transaction (for committing).
    pub state_changes: EvmState,
    /// Gas actually used by this transaction.
    pub gas_used: u64,
    /// Raw output bytes returned by the EVM (return data or revert reason).
    pub output: Vec<u8>,
}

/// High-level EVM executor for shell-chain.
///
/// Wraps revm and provides:
/// - `execute_tx()`: execute a single validated transaction → receipt + state
/// - Block-level gas tracking for cumulative_gas_used
pub struct ShellEvm<S: KvStore + 'static> {
    state_db: ShellStateDb<S>,
    chain_id: u64,
}

impl<S: KvStore + 'static> ShellEvm<S> {
    pub fn new(state_db: ShellStateDb<S>, chain_id: u64) -> Self {
        Self { state_db, chain_id }
    }

    /// Execute a single transaction that has already been validated.
    ///
    /// The caller is responsible for running `validate_tx()` first.
    /// This method builds the revm context, runs the EVM, and produces
    /// a `TxExecutionResult` with the receipt and state changes.
    ///
    /// State changes are NOT committed — the caller must apply them to
    /// WorldState after collecting all transactions in a block.
    pub fn execute_tx(
        &mut self,
        signed_tx: &shell_core::SignedTransaction,
        header: &BlockHeader,
        tx_index: u32,
        cumulative_gas_used: u64,
    ) -> Result<TxExecutionResult, ExecutorError> {
        let tx = &signed_tx.tx;

        // Build revm TxEnv
        let kind = match &tx.to {
            Some(addr) => TxKind::Call((*addr).into()),
            None => TxKind::Create,
        };

        let tx_env = TxEnv::builder()
            .caller(signed_tx.from.into())
            .gas_limit(tx.gas_limit)
            .max_fee_per_gas(tx.max_fee_per_gas as u128)
            .gas_priority_fee(Some(tx.max_priority_fee_per_gas as u128))
            .kind(kind)
            .value(tx.value)
            .data(AlBytes::from(tx.data.as_ref().to_vec()))
            .nonce(tx.nonce)
            .chain_id(Some(self.chain_id))
            .build_fill();

        // Build revm BlockEnv
        // Use Shanghai spec: no blob gas required, no EIP-4844
        let block_env = BlockEnv {
            number: U256::from(header.number),
            beneficiary: header.proposer.into(),
            timestamp: U256::from(header.timestamp),
            gas_limit: header.gas_limit,
            basefee: 0,
            difficulty: U256::ZERO,
            prevrandao: Some(B256::ZERO),
            blob_excess_gas_and_price: None,
            slot_num: 0,
        };

        // Build revm context + EVM
        // Use SHANGHAI spec — no blob gas, no EIP-4844 requirements.
        // Shell-chain can upgrade to Cancun/Deneb when blob support is added.
        let ctx: MainnetContext<&mut ShellStateDb<S>> =
            Context::new(&mut self.state_db, SpecId::SHANGHAI)
                .modify_block_chained(|b| *b = block_env)
                .modify_cfg_chained(|cfg: &mut CfgEnv| {
                    cfg.chain_id = self.chain_id;
                    cfg.disable_nonce_check = true;
                    cfg.disable_base_fee = true;
                });

        let spec = SpecId::SHANGHAI;
        let mut evm = Evm::new(
            ctx,
            EthInstructions::new_mainnet_with_spec(spec),
            ShellPrecompiles::new(spec),
        );

        // Execute
        let result_and_state = evm
            .transact(tx_env)
            .map_err(|e| ExecutorError::Evm(format!("{e:?}")))?;

        let exec_result = result_and_state.result;
        let state = result_and_state.state;

        // Build receipt
        let gas_used = exec_result.gas().spent();
        let new_cumulative = cumulative_gas_used + gas_used;

        let (status, logs, contract_address, output_bytes) = match &exec_result {
            ExecutionResult::Success { logs, output, .. } => {
                let contract_addr = match output {
                    revm::context::result::Output::Create(_, Some(addr)) => {
                        Some(ShellAddress::from(*addr))
                    }
                    _ => None,
                };
                let data = match output {
                    revm::context::result::Output::Call(bytes) => bytes.to_vec(),
                    revm::context::result::Output::Create(bytes, _) => bytes.to_vec(),
                };
                (1u8, logs.clone(), contract_addr, data)
            }
            ExecutionResult::Revert { output, .. } => (0u8, vec![], None, output.to_vec()),
            ExecutionResult::Halt { .. } => (0u8, vec![], None, vec![]),
        };

        // Convert revm logs to shell-chain logs
        let shell_logs: Vec<shell_core::Log> = logs
            .iter()
            .filter_map(|log| {
                shell_core::Log::new(
                    ShellAddress::from(log.address),
                    log.topics().iter().map(|t| ShellHash::from(*t)).collect(),
                    shell_primitives::Bytes::from(log.data.data.to_vec()),
                )
                .ok()
            })
            .collect();

        let receipt = TransactionReceipt {
            tx_hash: signed_tx.hash(),
            block_number: header.number,
            tx_index,
            status,
            gas_used,
            cumulative_gas_used: new_cumulative,
            contract_address,
            logs_bloom: shell_primitives::Bytes::from(crate::bloom::logs_bloom(&shell_logs).to_vec()),
            logs: shell_logs,
        };

        Ok(TxExecutionResult {
            receipt,
            state_changes: state,
            gas_used,
            output: output_bytes,
        })
    }

    /// Access the underlying state database.
    pub fn state_db(&self) -> &ShellStateDb<S> {
        &self.state_db
    }

    /// Access the underlying state database mutably.
    pub fn state_db_mut(&mut self) -> &mut ShellStateDb<S> {
        &mut self.state_db
    }
}

/// Apply EVM state changes to a WorldState and ChainStore.
///
/// Iterates the revm `EvmState` (address → account) and for each touched
/// account, updates balance, nonce, contract code, and storage slots.
///
/// Call this after `ShellEvm::execute_tx()` to persist the computed state
/// diff. For multi-transaction blocks, call after **each** transaction so
/// subsequent transactions see prior state updates.
pub fn commit_evm_state<S: KvStore + 'static>(
    state: &EvmState,
    world_state: &mut WorldState<S>,
    chain_store: &ChainStore<S>,
) -> Result<(), ExecutorError> {
    for (addr, acct) in state {
        let shell_addr = ShellAddress::from(*addr);
        let info = &acct.info;

        let mut account = world_state
            .get_account(&shell_addr)?
            .unwrap_or_else(|| Account {
                pq_pubkey_hash: ShellHash::default(),
                nonce: 0,
                balance: U256::ZERO,
                validation_code_hash: None,
                code_hash: None,
                storage_root: ShellHash::ZERO,
            });

        account.nonce = info.nonce;
        account.balance = info.balance;

        // Store deployed contract bytecode
        if let Some(code) = &info.code {
            let code_bytes = code.bytes_slice();
            if !code_bytes.is_empty() && info.code_hash != KECCAK_EMPTY {
                let code_hash = ShellHash::from(info.code_hash);
                chain_store.put_code(&code_hash, code_bytes)?;
                account.code_hash = Some(code_hash);
            }
        }

        world_state.set_account(&shell_addr, &account)?;

        // Apply storage slot changes
        for (slot, value) in &acct.storage {
            let key = ShellHash::from(B256::from(*slot));
            let val = ShellHash::from(B256::from(value.present_value));
            world_state.set_storage(&shell_addr, &key, &val)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shell_core::{Account, SignedTransaction, Transaction};
    use shell_crypto::{DilithiumSigner, PQSignature, SignatureType, Signer};
    use shell_storage::{ChainStore, MemoryDb, WorldState};
    use std::sync::Arc;

    fn setup_evm() -> ShellEvm<MemoryDb> {
        let ws = WorldState::new(Arc::new(MemoryDb::new()));
        let cs = ChainStore::new(Arc::new(MemoryDb::new()));
        let state_db = ShellStateDb::new(ws, cs);
        ShellEvm::new(state_db, 1337)
    }

    fn sample_header() -> BlockHeader {
        BlockHeader {
            parent_hash: ShellHash::ZERO,
            state_root: ShellHash::ZERO,
            transactions_root: ShellHash::ZERO,
            receipts_root: ShellHash::ZERO,
            logs_bloom: shell_primitives::Bytes::new(),
            number: 1,
            timestamp: 1_000_000,
            gas_limit: 30_000_000,
            gas_used: 0,
            extra_data: shell_primitives::Bytes::new(),
            proposer: ShellAddress::ZERO,
            sig_aggregate_proof: None,
        }
    }

    fn fund_account(evm: &mut ShellEvm<MemoryDb>, addr: &ShellAddress, balance: U256) {
        let account = Account {
            pq_pubkey_hash: ShellHash::ZERO,
            nonce: 0,
            balance,
            validation_code_hash: None,
            code_hash: None,
            storage_root: ShellHash::ZERO,
        };
        evm.state_db_mut()
            .world_state_mut()
            .set_account(addr, &account)
            .unwrap();
    }

    #[test]
    fn execute_simple_transfer() {
        let mut evm = setup_evm();

        let signer = DilithiumSigner::generate();
        let from = ShellAddress::from_public_key(signer.public_key());
        let to = ShellAddress::from([0x01; 20]);

        // Fund sender with plenty of balance
        fund_account(&mut evm, &from, U256::from(10_000_000_000u64));

        let tx = Transaction {
            chain_id: 1337,
            nonce: 0,
            to: Some(to),
            value: U256::from(1000),
            data: shell_primitives::Bytes::new(),
            gas_limit: 21_000,
            max_fee_per_gas: 10,
            max_priority_fee_per_gas: 1,
        };

        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xAA; 100]);
        let signed = SignedTransaction::new(from, tx, sig);

        let header = sample_header();
        let result = evm.execute_tx(&signed, &header, 0, 0);
        assert!(result.is_ok(), "execute_tx failed: {:?}", result.err());

        let tx_result = result.unwrap();
        assert_eq!(tx_result.receipt.status, 1); // success
        assert_eq!(tx_result.receipt.tx_index, 0);
        assert_eq!(tx_result.receipt.block_number, 1);
        assert!(tx_result.gas_used > 0);
        assert!(tx_result.gas_used <= 21_000);
    }

    #[test]
    fn execute_transfer_insufficient_gas_limit() {
        let mut evm = setup_evm();

        let from = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &from, U256::from(10_000_000_000u64));

        let tx = Transaction {
            chain_id: 1337,
            nonce: 0,
            to: Some(ShellAddress::from([0x01; 20])),
            value: U256::from(100),
            data: shell_primitives::Bytes::new(),
            gas_limit: 100, // way too low
            max_fee_per_gas: 10,
            max_priority_fee_per_gas: 1,
        };

        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xBB; 100]);
        let signed = SignedTransaction::new(from, tx, sig);

        let header = sample_header();
        // Should fail at the EVM level (intrinsic gas too low)
        let result = evm.execute_tx(&signed, &header, 0, 0);
        // This should be an error from revm
        assert!(result.is_err());
    }

    #[test]
    fn execute_contract_creation() {
        let mut evm = setup_evm();

        let from = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &from, U256::from(100_000_000_000u64));

        // Simple contract: PUSH1 0x42 PUSH1 0 MSTORE PUSH1 1 PUSH1 31 RETURN
        // This stores 0x42 at memory[0] and returns 1 byte from offset 31
        let init_code = vec![
            0x60, 0x42, // PUSH1 0x42
            0x60, 0x00, // PUSH1 0
            0x52, // MSTORE
            0x60, 0x01, // PUSH1 1
            0x60, 0x1f, // PUSH1 31
            0xf3, // RETURN
        ];

        let tx = Transaction {
            chain_id: 1337,
            nonce: 0,
            to: None, // contract creation
            value: U256::ZERO,
            data: shell_primitives::Bytes::from(init_code),
            gas_limit: 100_000,
            max_fee_per_gas: 10,
            max_priority_fee_per_gas: 1,
        };

        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xCC; 100]);
        let signed = SignedTransaction::new(from, tx, sig);

        let header = sample_header();
        let result = evm.execute_tx(&signed, &header, 0, 0);
        assert!(result.is_ok(), "contract creation failed: {:?}", result.err());

        let tx_result = result.unwrap();
        assert_eq!(tx_result.receipt.status, 1);
        // Contract creation should have a contract_address
        assert!(tx_result.receipt.contract_address.is_some());
    }
}
