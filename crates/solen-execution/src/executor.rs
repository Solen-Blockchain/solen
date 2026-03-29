//! Block executor: validates and applies user operations to state.

use solen_crypto::blake3_hash;
use solen_storage::StateStore;
use solen_types::account::AuthMethod;
use solen_types::transaction::{Action, UserOperation};
use solen_types::AccountId;
use thiserror::Error;
use tracing::{debug, warn};

use crate::fees::FeeConfig;
use crate::receipt::{BlockResult, Event, ExecutionReceipt};
use crate::state::{StateError, StateManager};

const TRANSFER_GAS: u64 = 100;
const CALL_BASE_GAS: u64 = 500;
const DEPLOY_BASE_GAS: u64 = 1000;

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("state error: {0}")]
    State(#[from] StateError),
    #[error("signature verification failed")]
    InvalidSignature,
    #[error("account not found: {0}")]
    AccountNotFound(String),
    #[error("insufficient balance for fee: have {have}, need {need}")]
    InsufficientFee { have: u128, need: u128 },
    #[error("vm error: {0}")]
    VmError(#[from] solen_vm::VmError),
}

/// Executes blocks of user operations against the state store.
pub struct BlockExecutor {
    fee_config: FeeConfig,
    vm_runtime: solen_vm::runtime::VmRuntime,
}

impl BlockExecutor {
    pub fn new() -> Self {
        Self {
            fee_config: FeeConfig::default(),
            vm_runtime: solen_vm::runtime::VmRuntime::new().expect("failed to create VM runtime"),
        }
    }

    pub fn with_fee_config(fee_config: FeeConfig) -> Self {
        Self {
            fee_config,
            vm_runtime: solen_vm::runtime::VmRuntime::new().expect("failed to create VM runtime"),
        }
    }

    /// Execute a batch of user operations, returning the block result.
    ///
    /// Each operation is executed independently — a failure in one does not
    /// abort the block. Failed operations still consume gas and produce a
    /// receipt with `success: false`.
    ///
    /// Uses parallel execution for operations from different senders.
    pub fn execute_block(
        &self,
        store: &mut dyn StateStore,
        operations: &[UserOperation],
    ) -> BlockResult {
        // For large batches, pre-verify signatures in parallel (the most
        // expensive per-op cost) then execute state changes sequentially.
        if operations.len() >= 200 {
            return self.execute_block_parallel(store, operations);
        }

        self.execute_block_sequential(store, operations)
    }

    /// Sequential execution (original path).
    fn execute_block_sequential(
        &self,
        store: &mut dyn StateStore,
        operations: &[UserOperation],
    ) -> BlockResult {
        let mut receipts = Vec::with_capacity(operations.len());
        let mut total_gas = 0u64;

        for op in operations {
            let receipt = self.execute_operation(store, op);
            total_gas += receipt.gas_used;
            receipts.push(receipt);
        }

        let root = store.state_root();
        store.commit_root();

        BlockResult {
            state_root: root,
            receipts,
            gas_used: total_gas,
        }
    }

    /// Parallel execution: pre-validate signatures in parallel (the most
    /// expensive per-op work), then execute state changes sequentially.
    fn execute_block_parallel(
        &self,
        store: &mut dyn StateStore,
        operations: &[UserOperation],
    ) -> BlockResult {
        use rayon::prelude::*;

        // Phase 1: pre-compute signing messages and load accounts (sequential,
        // but fast — just reads + hashing).
        let pre: Vec<(Vec<u8>, Option<Vec<solen_types::account::AuthMethod>>)> = operations
            .iter()
            .map(|op| {
                let msg = self.operation_signing_message(op);
                let auth = store
                    .get(&{
                        let mut k = b"acc/".to_vec();
                        k.extend_from_slice(&op.sender);
                        k
                    })
                    .ok()
                    .flatten()
                    .and_then(|data| {
                        <solen_types::account::Account as borsh::BorshDeserialize>::try_from_slice(&data)
                            .ok()
                            .map(|a| a.auth_methods)
                    });
                (msg, auth)
            })
            .collect();

        // Phase 2: parallel Ed25519 signature verification.
        let validations: Vec<bool> = operations
            .par_iter()
            .zip(pre.par_iter())
            .map(|(op, (msg, auth))| {
                let auth_methods = match auth {
                    Some(methods) => methods,
                    None => return false,
                };
                if auth_methods.is_empty() {
                    return true;
                }
                auth_methods.iter().any(|method| match method {
                    solen_types::account::AuthMethod::Ed25519 { public_key } => {
                        if op.signature.len() != 64 {
                            return false;
                        }
                        let mut sig = [0u8; 64];
                        sig.copy_from_slice(&op.signature);
                        solen_crypto::verify(public_key, msg, &sig).is_ok()
                    }
                    _ => false,
                })
            })
            .collect();

        // Phase 2: sequential state application, skipping ops that failed validation.
        let mut receipts = Vec::with_capacity(operations.len());
        let mut total_gas = 0u64;

        for (i, op) in operations.iter().enumerate() {
            if !validations[i] {
                receipts.push(ExecutionReceipt {
                    sender: op.sender,
                    nonce: op.nonce,
                    success: false,
                    gas_used: 0,
                    error: Some("signature verification failed".into()),
                    events: vec![],
                });
                continue;
            }

            let receipt = self.execute_operation(store, op);
            total_gas += receipt.gas_used;
            receipts.push(receipt);
        }

        let root = store.state_root();
        store.commit_root();

        BlockResult {
            state_root: root,
            receipts,
            gas_used: total_gas,
        }
    }


    /// Execute a single user operation.
    fn execute_operation(
        &self,
        store: &mut dyn StateStore,
        op: &UserOperation,
    ) -> ExecutionReceipt {
        let mut events = Vec::new();
        let mut gas_used = 0u64;

        // Validate signature against the account's auth methods.
        let mut state = StateManager::new(store);
        if let Err(e) = self.validate_and_prepare(&mut state, op) {
            warn!(sender = ?op.sender[..4], error = %e, "operation validation failed");
            return ExecutionReceipt {
                sender: op.sender,
                nonce: op.nonce,
                success: false,
                gas_used: 0,
                error: Some(e.to_string()),
                events: vec![],
            };
        }
        drop(state);

        // Execute each action.
        for action in &op.actions {
            let mut state = StateManager::new(store);
            match self.execute_action(&mut state, &op.sender, action, &mut events) {
                Ok(gas) => gas_used += gas,
                Err(e) => {
                    warn!(sender = ?op.sender[..4], error = %e, "action execution failed");
                    return ExecutionReceipt {
                        sender: op.sender,
                        nonce: op.nonce,
                        success: false,
                        gas_used,
                        error: Some(e.to_string()),
                        events,
                    };
                }
            }
        }

        // Deduct fees from sender, credit treasury.
        let total_fee = self.fee_config.calculate_fee(gas_used);
        if total_fee > 0 {
            let mut state = StateManager::new(store);
            if let Ok(Some(mut sender_acct)) = state.get_account(&op.sender) {
                let actual_fee = total_fee.min(sender_acct.balance);
                sender_acct.balance -= actual_fee;
                let _ = state.save_account(&sender_acct);

                // Credit treasury (non-burned portion).
                let treasury_share = self.fee_config.treasury_amount(actual_fee);
                if treasury_share > 0 {
                    if let Ok(Some(mut treasury)) =
                        state.get_account(&self.fee_config.treasury_account)
                    {
                        treasury.balance += treasury_share;
                        let _ = state.save_account(&treasury);
                    }
                }

                events.push(Event {
                    emitter: op.sender,
                    topic: b"fee".to_vec(),
                    data: actual_fee.to_le_bytes().to_vec(),
                });
            }
        }

        debug!(sender = ?op.sender[..4], gas_used, "operation executed successfully");

        ExecutionReceipt {
            sender: op.sender,
            nonce: op.nonce,
            success: true,
            gas_used,
            error: None,
            events,
        }
    }

    /// Validate the operation: check account exists, verify nonce, verify signature.
    fn validate_and_prepare(
        &self,
        state: &mut StateManager<'_>,
        op: &UserOperation,
    ) -> Result<(), ExecutionError> {
        let account = state
            .get_account(&op.sender)?
            .ok_or_else(|| ExecutionError::AccountNotFound(format!("{:?}", &op.sender[..4])))?;

        // Verify signature against one of the account's auth methods.
        let sig_valid = account.auth_methods.iter().any(|method| match method {
            AuthMethod::Ed25519 { public_key } => {
                let msg = self.operation_signing_message(op);
                if op.signature.len() != 64 {
                    return false;
                }
                let mut sig = [0u8; 64];
                sig.copy_from_slice(&op.signature);
                solen_crypto::verify(public_key, &msg, &sig).is_ok()
            }
            // Other auth methods not yet implemented.
            _ => false,
        });

        if !sig_valid && !account.auth_methods.is_empty() {
            return Err(ExecutionError::InvalidSignature);
        }

        // Consume nonce.
        state.consume_nonce(&op.sender, op.nonce)?;

        Ok(())
    }

    /// Compute the message that must be signed for an operation.
    pub fn operation_signing_message(&self, op: &UserOperation) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(&op.sender);
        msg.extend_from_slice(&op.nonce.to_le_bytes());
        msg.extend_from_slice(&op.max_fee.to_le_bytes());
        // Hash the actions to keep the signing message compact.
        let actions_bytes =
            serde_json::to_vec(&op.actions).unwrap_or_default();
        msg.extend_from_slice(&blake3_hash(&actions_bytes));
        msg
    }

    /// Execute a single action within an operation.
    fn execute_action(
        &self,
        state: &mut StateManager<'_>,
        sender: &AccountId,
        action: &Action,
        events: &mut Vec<Event>,
    ) -> Result<u64, ExecutionError> {
        match action {
            Action::Transfer { to, amount } => {
                state.transfer(sender, to, *amount)?;
                events.push(Event {
                    emitter: *sender,
                    topic: b"transfer".to_vec(),
                    data: amount.to_le_bytes().to_vec(),
                });
                Ok(TRANSFER_GAS)
            }
            Action::Call {
                target,
                method,
                args,
            } => {
                let target_account = state.require_account(target)?;

                // If the account has no code, it's not a contract.
                let zero_hash = [0u8; 32];
                if target_account.code_hash == zero_hash {
                    events.push(Event {
                        emitter: *target,
                        topic: format!("call:{method}").into_bytes(),
                        data: args.clone(),
                    });
                    return Ok(CALL_BASE_GAS);
                }

                // Load the contract bytecode.
                let bytecode = state
                    .load_bytecode(&target_account.code_hash)?
                    .ok_or_else(|| {
                        ExecutionError::State(StateError::AccountNotFound(
                            "bytecode not found".into(),
                        ))
                    })?;

                // Load contract storage.
                let contract_storage = state.load_contract_storage(target)?;

                // Build input: method name + args.
                let mut input = Vec::new();
                input.extend_from_slice(method.as_bytes());
                input.push(0); // null separator
                input.extend_from_slice(args);

                // Execute in the VM.
                let ctx = solen_vm::host::HostContext::new(*sender, 0)
                    .with_storage(contract_storage);

                match self.vm_runtime.execute(&target_account.code_hash, &bytecode, &input, ctx, None) {
                    Ok(result) => {
                        // Persist updated contract storage.
                        state.save_contract_storage(target, &result.storage)?;

                        // Convert VM events to execution events.
                        for vm_event in &result.events {
                            events.push(Event {
                                emitter: *target,
                                topic: vm_event.topic.clone(),
                                data: vm_event.data.clone(),
                            });
                        }

                        Ok(CALL_BASE_GAS + result.gas_used)
                    }
                    Err(solen_vm::VmError::OutOfGas) => {
                        Err(ExecutionError::State(StateError::AccountNotFound(
                            "out of gas".into(),
                        )))
                    }
                    Err(e) => Err(ExecutionError::VmError(e)),
                }
            }
            Action::Deploy { code, salt } => {
                // Store the bytecode.
                let code_hash = state.store_bytecode(code)?;

                // Derive account ID from sender + salt + code hash.
                let mut preimage = Vec::new();
                preimage.extend_from_slice(sender);
                preimage.extend_from_slice(salt);
                preimage.extend_from_slice(&code_hash);
                let new_id: AccountId = blake3_hash(&preimage);

                let mut account = solen_types::account::Account {
                    id: new_id,
                    code_hash,
                    auth_methods: vec![],
                    nonce: 0,
                    balance: 0,
                };
                if let Ok(Some(sender_acct)) = state.get_account(sender) {
                    account.auth_methods = sender_acct.auth_methods.clone();
                }
                state.save_account(&account)?;

                events.push(Event {
                    emitter: *sender,
                    topic: b"deploy".to_vec(),
                    data: new_id.to_vec(),
                });
                Ok(DEPLOY_BASE_GAS)
            }
        }
    }

    /// Simulate an operation without modifying state. Returns the receipt
    /// that would result from execution.
    pub fn simulate(
        &self,
        store: &dyn StateStore,
        op: &UserOperation,
    ) -> ExecutionReceipt {
        let mut snapshot = store.snapshot();
        self.execute_operation(snapshot.as_mut(), op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_crypto::Keypair;
    use solen_storage::MemoryStore;
    use crate::genesis::{apply_genesis, GenesisAccount};

    fn treasury_id() -> AccountId {
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(b"treasury");
        id
    }

    fn setup() -> (MemoryStore, Keypair, AccountId, AccountId) {
        let mut store = MemoryStore::new();
        let kp = Keypair::generate();

        let alice_id = {
            let mut id = [0u8; 32];
            id[..4].copy_from_slice(b"alic");
            id
        };
        let bob_id = {
            let mut id = [0u8; 32];
            id[..3].copy_from_slice(b"bob");
            id
        };

        apply_genesis(
            &mut store,
            vec![
                GenesisAccount {
                    id: alice_id,
                    balance: 10_000,
                    auth_methods: vec![AuthMethod::Ed25519 {
                        public_key: kp.public_key(),
                    }],
                },
                GenesisAccount {
                    id: bob_id,
                    balance: 500,
                    auth_methods: vec![],
                },
                GenesisAccount {
                    id: treasury_id(),
                    balance: 0,
                    auth_methods: vec![],
                },
            ],
        )
        .unwrap();

        (store, kp, alice_id, bob_id)
    }

    /// Executor with zero fees for backward-compatible tests.
    fn zero_fee_executor() -> BlockExecutor {
        BlockExecutor::with_fee_config(FeeConfig {
            base_fee_per_gas: 0,
            ..Default::default()
        })
    }

    fn sign_op(kp: &Keypair, executor: &BlockExecutor, op: &mut UserOperation) {
        let msg = executor.operation_signing_message(op);
        let sig = kp.sign(&msg);
        op.signature = sig.to_vec();
    }

    #[test]
    fn simple_transfer() {
        let (mut store, kp, alice, bob) = setup();
        let executor = zero_fee_executor();

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer {
                to: bob,
                amount: 200,
            }],
            max_fee: 1000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let result = executor.execute_block(&mut store, &[op]);
        assert_eq!(result.receipts.len(), 1);
        assert!(result.receipts[0].success, "receipt: {:?}", result.receipts[0]);
        assert_eq!(result.gas_used, TRANSFER_GAS);

        let state = StateManager::new(&mut store);
        assert_eq!(state.get_balance(&alice).unwrap(), 9_800);
        assert_eq!(state.get_balance(&bob).unwrap(), 700);
    }

    #[test]
    fn invalid_signature_rejected() {
        let (mut store, _kp, alice, bob) = setup();
        let executor = zero_fee_executor();

        let op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer {
                to: bob,
                amount: 100,
            }],
            max_fee: 1000,
            signature: vec![0u8; 64], // wrong signature
        };

        let result = executor.execute_block(&mut store, &[op]);
        assert!(!result.receipts[0].success);
        assert!(result.receipts[0].error.as_ref().unwrap().contains("signature"));
    }

    #[test]
    fn nonce_mismatch_rejected() {
        let (mut store, kp, alice, bob) = setup();
        let executor = zero_fee_executor();

        let mut op = UserOperation {
            sender: alice,
            nonce: 5, // wrong nonce
            actions: vec![Action::Transfer {
                to: bob,
                amount: 100,
            }],
            max_fee: 1000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let result = executor.execute_block(&mut store, &[op]);
        assert!(!result.receipts[0].success);
        assert!(result.receipts[0].error.as_ref().unwrap().contains("nonce"));
    }

    #[test]
    fn insufficient_balance_rejected() {
        let (mut store, kp, alice, bob) = setup();
        let executor = zero_fee_executor();

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer {
                to: bob,
                amount: 999_999,
            }],
            max_fee: 1000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let result = executor.execute_block(&mut store, &[op]);
        assert!(!result.receipts[0].success);
        assert!(result.receipts[0].error.as_ref().unwrap().contains("balance"));
    }

    #[test]
    fn deploy_creates_account() {
        let (mut store, kp, alice, _bob) = setup();
        let executor = zero_fee_executor();

        let code = b"contract bytecode placeholder";
        let salt = [42u8; 32];

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Deploy {
                code: code.to_vec(),
                salt,
            }],
            max_fee: 5000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let result = executor.execute_block(&mut store, &[op]);
        assert!(result.receipts[0].success, "{:?}", result.receipts[0]);

        // The deployed account ID is in the event data.
        let deploy_event = &result.receipts[0].events[0];
        assert_eq!(deploy_event.topic, b"deploy");
        let mut deployed_id = [0u8; 32];
        deployed_id.copy_from_slice(&deploy_event.data);

        let state = StateManager::new(&mut store);
        let deployed = state.get_account(&deployed_id).unwrap().unwrap();
        assert_eq!(deployed.code_hash, blake3_hash(code));
    }

    #[test]
    fn multiple_ops_in_block() {
        let (mut store, kp, alice, bob) = setup();
        let executor = zero_fee_executor();

        let mut op1 = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob, amount: 100 }],
            max_fee: 1000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op1);

        let mut op2 = UserOperation {
            sender: alice,
            nonce: 1,
            actions: vec![Action::Transfer { to: bob, amount: 200 }],
            max_fee: 1000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op2);

        let result = executor.execute_block(&mut store, &[op1, op2]);
        assert_eq!(result.receipts.len(), 2);
        assert!(result.receipts[0].success);
        assert!(result.receipts[1].success);

        let state = StateManager::new(&mut store);
        assert_eq!(state.get_balance(&alice).unwrap(), 9_700);
        assert_eq!(state.get_balance(&bob).unwrap(), 800);
    }

    /// WAT contract that increments a counter in storage and emits an event.
    const COUNTER_WAT: &str = r#"
    (module
        (import "env" "storage_read" (func $storage_read (param i32 i32 i32) (result i32)))
        (import "env" "storage_write" (func $storage_write (param i32 i32 i32 i32)))
        (import "env" "emit_event" (func $emit_event (param i32 i32 i32 i32)))
        (import "env" "get_caller" (func $get_caller (param i32)))
        (import "env" "get_block_height" (func $get_block_height (result i64)))
        (import "env" "set_return_data" (func $set_return_data (param i32 i32)))
        (memory (export "memory") 1)
        (data (i32.const 0) "count")
        (data (i32.const 100) "incremented")
        (func (export "call") (param $input_ptr i32) (param $input_len i32) (result i32)
            (local $val i32)
            (drop (call $storage_read (i32.const 0) (i32.const 5) (i32.const 200)))
            (local.set $val (i32.load (i32.const 200)))
            (local.set $val (i32.add (local.get $val) (i32.const 1)))
            (i32.store (i32.const 200) (local.get $val))
            (call $storage_write (i32.const 0) (i32.const 5) (i32.const 200) (i32.const 4))
            (call $emit_event (i32.const 100) (i32.const 11) (i32.const 200) (i32.const 4))
            (call $set_return_data (i32.const 200) (i32.const 4))
            (i32.const 4)
        )
    )
    "#;

    #[test]
    fn deploy_and_call_wasm_contract() {
        let (mut store, kp, alice, _bob) = setup();
        let executor = zero_fee_executor();

        // Compile the WAT to WASM bytecode.
        let wasm = wat::parse_str(COUNTER_WAT).expect("WAT parse failed");
        let salt = [99u8; 32];

        // Deploy the contract.
        let mut deploy_op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Deploy {
                code: wasm.to_vec(),
                salt,
            }],
            max_fee: 50_000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut deploy_op);

        let result = executor.execute_block(&mut store, &[deploy_op]);
        assert!(result.receipts[0].success, "deploy failed: {:?}", result.receipts[0]);

        // Extract the deployed contract ID.
        let deploy_event = &result.receipts[0].events[0];
        let mut contract_id = [0u8; 32];
        contract_id.copy_from_slice(&deploy_event.data);

        // Call the contract (increment counter).
        let mut call_op = UserOperation {
            sender: alice,
            nonce: 1,
            actions: vec![Action::Call {
                target: contract_id,
                method: "increment".to_string(),
                args: vec![],
            }],
            max_fee: 100_000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut call_op);

        let result = executor.execute_block(&mut store, &[call_op]);
        assert!(result.receipts[0].success, "call failed: {:?}", result.receipts[0]);

        // Should have an "incremented" event from the contract.
        let events = &result.receipts[0].events;
        assert!(events.iter().any(|e| e.topic == b"incremented"));
        assert!(result.receipts[0].gas_used > CALL_BASE_GAS);

        // Call again — counter should be 2.
        let mut call_op2 = UserOperation {
            sender: alice,
            nonce: 2,
            actions: vec![Action::Call {
                target: contract_id,
                method: "increment".to_string(),
                args: vec![],
            }],
            max_fee: 100_000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut call_op2);

        let result2 = executor.execute_block(&mut store, &[call_op2]);
        assert!(result2.receipts[0].success, "call2 failed: {:?}", result2.receipts[0]);
    }

    #[test]
    fn simulate_does_not_modify_state() {
        let (mut store, kp, alice, bob) = setup();
        let executor = zero_fee_executor();

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob, amount: 500 }],
            max_fee: 1000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let receipt = executor.simulate(&store, &op);
        assert!(receipt.success);

        // State should be unchanged.
        let state = StateManager::new(&mut store);
        assert_eq!(state.get_balance(&alice).unwrap(), 10_000);
        assert_eq!(state.get_balance(&bob).unwrap(), 500);
    }

    #[test]
    fn fees_deducted_from_sender() {
        let (mut store, kp, alice, bob) = setup();
        let fee_config = FeeConfig {
            base_fee_per_gas: 10,
            burn_rate_bps: 0, // No burn, all to treasury.
            ..Default::default()
        };
        let executor = BlockExecutor::with_fee_config(fee_config);

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob, amount: 200 }],
            max_fee: 5000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let result = executor.execute_block(&mut store, &[op]);
        assert!(result.receipts[0].success);

        // Gas used = 100 (TRANSFER_GAS), fee = 100 * 10 = 1000
        let state = StateManager::new(&mut store);
        assert_eq!(state.get_balance(&alice).unwrap(), 10_000 - 200 - 1000);
        assert_eq!(state.get_balance(&bob).unwrap(), 700);
        assert_eq!(state.get_balance(&treasury_id()).unwrap(), 1000);
    }

    #[test]
    fn fees_with_burn() {
        let (mut store, kp, alice, bob) = setup();
        let fee_config = FeeConfig {
            base_fee_per_gas: 10,
            burn_rate_bps: 5000, // 50% burned.
            ..Default::default()
        };
        let executor = BlockExecutor::with_fee_config(fee_config);

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob, amount: 100 }],
            max_fee: 5000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let result = executor.execute_block(&mut store, &[op]);
        assert!(result.receipts[0].success);

        // Fee = 100 * 10 = 1000. 500 burned, 500 to treasury.
        let state = StateManager::new(&mut store);
        assert_eq!(state.get_balance(&alice).unwrap(), 10_000 - 100 - 1000);
        assert_eq!(state.get_balance(&treasury_id()).unwrap(), 500);
    }
}
