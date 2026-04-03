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
const SET_AUTH_GAS: u64 = 200;
const MAX_ACTIONS_PER_OP: usize = 16;
const MAX_CODE_SIZE: usize = 4 * 1024 * 1024; // 4 MB

/// Save account, logging on failure instead of silently discarding errors.
fn save_or_warn(state: &mut StateManager<'_>, account: &solen_types::account::Account) {
    if let Err(e) = state.save_account(account) {
        warn!(account = ?account.id[..4], error = %e, "failed to persist account state");
    }
}

/// Verify a signature against an auth method.
///
/// For `Ed25519`: expects a 64-byte signature.
/// For `Threshold`: expects concatenated (pubkey[32] + sig[64]) pairs.
/// At least `threshold` valid signatures from the signers list are required.
fn verify_auth(method: &AuthMethod, msg: &[u8], signature: &[u8]) -> bool {
    match method {
        AuthMethod::Ed25519 { public_key } => {
            if signature.len() != 64 {
                return false;
            }
            let mut sig = [0u8; 64];
            sig.copy_from_slice(signature);
            solen_crypto::verify(public_key, msg, &sig).is_ok()
        }
        AuthMethod::Threshold { signers, threshold } => {
            // Each sub-signature is pubkey[32] + sig[64] = 96 bytes.
            if signature.len() % 96 != 0 {
                return false;
            }
            let mut valid_count = 0u16;
            for chunk in signature.chunks_exact(96) {
                let mut pubkey = [0u8; 32];
                pubkey.copy_from_slice(&chunk[..32]);
                let mut sig = [0u8; 64];
                sig.copy_from_slice(&chunk[32..96]);

                // Only count if this pubkey is in the signers list.
                if signers.contains(&pubkey) && solen_crypto::verify(&pubkey, msg, &sig).is_ok() {
                    valid_count += 1;
                }
            }
            valid_count >= *threshold
        }
        AuthMethod::Passkey { public_key_x, public_key_y, .. } => {
            verify_passkey(public_key_x, public_key_y, msg, signature)
        }
        AuthMethod::Session { session_key, .. } => {
            // Session keys use Ed25519 signatures. Restriction checks
            // (expiry, spending, targets, methods) are done in execute_operation.
            if signature.len() != 64 {
                return false;
            }
            let mut sig = [0u8; 64];
            sig.copy_from_slice(signature);
            solen_crypto::verify(session_key, msg, &sig).is_ok()
        }
        AuthMethod::Guardian { .. } => false, // Guardians don't sign transactions.
    }
}

/// Verify a WebAuthn/Passkey P-256 (secp256r1) ECDSA signature.
///
/// Signature format:
///   auth_data_len[2 LE] + authenticatorData[N] + client_data_json_len[2 LE] + clientDataJSON[M] + r[32] + s[32]
///
/// Verification:
///   1. Extract challenge from clientDataJSON, verify it matches base64url(msg)
///   2. Compute signed_data = authenticatorData || SHA-256(clientDataJSON)
///   3. Verify P-256 ECDSA signature over SHA-256(signed_data)
fn verify_passkey(pk_x: &[u8; 32], pk_y: &[u8; 32], msg: &[u8], signature: &[u8]) -> bool {
    use p256::ecdsa::{signature::Verifier, Signature, VerifyingKey};
    use p256::EncodedPoint;
    use sha2::{Digest, Sha256};

    // Parse: auth_data_len[2] + auth_data + client_data_len[2] + client_data + r[32] + s[32]
    if signature.len() < 68 {
        return false; // minimum: 2 + 0 + 2 + 0 + 32 + 32
    }

    let auth_data_len = u16::from_le_bytes([signature[0], signature[1]]) as usize;
    if signature.len() < 2 + auth_data_len + 2 + 64 {
        return false;
    }
    let auth_data = &signature[2..2 + auth_data_len];

    let cd_offset = 2 + auth_data_len;
    let client_data_len = u16::from_le_bytes([signature[cd_offset], signature[cd_offset + 1]]) as usize;
    let sig_start = cd_offset + 2 + client_data_len;
    if signature.len() < sig_start + 64 {
        return false;
    }
    let client_data_json = &signature[cd_offset + 2..sig_start];

    let mut r_bytes = [0u8; 32];
    let mut s_bytes = [0u8; 32];
    r_bytes.copy_from_slice(&signature[sig_start..sig_start + 32]);
    s_bytes.copy_from_slice(&signature[sig_start + 32..sig_start + 64]);

    // 1. Verify challenge in clientDataJSON matches base64url(msg).
    // Parse clientDataJSON to extract challenge field.
    let client_data_str = match std::str::from_utf8(client_data_json) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // Extract challenge value (simple JSON parsing — look for "challenge":"...")
    let challenge_value = match extract_json_string(client_data_str, "challenge") {
        Some(c) => c,
        None => return false,
    };

    // The challenge should be base64url-encoded signing message.
    let expected_challenge = base64url_encode(msg);
    if challenge_value != expected_challenge {
        return false;
    }

    // 2. Verify authenticatorData flags (UP bit must be set).
    if auth_data.len() < 37 {
        return false;
    }
    let flags = auth_data[32];
    if flags & 0x01 == 0 {
        return false; // User Present flag not set.
    }

    // 3. Compute signed data = authenticatorData || SHA-256(clientDataJSON)
    let client_data_hash = Sha256::digest(client_data_json);
    let mut signed_data = Vec::with_capacity(auth_data.len() + 32);
    signed_data.extend_from_slice(auth_data);
    signed_data.extend_from_slice(&client_data_hash);

    // 4. Reconstruct P-256 public key and verify.
    let point = EncodedPoint::from_affine_coordinates(pk_x.into(), pk_y.into(), false);
    let verifying_key = match VerifyingKey::from_encoded_point(&point) {
        Ok(k) => k,
        Err(_) => return false,
    };

    let ecdsa_sig = match Signature::from_scalars(r_bytes, s_bytes) {
        Ok(s) => s,
        Err(_) => return false,
    };

    // P-256 ECDSA verify: the p256 crate internally hashes signed_data with SHA-256.
    verifying_key.verify(&signed_data, &ecdsa_sig).is_ok()
}

/// Extract a string value from a JSON object (simple parser, no dependencies).
fn extract_json_string<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{}\"", key);
    let key_pos = json.find(&pattern)?;
    let after_key = &json[key_pos + pattern.len()..];
    // Skip whitespace and colon
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_ws = after_colon.trim_start();
    // Expect opening quote
    let after_quote = after_ws.strip_prefix('"')?;
    let end = after_quote.find('"')?;
    Some(&after_quote[..end])
}

/// Base64url encoding without padding (RFC 4648 §5).
fn base64url_encode(data: &[u8]) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut result = String::with_capacity((data.len() * 4 + 2) / 3);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARSET[((n >> 18) & 0x3F) as usize] as char);
        result.push(CHARSET[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARSET[((n >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            result.push(CHARSET[(n & 0x3F) as usize] as char);
        }
    }
    result
}

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
    chain_id: u64,
}

impl BlockExecutor {
    pub fn new() -> Self {
        Self {
            fee_config: FeeConfig::default(),
            vm_runtime: solen_vm::runtime::VmRuntime::new().expect("failed to create VM runtime"),
            chain_id: 0,
        }
    }

    pub fn with_fee_config(fee_config: FeeConfig) -> Self {
        Self {
            fee_config,
            vm_runtime: solen_vm::runtime::VmRuntime::new().expect("failed to create VM runtime"),
            chain_id: 0,
        }
    }

    pub fn with_chain_id(mut self, chain_id: u64) -> Self {
        self.chain_id = chain_id;
        self
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
        self.execute_block_with_height(store, operations, 0)
    }

    /// Execute a block at a specific height. If the height is an epoch
    /// boundary, automatically distributes staking rewards as part of
    /// the block execution (deterministic — all nodes get same result).
    pub fn execute_block_with_height(
        &self,
        store: &mut dyn StateStore,
        operations: &[UserOperation],
        height: u64,
    ) -> BlockResult {
        let mut result = if operations.len() >= 200 {
            self.execute_block_parallel(store, operations)
        } else {
            self.execute_block_sequential(store, operations)
        };

        // Distribute epoch rewards deterministically as part of block execution.
        if height > 0 && height % 100 == 0 {
            let reward_receipts = distribute_epoch_rewards_in_executor(store, height);
            result.receipts.extend(reward_receipts);
            // Recompute state root after rewards.
            result.state_root = store.state_root();
            store.commit_root();
        }

        result
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

        // Phase 2: parallel signature verification (Ed25519 + Threshold).
        let validations: Vec<bool> = operations
            .par_iter()
            .zip(pre.par_iter())
            .map(|(op, (msg, auth))| {
                let auth_methods = match auth {
                    Some(methods) => methods,
                    None => return false,
                };
                if auth_methods.is_empty() {
                    return false; // no auth methods = reject (accounts must have auth)
                }
                auth_methods.iter().any(|method| verify_auth(method, msg, &op.signature))
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

        // Validate action count.
        if op.actions.len() > MAX_ACTIONS_PER_OP {
            return ExecutionReceipt {
                sender: op.sender,
                nonce: op.nonce,
                success: false,
                gas_used: 0,
                error: Some(format!("too many actions: {} (max {})", op.actions.len(), MAX_ACTIONS_PER_OP)),
                events: vec![],
            };
        }

        // Validate deploy code sizes.
        for action in &op.actions {
            if let Action::Deploy { code, .. } = action {
                if code.len() > MAX_CODE_SIZE {
                    return ExecutionReceipt {
                        sender: op.sender,
                        nonce: op.nonce,
                        success: false,
                        gas_used: 0,
                        error: Some(format!("contract too large: {} bytes (max {})", code.len(), MAX_CODE_SIZE)),
                        events: vec![],
                    };
                }
            }
        }

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

        let max_possible_fee = 0u128; // No upfront reservation.
        drop(state);

        // Execute each action. For multi-action operations, take a full store
        // snapshot so ALL state changes (including system contracts) can be rolled
        // back if any action fails.
        let store_snapshot = if op.actions.len() > 1 {
            Some(store.snapshot())
        } else {
            None
        };

        let mut action_failed = None;
        for action in &op.actions {
            // Check for system contract calls (need raw store access).
            if let Action::Call { target, method, args } = action {
                if solen_types::system::is_system_contract(target) {
                    let result = crate::system_calls::execute_system_call(
                        store, &op.sender, target, method, args,
                    );
                    gas_used += result.gas_used;
                    events.extend(result.events);
                    if let Some(err) = result.error {
                        action_failed = Some(err);
                        break;
                    }
                    continue;
                }
            }

            let mut state = StateManager::new(store);
            match self.execute_action(&mut state, &op.sender, action, &mut events) {
                Ok(gas) => gas_used += gas,
                Err(e) => {
                    action_failed = Some(e.to_string());
                    break;
                }
            }
        }

        // If any action failed, roll back ALL state changes from this operation.
        if let Some(err) = action_failed {
            if let Some(snapshot) = store_snapshot {
                // Restore entire store to pre-execution state.
                // Re-apply only the sender's nonce increment and fee deduction.
                let all_entries = snapshot.scan_all().unwrap_or_default();
                for (k, v) in &all_entries {
                    let _ = store.put(k, v);
                }
                // Deduct gas fee from the restored state.
                let actual_fee = self.fee_config.calculate_fee(gas_used);
                if actual_fee > 0 {
                    let mut state = StateManager::new(store);
                    if let Ok(mut acct) = state.require_account(&op.sender) {
                        acct.balance = acct.balance.saturating_sub(actual_fee);
                        let _ = state.save_account(&acct);
                    }
                }
            }
            events.clear(); // Clear events from failed actions.
            warn!(sender = ?op.sender[..4], error = %err, "action execution failed");
            return ExecutionReceipt {
                sender: op.sender,
                nonce: op.nonce,
                success: false,
                gas_used,
                error: Some(err),
                events: vec![], // discard events from failed operation
            };
        }

        // Settle fees: refund reserved amount, charge actual gas used.
        // Read burn rate from governance config (may have been changed by proposal).
        let mut fee_config = self.fee_config.clone();
        if let Ok(Some(data)) = store.get(b"__config_burn_rate__") {
            if data.len() >= 8 {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&data[..8]);
                fee_config.burn_rate_bps = u64::from_le_bytes(buf);
            }
        }
        let total_fee = fee_config.calculate_fee(gas_used);
        if max_possible_fee > 0 || total_fee > 0 {
            let mut state = StateManager::new(store);
            if let Ok(Some(mut sender_acct)) = state.get_account(&op.sender) {
                // Refund the reserved max fee, then deduct actual fee.
                sender_acct.balance = sender_acct.balance.saturating_add(max_possible_fee);
                sender_acct.balance = sender_acct.balance.saturating_sub(total_fee);
                save_or_warn(&mut state, &sender_acct);

                // Credit treasury (non-burned portion).
                let treasury_share = fee_config.treasury_amount(total_fee);
                if treasury_share > 0 {
                    if let Ok(Some(mut treasury)) =
                        state.get_account(&fee_config.treasury_account)
                    {
                        treasury.balance = treasury.balance.saturating_add(treasury_share);
                        save_or_warn(&mut state, &treasury);
                    }
                }

                events.push(Event {
                    emitter: op.sender,
                    topic: b"fee".to_vec(),
                    data: total_fee.to_le_bytes().to_vec(),
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

        // System-authorized intent operations (signature = [0xFF]) skip signature checks.
        // These are created by the block proposer for intent fulfillment.
        let is_intent_op = op.signature == [0xFF]
            && op.actions.len() == 1
            && matches!(&op.actions[0], Action::Call { target, method, .. }
                if *target == solen_types::system::INTENT_ADDRESS && method == "fulfill");

        let matched_session = if is_intent_op {
            // Skip signature and session key checks for intent operations.
            None
        } else {
            // Verify signature against one of the account's auth methods.
            let msg = self.operation_signing_message(op);
            let sig_valid = account.auth_methods.iter().any(|method| {
                verify_auth(method, &msg, &op.signature)
            });

            if !sig_valid {
                return Err(ExecutionError::InvalidSignature);
            }

            // If signed by a session key, enforce its restrictions.
            account.auth_methods.iter().find(|method| {
                if let AuthMethod::Session { session_key, .. } = method {
                    if op.signature.len() == 64 {
                        let mut sig = [0u8; 64];
                        sig.copy_from_slice(&op.signature);
                        return solen_crypto::verify(session_key, &msg, &sig).is_ok();
                    }
                }
                false
            })
        };

        if let Some(AuthMethod::Session {
            expires_at,
            spending_limit,
            allowed_targets,
            allowed_methods,
            ..
        }) = matched_session
        {
            // Check expiry (read current block height from chain meta).
            let current_height = state.current_height().unwrap_or(0);
            if current_height > *expires_at {
                return Err(ExecutionError::State(StateError::AccountNotFound(
                    "session key expired".into(),
                )));
            }

            // Check spending limit — counts all balance-affecting actions,
            // including system contract calls (staking, bridge deposits, etc.).
            if *spending_limit > 0 {
                let total_spend: u128 = op.actions.iter().map(|a| match a {
                    Action::Transfer { amount, .. } => *amount,
                    Action::Call { target, args, .. } => {
                        // System calls that deduct balance: delegate, deposit, register_rollup, etc.
                        // The amount is typically encoded in args as u128 at a known offset.
                        if solen_types::system::is_system_contract(target) && args.len() >= 48 {
                            // Most system calls: args contain amount at offset 32 as u128 LE.
                            // (staking: validator[32] + amount[16], bridge deposit: rollup_id[8] + amount[16])
                            let amount_offset = if target == &solen_types::system::BRIDGE_ADDRESS { 8 } else { 32 };
                            if args.len() >= amount_offset + 16 {
                                let mut buf = [0u8; 16];
                                buf.copy_from_slice(&args[amount_offset..amount_offset + 16]);
                                u128::from_le_bytes(buf)
                            } else {
                                0
                            }
                        } else {
                            0
                        }
                    }
                    Action::Deploy { .. } => 0,
                    _ => 0,
                }).sum();
                if total_spend > *spending_limit {
                    return Err(ExecutionError::State(StateError::AccountNotFound(
                        format!("session spending limit exceeded: {} > {}", total_spend, spending_limit),
                    )));
                }
            }

            // Check allowed targets.
            if !allowed_targets.is_empty() {
                for action in &op.actions {
                    let target = match action {
                        Action::Call { target, .. } => Some(target),
                        Action::Transfer { to, .. } => Some(to),
                        _ => None,
                    };
                    if let Some(t) = target {
                        if !allowed_targets.contains(t) {
                            return Err(ExecutionError::State(StateError::AccountNotFound(
                                "session key not authorized for this target".into(),
                            )));
                        }
                    }
                }
            }

            // Check allowed methods.
            if !allowed_methods.is_empty() {
                for action in &op.actions {
                    if let Action::Call { method, .. } = action {
                        if !allowed_methods.contains(method) {
                            return Err(ExecutionError::State(StateError::AccountNotFound(
                                format!("session key not authorized for method: {}", method),
                            )));
                        }
                    }
                }
            }
        }

        // Consume nonce (skip for intent-authorized ops).
        if !is_intent_op {
            state.consume_nonce(&op.sender, op.nonce)?;
        }

        Ok(())
    }

    /// Compute the message that must be signed for an operation.
    /// Format: chain_id[8] + sender[32] + nonce[8] + max_fee[16] + blake3(actions)[32]
    pub fn operation_signing_message(&self, op: &UserOperation) -> Vec<u8> {
        let mut msg = Vec::with_capacity(96);
        msg.extend_from_slice(&self.chain_id.to_le_bytes());
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
                // Emit event with recipient + amount in data.
                let mut data = Vec::with_capacity(32 + 16);
                data.extend_from_slice(to);
                data.extend_from_slice(&amount.to_le_bytes());
                events.push(Event {
                    emitter: *sender,
                    topic: b"transfer".to_vec(),
                    data,
                });
                Ok(TRANSFER_GAS)
            }
            Action::Call {
                target,
                method,
                args,
            } => {
                // System contract calls are handled at the operation level.
                // If we get here, it's a user contract call.
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
            Action::SetAuth { auth_methods } => {
                if auth_methods.is_empty() {
                    return Err(ExecutionError::State(StateError::AccountNotFound(
                        "auth_methods cannot be empty".into(),
                    )));
                }
                // Validate auth methods.
                for method in auth_methods {
                    match method {
                        AuthMethod::Threshold { signers, threshold } => {
                            if *threshold == 0 || *threshold as usize > signers.len() {
                                return Err(ExecutionError::State(StateError::AccountNotFound(
                                    format!("invalid threshold: {} of {} signers", threshold, signers.len()),
                                )));
                            }
                        }
                        AuthMethod::Guardian { guardian_id } => {
                            // Verify guardian account exists.
                            if state.get_account(guardian_id)?.is_none() {
                                return Err(ExecutionError::State(StateError::AccountNotFound(
                                    format!("guardian account does not exist: {:?}", &guardian_id[..4]),
                                )));
                            }
                        }
                        _ => {}
                    }
                }
                let mut account = state.require_account(sender)?;
                account.auth_methods = auth_methods.clone();
                state.save_account(&account)?;

                events.push(Event {
                    emitter: *sender,
                    topic: b"set_auth".to_vec(),
                    data: vec![auth_methods.len() as u8],
                });
                Ok(SET_AUTH_GAS)
            }
            Action::Deploy { code, salt } => {
                // Validate code size.
                if code.len() > MAX_CODE_SIZE {
                    return Err(ExecutionError::State(StateError::AccountNotFound(
                        format!("contract too large: {} bytes (max {})", code.len(), MAX_CODE_SIZE),
                    )));
                }

                // Store the bytecode.
                let code_hash = state.store_bytecode(code)?;

                // Derive account ID from sender + salt + code hash.
                let mut preimage = Vec::new();
                preimage.extend_from_slice(sender);
                preimage.extend_from_slice(salt);
                preimage.extend_from_slice(&code_hash);
                let new_id: AccountId = blake3_hash(&preimage);

                // Check for address collision.
                if let Ok(Some(existing)) = state.get_account(&new_id) {
                    if existing.code_hash != [0u8; 32] {
                        return Err(ExecutionError::State(StateError::AccountNotFound(
                            "contract address already exists".into(),
                        )));
                    }
                }

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
    /// that would result from execution. Uses a copy-on-write overlay
    /// instead of copying the entire database.
    pub fn simulate(
        &self,
        store: &dyn StateStore,
        op: &UserOperation,
    ) -> ExecutionReceipt {
        let mut overlay = solen_storage::OverlayStore::new(store);
        self.execute_operation(&mut overlay, op)
    }
}

/// Distribute epoch rewards deterministically as part of block execution.
/// This runs inside the executor so ALL nodes produce the same state root.
/// Only validators and delegators who were staked for the full epoch are eligible.
fn distribute_epoch_rewards_in_executor(
    store: &mut dyn StateStore,
    height: u64,
) -> Vec<ExecutionReceipt> {
    use borsh::BorshDeserialize;
    use solen_system_contracts::staking::StakingContract;
    use solen_types::system::STAKING_POOL_ADDRESS;

    let current_epoch = height / 100; // epoch length = 100 blocks

    // Read epoch reward from governance config, or use default (317 SOLEN).
    let reward_per_epoch: u128 = match store.get(b"__config_epoch_reward__") {
        Ok(Some(data)) if data.len() >= 16 => {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&data[..16]);
            u128::from_le_bytes(buf)
        }
        _ => 31_700_000_000, // 317 SOLEN (8 decimals)
    };

    let staking = StakingContract::load(store);

    // Only include validators eligible for this epoch's rewards.
    let active = staking.eligible_validators(current_epoch);
    let total_stake: u128 = active.iter().map(|v| v.total_stake()).sum();

    if total_stake == 0 || active.is_empty() {
        return vec![];
    }

    // Check staking pool balance.
    let pool_key = {
        let mut k = b"acc/".to_vec();
        k.extend_from_slice(&STAKING_POOL_ADDRESS);
        k
    };

    let pool_balance = match store.get(&pool_key) {
        Ok(Some(data)) => {
            solen_types::account::Account::try_from_slice(&data)
                .map(|a| a.balance)
                .unwrap_or(0)
        }
        _ => 0,
    };

    if pool_balance == 0 {
        return vec![];
    }

    let actual_reward = reward_per_epoch.min(pool_balance);

    // Deduct from pool.
    if let Ok(Some(data)) = store.get(&pool_key) {
        if let Ok(mut pool_acct) = solen_types::account::Account::try_from_slice(&data) {
            pool_acct.balance = pool_acct.balance.saturating_sub(actual_reward);
            if let Ok(encoded) = borsh::to_vec(&pool_acct) {
                if let Err(e) = store.put(&pool_key, &encoded) {
                    warn!(error = %e, "failed to persist staking pool deduction");
                }
            }
        }
    }

    // Distribute to validators and delegators.
    let mut events = Vec::new();

    for validator in &active {
        let v_total = validator.total_stake();
        if v_total == 0 {
            continue; // skip validators with zero stake
        }

        let validator_share = actual_reward.saturating_mul(v_total) / total_stake;
        if validator_share == 0 {
            continue;
        }

        // Only count eligible delegations for reward splitting.
        let eligible_delegations = staking.eligible_delegations_for_validator(&validator.id, current_epoch);
        let eligible_delegated: u128 = eligible_delegations.iter().map(|d| d.amount).sum();

        // Split rewards: eligible delegators get their proportional share,
        // validator gets the rest (self-stake share + ineligible delegation share + commission).
        let delegator_pool = if eligible_delegated > 0 && v_total > 0 && validator_share > 0 {
            validator_share.saturating_mul(eligible_delegated) / v_total
        } else {
            0
        };
        let commission = delegator_pool.saturating_mul(validator.commission_rate_bps as u128) / 10_000;
        let delegator_net = delegator_pool.saturating_sub(commission);
        let validator_reward = validator_share.saturating_sub(delegator_pool) + commission;

        // Credit validator.
        credit_account_raw(store, &validator.id, validator_reward);

        let mut event_data = Vec::with_capacity(48);
        event_data.extend_from_slice(&validator.id);
        event_data.extend_from_slice(&validator_reward.to_le_bytes());
        events.push(Event {
            emitter: STAKING_POOL_ADDRESS,
            topic: b"epoch_reward".to_vec(),
            data: event_data,
        });

        // Credit eligible delegators only.
        if delegator_net > 0 && eligible_delegated > 0 {
            for delegation in &eligible_delegations {
                let del_share = delegator_net.saturating_mul(delegation.amount) / eligible_delegated;
                if del_share == 0 {
                    continue;
                }
                credit_account_raw(store, &delegation.delegator, del_share);

                let mut event_data = Vec::with_capacity(48);
                event_data.extend_from_slice(&delegation.delegator);
                event_data.extend_from_slice(&del_share.to_le_bytes());
                events.push(Event {
                    emitter: STAKING_POOL_ADDRESS,
                    topic: b"delegator_reward".to_vec(),
                    data: event_data,
                });
            }
        }
    }

    if events.is_empty() {
        return vec![];
    }

    vec![ExecutionReceipt {
        sender: STAKING_POOL_ADDRESS,
        nonce: 0,
        success: true,
        gas_used: 0,
        error: None,
        events,
    }]
}

/// Credit an account balance directly in the store.
fn credit_account_raw(store: &mut dyn StateStore, id: &[u8; 32], amount: u128) {
    use borsh::BorshDeserialize;

    let key = {
        let mut k = b"acc/".to_vec();
        k.extend_from_slice(id);
        k
    };

    if let Ok(Some(data)) = store.get(&key) {
        if let Ok(mut account) = solen_types::account::Account::try_from_slice(&data) {
            account.balance = account.balance.saturating_add(amount);
            if let Ok(encoded) = borsh::to_vec(&account) {
                if let Err(e) = store.put(&key, &encoded) {
                    warn!(account = ?id[..4], error = %e, "failed to persist reward credit");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_crypto::Keypair;
    use solen_storage::MemoryStore;
    use crate::genesis::{apply_genesis, GenesisAccount};

    fn treasury_id() -> AccountId {
        crate::fees::TREASURY_ADDRESS
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
