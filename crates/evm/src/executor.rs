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
use crate::system_contracts::{self, execute_system_contract, SYSTEM_CALL_BASE_GAS};

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
    /// True if this was a system contract transaction whose state changes
    /// were applied directly to the EVM's WorldState (not via EvmState).
    pub is_system_tx: bool,
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
    ///
    /// **System contract intercept**: if the transaction targets the
    /// ValidatorRegistry at 0x0000…0001, native Rust logic handles it
    /// instead of routing through revm.
    pub fn execute_tx(
        &mut self,
        signed_tx: &shell_core::SignedTransaction,
        header: &BlockHeader,
        tx_index: u32,
        cumulative_gas_used: u64,
    ) -> Result<TxExecutionResult, ExecutorError> {
        let tx = &signed_tx.tx;

        // ── System contract intercept ──────────────────────────
        if let Some(to) = &tx.to {
            if to.as_bytes() == &system_contracts::VALIDATOR_REGISTRY_ADDR {
                return self.execute_system_contract_tx(
                    signed_tx,
                    header,
                    tx_index,
                    cumulative_gas_used,
                );
            }
        }

        // ── Normal EVM execution path ──────────────────────────
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
            is_system_tx: false,
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

    /// Execute a transaction targeting the ValidatorRegistry system contract.
    ///
    /// Runs native Rust logic instead of the EVM, produces appropriate logs,
    /// and charges a fixed gas fee.
    fn execute_system_contract_tx(
        &mut self,
        signed_tx: &shell_core::SignedTransaction,
        header: &BlockHeader,
        tx_index: u32,
        cumulative_gas_used: u64,
    ) -> Result<TxExecutionResult, ExecutorError> {
        let caller = &signed_tx.from;
        let input = signed_tx.tx.data.as_ref();
        let ws = self.state_db.world_state_mut();

        let result = execute_system_contract(caller, input, ws);

        match result {
            Ok((output, gas_used)) => {
                let new_cumulative = cumulative_gas_used + gas_used;

                // Build event logs for mutating operations
                let mut shell_logs = Vec::new();
                if input.len() >= 4 {
                    let selector: [u8; 4] = input[..4].try_into().unwrap();
                    let registry_addr = system_contracts::registry_address();
                    if selector == system_contracts::ADD_VALIDATOR_SELECTOR {
                        if let Ok(addr) = system_contracts::decode_address(&input[4..]) {
                            let topic = ShellHash::from(system_contracts::validator_added_topic());
                            let mut addr_word = [0u8; 32];
                            addr_word[12..32].copy_from_slice(addr.as_bytes());
                            if let Ok(log) = shell_core::Log::new(
                                registry_addr,
                                vec![topic],
                                shell_primitives::Bytes::from(addr_word.to_vec()),
                            ) {
                                shell_logs.push(log);
                            }
                        }
                    } else if selector == system_contracts::REMOVE_VALIDATOR_SELECTOR {
                        if let Ok(addr) = system_contracts::decode_address(&input[4..]) {
                            let topic =
                                ShellHash::from(system_contracts::validator_removed_topic());
                            let mut addr_word = [0u8; 32];
                            addr_word[12..32].copy_from_slice(addr.as_bytes());
                            if let Ok(log) = shell_core::Log::new(
                                registry_addr,
                                vec![topic],
                                shell_primitives::Bytes::from(addr_word.to_vec()),
                            ) {
                                shell_logs.push(log);
                            }
                        }
                    }
                }

                let receipt = TransactionReceipt {
                    tx_hash: signed_tx.hash(),
                    block_number: header.number,
                    tx_index,
                    status: 1, // success
                    gas_used,
                    cumulative_gas_used: new_cumulative,
                    contract_address: None,
                    logs_bloom: shell_primitives::Bytes::from(
                        crate::bloom::logs_bloom(&shell_logs).to_vec(),
                    ),
                    logs: shell_logs,
                };

                Ok(TxExecutionResult {
                    receipt,
                    state_changes: EvmState::default(),
                    gas_used,
                    output,
                    is_system_tx: true,
                })
            }
            Err(e) => {
                // System contract reverted — produce a failed receipt
                let gas_used = SYSTEM_CALL_BASE_GAS;
                let new_cumulative = cumulative_gas_used + gas_used;
                let revert_msg = e.to_string().into_bytes();

                let receipt = TransactionReceipt {
                    tx_hash: signed_tx.hash(),
                    block_number: header.number,
                    tx_index,
                    status: 0, // failure
                    gas_used,
                    cumulative_gas_used: new_cumulative,
                    contract_address: None,
                    logs_bloom: shell_primitives::Bytes::new(),
                    logs: vec![],
                };

                Ok(TxExecutionResult {
                    receipt,
                    state_changes: EvmState::default(),
                    gas_used,
                    output: revert_msg,
                    is_system_tx: true,
                })
            }
        }
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
            base_fee_per_gas: 0,
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

    // ── Helper: build a system contract tx ─────────────────────

    fn make_system_tx(
        from: ShellAddress,
        calldata: Vec<u8>,
    ) -> SignedTransaction {
        let tx = Transaction {
            chain_id: 1337,
            nonce: 0,
            to: Some(system_contracts::registry_address()),
            value: U256::ZERO,
            data: shell_primitives::Bytes::from(calldata),
            gas_limit: 100_000,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
        };
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xDD; 100]);
        SignedTransaction::new(from, tx, sig)
    }

    // ── System contract executor integration tests ─────────────

    #[test]
    fn execute_add_validator_via_executor() {
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        let new_val = ShellAddress::from([0x02; 20]);

        // Seed v1 as an existing validator
        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1])
            .unwrap();

        let calldata = system_contracts::encode_add_validator_calldata(&new_val);
        let signed = make_system_tx(v1, calldata);
        let header = sample_header();

        let result = evm.execute_tx(&signed, &header, 0, 0);
        assert!(result.is_ok(), "addValidator tx failed: {:?}", result.err());

        let tx_result = result.unwrap();
        assert_eq!(tx_result.receipt.status, 1);
        assert!(tx_result.is_system_tx);
        assert_eq!(
            tx_result.gas_used,
            system_contracts::SYSTEM_CALL_BASE_GAS + system_contracts::SYSTEM_CALL_OP_GAS
        );
        assert_eq!(tx_result.receipt.block_number, 1);
        assert_eq!(tx_result.receipt.tx_index, 0);
        assert!(tx_result.receipt.contract_address.is_none());
        // Output should be ABI-encoded true
        assert_eq!(tx_result.output, system_contracts::encode_bool(true));

        // Verify the validator was actually added
        let validators = evm.state_db_mut().world_state_mut().get_validators().unwrap();
        assert_eq!(validators.len(), 2);
        assert!(validators.contains(&new_val));
    }

    #[test]
    fn execute_remove_validator_via_executor() {
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        let v2 = ShellAddress::from([0x02; 20]);

        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1, v2])
            .unwrap();

        let calldata = system_contracts::encode_remove_validator_calldata(&v2);
        let signed = make_system_tx(v1, calldata);
        let header = sample_header();

        let tx_result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        assert_eq!(tx_result.receipt.status, 1);
        assert!(tx_result.is_system_tx);

        let validators = evm.state_db_mut().world_state_mut().get_validators().unwrap();
        assert_eq!(validators, vec![v1]);
    }

    #[test]
    fn system_tx_flag_is_true_for_system_contract() {
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1])
            .unwrap();

        // A read-only system call (getValidators)
        let calldata = system_contracts::GET_VALIDATORS_SELECTOR.to_vec();
        let signed = make_system_tx(v1, calldata);
        let header = sample_header();

        let tx_result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        assert!(tx_result.is_system_tx);
    }

    #[test]
    fn normal_tx_is_not_system_tx() {
        let mut evm = setup_evm();
        let from = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &from, U256::from(10_000_000_000u64));

        let tx = Transaction {
            chain_id: 1337,
            nonce: 0,
            to: Some(ShellAddress::from([0x01; 20])),
            value: U256::from(100),
            data: shell_primitives::Bytes::new(),
            gas_limit: 21_000,
            max_fee_per_gas: 10,
            max_priority_fee_per_gas: 1,
        };
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xAA; 100]);
        let signed = SignedTransaction::new(from, tx, sig);
        let header = sample_header();

        let tx_result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        assert!(!tx_result.is_system_tx);
    }

    #[test]
    fn system_tx_invalid_calldata_produces_failed_receipt() {
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1])
            .unwrap();

        // Too short (< 4 bytes)
        let signed = make_system_tx(v1, vec![0x00, 0x01]);
        let header = sample_header();

        let tx_result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        assert_eq!(tx_result.receipt.status, 0); // failed
        assert!(tx_result.is_system_tx);
        assert_eq!(tx_result.gas_used, system_contracts::SYSTEM_CALL_BASE_GAS);
        assert!(tx_result.receipt.logs.is_empty());
    }

    #[test]
    fn system_tx_unknown_selector_produces_failed_receipt() {
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1])
            .unwrap();

        let signed = make_system_tx(v1, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let header = sample_header();

        let tx_result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        assert_eq!(tx_result.receipt.status, 0);
        assert!(tx_result.is_system_tx);
        // Revert message should contain "unknown function selector"
        let msg = String::from_utf8_lossy(&tx_result.output);
        assert!(msg.contains("unknown function selector"), "got: {msg}");
    }

    #[test]
    fn system_tx_unauthorized_produces_failed_receipt() {
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        let outsider = ShellAddress::from([0x99; 20]);
        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1])
            .unwrap();

        let new_val = ShellAddress::from([0x02; 20]);
        let calldata = system_contracts::encode_add_validator_calldata(&new_val);
        let signed = make_system_tx(outsider, calldata);
        let header = sample_header();

        let tx_result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        assert_eq!(tx_result.receipt.status, 0);
        assert!(tx_result.is_system_tx);
        let msg = String::from_utf8_lossy(&tx_result.output);
        assert!(msg.contains("unauthorized"), "got: {msg}");
    }

    #[test]
    fn system_tx_generates_event_logs() {
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        let new_val = ShellAddress::from([0x02; 20]);
        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1])
            .unwrap();

        let calldata = system_contracts::encode_add_validator_calldata(&new_val);
        let signed = make_system_tx(v1, calldata);
        let header = sample_header();

        let tx_result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        assert_eq!(tx_result.receipt.status, 1);

        // Should have exactly one ValidatorAdded log
        assert_eq!(tx_result.receipt.logs.len(), 1);
        let log = &tx_result.receipt.logs[0];
        assert_eq!(log.address, system_contracts::registry_address());
        assert_eq!(log.topics.len(), 1);
        assert_eq!(
            log.topics[0],
            ShellHash::from(system_contracts::validator_added_topic())
        );
        // Log data should be the ABI-encoded address
        let mut expected_data = [0u8; 32];
        expected_data[12..32].copy_from_slice(new_val.as_bytes());
        assert_eq!(log.data.as_ref(), &expected_data);
    }

    #[test]
    fn system_tx_remove_generates_removed_event() {
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        let v2 = ShellAddress::from([0x02; 20]);
        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1, v2])
            .unwrap();

        let calldata = system_contracts::encode_remove_validator_calldata(&v2);
        let signed = make_system_tx(v1, calldata);
        let header = sample_header();

        let tx_result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        assert_eq!(tx_result.receipt.status, 1);

        assert_eq!(tx_result.receipt.logs.len(), 1);
        let log = &tx_result.receipt.logs[0];
        assert_eq!(
            log.topics[0],
            ShellHash::from(system_contracts::validator_removed_topic())
        );
    }

    #[test]
    fn system_tx_cumulative_gas_is_correct() {
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1])
            .unwrap();

        let calldata = system_contracts::GET_VALIDATORS_SELECTOR.to_vec();
        let signed = make_system_tx(v1, calldata);
        let header = sample_header();

        let prior_cumulative = 50_000u64;
        let tx_result = evm.execute_tx(&signed, &header, 1, prior_cumulative).unwrap();
        assert_eq!(
            tx_result.receipt.cumulative_gas_used,
            prior_cumulative + tx_result.gas_used
        );
        assert_eq!(tx_result.receipt.tx_index, 1);
    }

    #[test]
    fn system_tx_state_changes_are_empty() {
        // System contract changes go directly to WorldState, not via EvmState
        let mut evm = setup_evm();
        let v1 = ShellAddress::from([0x01; 20]);
        let new_val = ShellAddress::from([0x02; 20]);
        evm.state_db_mut()
            .world_state_mut()
            .set_validators(&[v1])
            .unwrap();

        let calldata = system_contracts::encode_add_validator_calldata(&new_val);
        let signed = make_system_tx(v1, calldata);
        let header = sample_header();

        let tx_result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        assert!(tx_result.state_changes.is_empty());
    }

    // ── Helpers for advanced EVM tests ────────────────────────

    fn commit_state(evm: &mut ShellEvm<MemoryDb>, state: &EvmState) {
        let (ws, cs) = evm.state_db_mut().world_state_and_chain_store();
        commit_evm_state(state, ws, cs).unwrap();
    }

    fn deploy_contract(
        evm: &mut ShellEvm<MemoryDb>,
        from: &ShellAddress,
        init_code: Vec<u8>,
        value: U256,
        nonce: u64,
    ) -> (TxExecutionResult, ShellAddress) {
        let tx = Transaction {
            chain_id: 1337,
            nonce,
            to: None,
            value,
            data: shell_primitives::Bytes::from(init_code),
            gas_limit: 5_000_000,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
        };
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xCC; 100]);
        let signed = SignedTransaction::new(*from, tx, sig);
        let header = sample_header();
        let result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        let addr = result.receipt.contract_address.unwrap();
        commit_state(evm, &result.state_changes);
        (result, addr)
    }

    fn call_contract(
        evm: &mut ShellEvm<MemoryDb>,
        from: &ShellAddress,
        to: &ShellAddress,
        calldata: Vec<u8>,
        value: U256,
        nonce: u64,
        gas_limit: u64,
    ) -> TxExecutionResult {
        let tx = Transaction {
            chain_id: 1337,
            nonce,
            to: Some(*to),
            value,
            data: shell_primitives::Bytes::from(calldata),
            gas_limit,
            max_fee_per_gas: 0,
            max_priority_fee_per_gas: 0,
        };
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xDD; 100]);
        let signed = SignedTransaction::new(*from, tx, sig);
        let header = sample_header();
        let result = evm.execute_tx(&signed, &header, 0, 0).unwrap();
        commit_state(evm, &result.state_changes);
        result
    }

    /// Build init code that deploys `runtime` as contract code.
    /// Uses CODECOPY to copy the runtime bytes appended after the prefix.
    fn make_init_code(runtime: &[u8]) -> Vec<u8> {
        let runtime_len = runtime.len();
        assert!(runtime_len <= 0xFFFF, "runtime too large for PUSH2");
        let mut init = Vec::new();
        if runtime_len <= 255 {
            // PUSH1 len, PUSH1 offset, PUSH1 0, CODECOPY, PUSH1 len, PUSH1 0, RETURN
            let prefix_len: u8 = 12;
            init.extend_from_slice(&[
                0x60, runtime_len as u8,
                0x60, prefix_len,
                0x60, 0x00,
                0x39, // CODECOPY
                0x60, runtime_len as u8,
                0x60, 0x00,
                0xF3, // RETURN
            ]);
        } else {
            // PUSH2 len, PUSH2 offset, PUSH1 0, CODECOPY, PUSH2 len, PUSH1 0, RETURN
            let prefix_len: u16 = 15;
            init.extend_from_slice(&[
                0x61, (runtime_len >> 8) as u8, (runtime_len & 0xFF) as u8,
                0x61, (prefix_len >> 8) as u8, (prefix_len & 0xFF) as u8,
                0x60, 0x00,
                0x39, // CODECOPY
                0x61, (runtime_len >> 8) as u8, (runtime_len & 0xFF) as u8,
                0x60, 0x00,
                0xF3, // RETURN
            ]);
        }
        init.extend_from_slice(runtime);
        init
    }

    // ════════════════════════════════════════════════════════════
    //  CREATE2 tests
    // ════════════════════════════════════════════════════════════

    #[test]
    fn create2_deploy_and_verify_address() {
        use alloy_primitives::keccak256;

        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Child init code: returns 1-byte runtime 0x42
        let child_init: Vec<u8> = vec![
            0x60, 0x42, 0x60, 0x00, 0x52, 0x60, 0x01, 0x60, 0x1f, 0xf3,
        ];

        // Factory runtime: store child_init in memory → CREATE2(val=0, off, sz, salt=1)
        // → return created address
        let mut factory_rt = Vec::new();
        factory_rt.push(0x69); // PUSH10
        factory_rt.extend_from_slice(&child_init);
        factory_rt.extend_from_slice(&[
            0x60, 0x00, 0x52,       // MSTORE (right-aligned at mem[22..32])
            0x60, 0x01,             // PUSH1 1 (salt)
            0x60, 0x0a,             // PUSH1 10 (size)
            0x60, 0x16,             // PUSH1 22 (offset = 32-10)
            0x60, 0x00,             // PUSH1 0 (value)
            0xf5,                   // CREATE2
            0x60, 0x00, 0x52,       // store addr at mem[0]
            0x60, 0x20, 0x60, 0x00, 0xf3, // RETURN 32 bytes
        ]);

        let factory_init = make_init_code(&factory_rt);
        let (_, factory_addr) = deploy_contract(&mut evm, &deployer, factory_init, U256::ZERO, 0);

        // Call factory to trigger CREATE2
        let result = call_contract(
            &mut evm, &deployer, &factory_addr, vec![], U256::ZERO, 1, 5_000_000,
        );
        assert_eq!(result.receipt.status, 1, "CREATE2 call failed");
        assert_eq!(result.output.len(), 32);
        let created_addr = ShellAddress::from_slice(&result.output[12..32]);

        // Verify via CREATE2 formula: keccak256(0xff ++ factory ++ salt ++ keccak256(init))
        let init_hash = keccak256(&child_init);
        let salt = B256::from(U256::from(1));
        let mut pre = vec![0xff];
        pre.extend_from_slice(factory_addr.as_bytes());
        pre.extend_from_slice(salt.as_ref());
        pre.extend_from_slice(init_hash.as_ref());
        let expected = ShellAddress::from_slice(&keccak256(&pre)[12..]);
        assert_eq!(created_addr, expected, "CREATE2 address mismatch");
    }

    #[test]
    fn create2_same_salt_collision_returns_zero() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        let child_init: Vec<u8> = vec![
            0x60, 0x42, 0x60, 0x00, 0x52, 0x60, 0x01, 0x60, 0x1f, 0xf3,
        ];
        let mut factory_rt = Vec::new();
        factory_rt.push(0x69); // PUSH10
        factory_rt.extend_from_slice(&child_init);
        factory_rt.extend_from_slice(&[
            0x60, 0x00, 0x52,
            0x60, 0x00,             // salt = 0
            0x60, 0x0a, 0x60, 0x16, 0x60, 0x00, 0xf5,
            0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3,
        ]);
        let factory_init = make_init_code(&factory_rt);
        let (_, factory_addr) = deploy_contract(&mut evm, &deployer, factory_init, U256::ZERO, 0);

        // First CREATE2
        let r1 = call_contract(&mut evm, &deployer, &factory_addr, vec![], U256::ZERO, 1, 5_000_000);
        assert_eq!(r1.receipt.status, 1);
        assert_ne!(&r1.output[12..32], &[0u8; 20], "first deploy should succeed");

        // Second CREATE2 with same salt → address collision, returns address(0)
        let r2 = call_contract(&mut evm, &deployer, &factory_addr, vec![], U256::ZERO, 2, 5_000_000);
        assert_eq!(r2.receipt.status, 1, "outer call should succeed");
        assert_eq!(&r2.output[12..32], &[0u8; 20], "collision should return zero");
    }

    #[test]
    fn create2_deterministic_address() {
        use alloy_primitives::keccak256;

        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        let child_init: Vec<u8> = vec![
            0x60, 0xAA, 0x60, 0x00, 0x52, 0x60, 0x01, 0x60, 0x1f, 0xf3,
        ];
        let mut factory_rt = Vec::new();
        factory_rt.push(0x69);
        factory_rt.extend_from_slice(&child_init);
        factory_rt.extend_from_slice(&[
            0x60, 0x00, 0x52,
            0x60, 0x42,             // salt = 0x42
            0x60, 0x0a, 0x60, 0x16, 0x60, 0x00, 0xf5,
            0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3,
        ]);
        let factory_init = make_init_code(&factory_rt);
        let (_, factory_addr) = deploy_contract(&mut evm, &deployer, factory_init, U256::ZERO, 0);

        let r = call_contract(&mut evm, &deployer, &factory_addr, vec![], U256::ZERO, 1, 5_000_000);
        assert_eq!(r.receipt.status, 1);
        let created = ShellAddress::from_slice(&r.output[12..32]);

        let init_hash = keccak256(&child_init);
        let salt = B256::from(U256::from(0x42));
        let mut pre = vec![0xff];
        pre.extend_from_slice(factory_addr.as_bytes());
        pre.extend_from_slice(salt.as_ref());
        pre.extend_from_slice(init_hash.as_ref());
        let expected = ShellAddress::from_slice(&keccak256(&pre)[12..]);
        assert_eq!(created, expected);
    }

    // ════════════════════════════════════════════════════════════
    //  SELFDESTRUCT tests
    // ════════════════════════════════════════════════════════════

    #[test]
    fn selfdestruct_transfers_balance() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        let beneficiary = ShellAddress::from([0xBB; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Runtime: PUSH20 <beneficiary> SELFDESTRUCT
        let mut runtime = vec![0x73]; // PUSH20
        runtime.extend_from_slice(beneficiary.as_bytes());
        runtime.push(0xFF); // SELFDESTRUCT

        let init_code = make_init_code(&runtime);
        let deposit = U256::from(1_000_000_000u64);
        let (_, contract_addr) = deploy_contract(&mut evm, &deployer, init_code, deposit, 0);

        let result = call_contract(
            &mut evm, &deployer, &contract_addr, vec![], U256::ZERO, 1, 100_000,
        );
        assert_eq!(result.receipt.status, 1, "selfdestruct tx failed");

        let ben_bal = evm.state_db_mut().world_state_mut()
            .get_balance(&beneficiary).unwrap();
        assert!(ben_bal >= deposit, "beneficiary should receive balance");
    }

    #[test]
    fn selfdestruct_to_self_zeroes_balance() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Runtime: ADDRESS SELFDESTRUCT
        let runtime = vec![0x30, 0xFF];
        let init_code = make_init_code(&runtime);
        let deposit = U256::from(5_000_000u64);
        let (_, contract_addr) = deploy_contract(&mut evm, &deployer, init_code, deposit, 0);

        let result = call_contract(
            &mut evm, &deployer, &contract_addr, vec![], U256::ZERO, 1, 100_000,
        );
        assert_eq!(result.receipt.status, 1);

        let balance = evm.state_db_mut().world_state_mut()
            .get_balance(&contract_addr).unwrap();
        assert_eq!(balance, U256::ZERO, "self-destruct to self should zero balance");
    }

    #[test]
    fn selfdestruct_post_shanghai_code_remains() {
        // Post-Shanghai (EIP-6780): SELFDESTRUCT in a separate tx only
        // transfers balance; code/storage remain.
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        let beneficiary = ShellAddress::from([0xBB; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Runtime: SSTORE(0, 0x42) then SELFDESTRUCT to beneficiary
        let mut runtime = vec![
            0x60, 0x42, 0x60, 0x00, 0x55, // SSTORE(0, 0x42)
            0x73,
        ];
        runtime.extend_from_slice(beneficiary.as_bytes());
        runtime.push(0xFF);

        let init_code = make_init_code(&runtime);
        let (_, contract_addr) = deploy_contract(&mut evm, &deployer, init_code, U256::from(1_000_000u64), 0);

        // Trigger SELFDESTRUCT in a separate transaction
        let result = call_contract(
            &mut evm, &deployer, &contract_addr, vec![], U256::ZERO, 1, 200_000,
        );
        assert_eq!(result.receipt.status, 1);

        // Post-Shanghai: code hash should still exist
        let code_hash = evm.state_db_mut().world_state_mut()
            .get_code_hash(&contract_addr).unwrap();
        assert!(code_hash.is_some(), "code should remain post-Shanghai SELFDESTRUCT");
    }

    // ════════════════════════════════════════════════════════════
    //  DELEGATECALL tests
    // ════════════════════════════════════════════════════════════

    #[test]
    fn delegatecall_storage_writes_to_proxy() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Logic: PUSH1 0xAA  PUSH1 0  SSTORE  STOP
        let logic_rt = vec![0x60, 0xAA, 0x60, 0x00, 0x55, 0x00];
        let (_, logic_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&logic_rt), U256::ZERO, 0);

        // Proxy: DELEGATECALL(gas, logic_addr, 0, 0, 0, 0) POP STOP
        let mut proxy_rt = vec![
            0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, // retSz retOff argsSz argsOff
            0x73,
        ];
        proxy_rt.extend_from_slice(logic_addr.as_bytes());
        proxy_rt.extend_from_slice(&[0x5A, 0xF4, 0x50, 0x00]);
        let (_, proxy_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&proxy_rt), U256::ZERO, 1);

        let result = call_contract(&mut evm, &deployer, &proxy_addr, vec![], U256::ZERO, 2, 500_000);
        assert_eq!(result.receipt.status, 1, "delegatecall failed");

        // Storage written in proxy's context
        let slot = ShellHash::ZERO;
        let proxy_val = evm.state_db_mut().world_state_mut()
            .get_storage(&proxy_addr, &slot).unwrap();
        let mut expected = [0u8; 32];
        expected[31] = 0xAA;
        assert_eq!(proxy_val.as_bytes(), &expected);

        // Logic contract's storage untouched
        let logic_val = evm.state_db_mut().world_state_mut()
            .get_storage(&logic_addr, &slot).unwrap();
        assert_eq!(logic_val, ShellHash::ZERO);
    }

    #[test]
    fn delegatecall_preserves_msg_sender() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Logic: CALLER PUSH1 0 SSTORE STOP
        let logic_rt = vec![0x33, 0x60, 0x00, 0x55, 0x00];
        let (_, logic_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&logic_rt), U256::ZERO, 0);

        // Proxy: DELEGATECALL to logic
        let mut proxy_rt = vec![
            0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00,
            0x73,
        ];
        proxy_rt.extend_from_slice(logic_addr.as_bytes());
        proxy_rt.extend_from_slice(&[0x5A, 0xF4, 0x50, 0x00]);
        let (_, proxy_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&proxy_rt), U256::ZERO, 1);

        let result = call_contract(&mut evm, &deployer, &proxy_addr, vec![], U256::ZERO, 2, 500_000);
        assert_eq!(result.receipt.status, 1);

        // slot 0 in proxy should hold the original caller (deployer)
        let slot = ShellHash::ZERO;
        let stored = evm.state_db_mut().world_state_mut()
            .get_storage(&proxy_addr, &slot).unwrap();
        let mut expected = [0u8; 32];
        expected[12..32].copy_from_slice(deployer.as_bytes());
        assert_eq!(stored.as_bytes(), &expected, "msg.sender should be preserved");
    }

    #[test]
    fn delegatecall_return_data_forwarded() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Logic: PUSH1 0xBE PUSH1 0 MSTORE PUSH1 1 PUSH1 31 RETURN
        let logic_rt = vec![0x60, 0xBE, 0x60, 0x00, 0x52, 0x60, 0x01, 0x60, 0x1f, 0xf3];
        let (_, logic_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&logic_rt), U256::ZERO, 0);

        // Proxy: DELEGATECALL → RETURNDATASIZE → RETURNDATACOPY → RETURN
        let mut proxy_rt = vec![
            0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00,
            0x73,
        ];
        proxy_rt.extend_from_slice(logic_addr.as_bytes());
        proxy_rt.extend_from_slice(&[
            0x5A, 0xF4, 0x50,       // DELEGATECALL, POP success
            0x3D,                   // RETURNDATASIZE
            0x60, 0x00, 0x60, 0x00, // offset=0, destOffset=0
            0x3E,                   // RETURNDATACOPY
            0x3D,                   // RETURNDATASIZE
            0x60, 0x00,             // offset=0
            0xF3,                   // RETURN
        ]);
        let (_, proxy_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&proxy_rt), U256::ZERO, 1);

        let result = call_contract(&mut evm, &deployer, &proxy_addr, vec![], U256::ZERO, 2, 500_000);
        assert_eq!(result.receipt.status, 1);
        assert_eq!(result.output, vec![0xBE], "should forward return data");
    }

    // ════════════════════════════════════════════════════════════
    //  Call depth limit test
    // ════════════════════════════════════════════════════════════

    #[test]
    fn call_depth_limit_1024() {
        // Contract recursively CALLs itself; EVM depth limit = 1024.
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Runtime: CALL(gas, self, 0, 0, 0, 0, 0) → store result → RETURN
        let runtime = vec![
            0x60, 0x00, // retSize
            0x60, 0x00, // retOffset
            0x60, 0x00, // argsSize
            0x60, 0x00, // argsOffset
            0x60, 0x00, // value
            0x30,       // ADDRESS (self)
            0x5A,       // GAS
            0xF1,       // CALL
            0x60, 0x00, 0x52, // MSTORE result
            0x60, 0x20, 0x60, 0x00, 0xF3, // RETURN 32 bytes
        ];
        let (_, contract_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&runtime), U256::ZERO, 0);

        let result = call_contract(
            &mut evm, &deployer, &contract_addr, vec![], U256::ZERO, 1, 30_000_000,
        );
        // Outer call succeeds; deep recursion eventually hits depth limit
        assert_eq!(result.receipt.status, 1, "outer call should succeed");
        assert_eq!(result.output.len(), 32);
    }

    // ════════════════════════════════════════════════════════════
    //  Code size limit tests (EIP-170)
    // ════════════════════════════════════════════════════════════

    #[test]
    fn code_size_over_24kb_fails() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // 24577 bytes of STOP opcodes — 1 byte over limit
        let oversized = vec![0x00u8; 24577];
        let init_code = make_init_code(&oversized);

        let tx = Transaction {
            chain_id: 1337, nonce: 0, to: None, value: U256::ZERO,
            data: shell_primitives::Bytes::from(init_code),
            gas_limit: 29_000_000, max_fee_per_gas: 0, max_priority_fee_per_gas: 0,
        };
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xCC; 100]);
        let signed = SignedTransaction::new(deployer, tx, sig);
        let result = evm.execute_tx(&signed, &sample_header(), 0, 0).unwrap();

        assert_eq!(result.receipt.status, 0, "deploying >24KB should fail");
        assert!(result.receipt.contract_address.is_none());
    }

    #[test]
    fn code_size_exactly_24kb_succeeds() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        let exact = vec![0x00u8; 24576];
        let init_code = make_init_code(&exact);

        let tx = Transaction {
            chain_id: 1337, nonce: 0, to: None, value: U256::ZERO,
            data: shell_primitives::Bytes::from(init_code),
            gas_limit: 29_000_000, max_fee_per_gas: 0, max_priority_fee_per_gas: 0,
        };
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xCC; 100]);
        let signed = SignedTransaction::new(deployer, tx, sig);
        let result = evm.execute_tx(&signed, &sample_header(), 0, 0).unwrap();

        assert_eq!(result.receipt.status, 1, "deploying exactly 24KB should succeed");
        assert!(result.receipt.contract_address.is_some());
    }

    // ════════════════════════════════════════════════════════════
    //  Gas limit tests
    // ════════════════════════════════════════════════════════════

    #[test]
    fn gas_exact_for_simple_transfer() {
        let mut evm = setup_evm();
        let from = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &from, U256::from(10_000_000_000u64));

        let tx = Transaction {
            chain_id: 1337, nonce: 0,
            to: Some(ShellAddress::from([0x01; 20])),
            value: U256::from(100),
            data: shell_primitives::Bytes::new(),
            gas_limit: 21_000, max_fee_per_gas: 0, max_priority_fee_per_gas: 0,
        };
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xAA; 100]);
        let signed = SignedTransaction::new(from, tx, sig);

        let result = evm.execute_tx(&signed, &sample_header(), 0, 0).unwrap();
        assert_eq!(result.receipt.status, 1);
        assert_eq!(result.gas_used, 21_000);
    }

    #[test]
    fn gas_insufficient_for_sstore_reverts() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Contract: PUSH1 1 PUSH1 0 SSTORE STOP
        let runtime = vec![0x60, 0x01, 0x60, 0x00, 0x55, 0x00];
        let (_, addr) = deploy_contract(&mut evm, &deployer, make_init_code(&runtime), U256::ZERO, 0);

        // Call with barely enough for intrinsic gas but not for SSTORE
        let tx = Transaction {
            chain_id: 1337, nonce: 1,
            to: Some(addr), value: U256::ZERO,
            data: shell_primitives::Bytes::new(),
            gas_limit: 21_100, max_fee_per_gas: 0, max_priority_fee_per_gas: 0,
        };
        let sig = PQSignature::new(SignatureType::Dilithium3, vec![0xDD; 100]);
        let signed = SignedTransaction::new(deployer, tx, sig);

        let result = evm.execute_tx(&signed, &sample_header(), 0, 0).unwrap();
        assert_eq!(result.receipt.status, 0, "should revert on insufficient gas");
    }

    #[test]
    fn gas_refund_from_clearing_storage() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Contract: SSTORE(0, calldataload(0)) STOP
        let runtime = vec![
            0x60, 0x00, 0x35, // PUSH1 0, CALLDATALOAD
            0x60, 0x00, 0x55, // PUSH1 0, SSTORE
            0x00,             // STOP
        ];
        let (_, addr) = deploy_contract(&mut evm, &deployer, make_init_code(&runtime), U256::ZERO, 0);

        // Set storage to non-zero
        let mut set_data = [0u8; 32];
        set_data[31] = 0x01;
        let r1 = call_contract(&mut evm, &deployer, &addr, set_data.to_vec(), U256::ZERO, 1, 500_000);
        assert_eq!(r1.receipt.status, 1);
        let gas_set = r1.gas_used;

        // Clear storage to zero (earns refund)
        let r2 = call_contract(&mut evm, &deployer, &addr, vec![0u8; 32], U256::ZERO, 2, 500_000);
        assert_eq!(r2.receipt.status, 1);
        let gas_clear = r2.gas_used;

        assert!(gas_clear < gas_set,
            "clearing storage (gas={gas_clear}) should cost less than setting (gas={gas_set})");
    }

    // ════════════════════════════════════════════════════════════
    //  Additional EVM operation tests
    // ════════════════════════════════════════════════════════════

    #[test]
    fn contract_to_contract_call() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Callee: returns 0xFF in a 32-byte word
        let callee_rt = vec![0x60, 0xFF, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xf3];
        let (_, callee_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&callee_rt), U256::ZERO, 0);

        // Caller: CALL(gas, callee, 0, 0, 0, 0, 32) → RETURN mem[0..32]
        let mut caller_rt = vec![
            0x60, 0x20, 0x60, 0x00, // retSize=32, retOff=0
            0x60, 0x00, 0x60, 0x00, // argsSz=0, argsOff=0
            0x60, 0x00,             // value=0
            0x73,
        ];
        caller_rt.extend_from_slice(callee_addr.as_bytes());
        caller_rt.extend_from_slice(&[
            0x5A, 0xF1, 0x50,       // GAS, CALL, POP
            0x60, 0x20, 0x60, 0x00, 0xF3, // RETURN 32 bytes
        ]);
        let (_, caller_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&caller_rt), U256::ZERO, 1);

        let result = call_contract(&mut evm, &deployer, &caller_addr, vec![], U256::ZERO, 2, 500_000);
        assert_eq!(result.receipt.status, 1);
        assert_eq!(result.output.len(), 32);
        assert_eq!(result.output[31], 0xFF);
    }

    #[test]
    fn revert_preserves_revert_data() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Runtime: PUSH4 0xDEADBEEF PUSH1 0 MSTORE PUSH1 4 PUSH1 28 REVERT
        let runtime = vec![
            0x63, 0xDE, 0xAD, 0xBE, 0xEF, // PUSH4
            0x60, 0x00, 0x52,               // MSTORE
            0x60, 0x04, 0x60, 0x1c, 0xFD,   // PUSH1 4, PUSH1 28, REVERT
        ];
        let (_, addr) = deploy_contract(&mut evm, &deployer, make_init_code(&runtime), U256::ZERO, 0);

        let result = call_contract(&mut evm, &deployer, &addr, vec![], U256::ZERO, 1, 100_000);
        assert_eq!(result.receipt.status, 0, "should revert");
        assert_eq!(&result.output, &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn create_opcode_basic() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Child init: returns 1-byte runtime 0xBB
        let child_init: Vec<u8> = vec![
            0x60, 0xBB, 0x60, 0x00, 0x52, 0x60, 0x01, 0x60, 0x1f, 0xf3,
        ];
        let mut factory_rt = Vec::new();
        factory_rt.push(0x69); // PUSH10
        factory_rt.extend_from_slice(&child_init);
        factory_rt.extend_from_slice(&[
            0x60, 0x00, 0x52,       // MSTORE
            0x60, 0x0a,             // PUSH1 10 (size)
            0x60, 0x16,             // PUSH1 22 (offset = 32-10)
            0x60, 0x00,             // PUSH1 0 (value)
            0xF0,                   // CREATE
            0x60, 0x00, 0x52,       // MSTORE
            0x60, 0x20, 0x60, 0x00, 0xf3,
        ]);
        let (_, factory_addr) = deploy_contract(&mut evm, &deployer, make_init_code(&factory_rt), U256::ZERO, 0);

        let result = call_contract(&mut evm, &deployer, &factory_addr, vec![], U256::ZERO, 1, 5_000_000);
        assert_eq!(result.receipt.status, 1);
        assert_ne!(&result.output[12..32], &[0u8; 20], "CREATE should return non-zero address");
    }

    #[test]
    fn sstore_sload_roundtrip() {
        let mut evm = setup_evm();
        let deployer = ShellAddress::from([0x42; 20]);
        fund_account(&mut evm, &deployer, U256::from(100_000_000_000u64));

        // Runtime: SSTORE(0, calldataload(0)), SLOAD(0), MSTORE, RETURN 32
        let runtime = vec![
            0x60, 0x00, 0x35,       // CALLDATALOAD(0)
            0x60, 0x00, 0x55,       // SSTORE(0, ...)
            0x60, 0x00, 0x54,       // SLOAD(0)
            0x60, 0x00, 0x52,       // MSTORE
            0x60, 0x20, 0x60, 0x00, 0xF3, // RETURN
        ];
        let (_, addr) = deploy_contract(&mut evm, &deployer, make_init_code(&runtime), U256::ZERO, 0);

        let mut calldata = [0u8; 32];
        calldata[30] = 0x12;
        calldata[31] = 0x34;
        let result = call_contract(&mut evm, &deployer, &addr, calldata.to_vec(), U256::ZERO, 1, 500_000);
        assert_eq!(result.receipt.status, 1);
        assert_eq!(result.output.len(), 32);
        assert_eq!(result.output[30], 0x12);
        assert_eq!(result.output[31], 0x34);
    }
}
