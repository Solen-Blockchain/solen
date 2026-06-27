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

/// Reverse-delta for one block: the pre-commit value of every key the block
/// touched (`None` = the key did not exist). Applying it to the store undoes
/// exactly that block. Used by the rollback journal to rewind a shallow fork
/// to its common ancestor without a snapshot restore.
pub type BlockRevert = std::collections::HashMap<Vec<u8>, Option<Vec<u8>>>;

const TRANSFER_GAS: u64 = 100;
const ACCOUNT_CREATION_GAS: u64 = 25_000; // surcharge for auto-creating recipient
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
/// Returns the method name (e.g. "ed25519", "passkey", "session", "threshold")
/// if verification succeeds, None otherwise.
///
/// For `Ed25519`: expects a 64-byte signature.
/// For `Threshold`: expects concatenated (pubkey[32] + sig[64]) pairs.
/// At least `threshold` valid signatures from the signers list are required.
fn verify_auth(method: &AuthMethod, msg: &[u8], signature: &[u8]) -> Option<&'static str> {
    match method {
        AuthMethod::Ed25519 { public_key } => {
            if signature.len() != 64 {
                return None;
            }
            let mut sig = [0u8; 64];
            sig.copy_from_slice(signature);
            solen_crypto::verify(public_key, msg, &sig).ok().map(|_| "ed25519")
        }
        AuthMethod::Threshold { signers, threshold } => {
            // Reject invalid threshold (0 would accept empty signatures).
            if *threshold == 0 || signers.is_empty() {
                return None;
            }
            // Each sub-signature is pubkey[32] + sig[64] = 96 bytes.
            if signature.len() % 96 != 0 || signature.is_empty() {
                return None;
            }
            let mut valid_count = 0u16;
            let mut counted_signers = std::collections::HashSet::new();
            for chunk in signature.chunks_exact(96) {
                let mut pubkey = [0u8; 32];
                pubkey.copy_from_slice(&chunk[..32]);
                let mut sig = [0u8; 64];
                sig.copy_from_slice(&chunk[32..96]);

                // Reject duplicate signers — each key can only count once.
                if !counted_signers.insert(pubkey) {
                    continue;
                }

                // Only count if this pubkey is in the signers list.
                if signers.contains(&pubkey) && solen_crypto::verify(&pubkey, msg, &sig).is_ok() {
                    valid_count += 1;
                }
            }
            if valid_count >= *threshold { Some("threshold") } else { None }
        }
        AuthMethod::Passkey { public_key_x, public_key_y, rp_id, origins, .. } => {
            if verify_passkey(public_key_x, public_key_y, rp_id, origins, msg, signature) {
                Some("passkey")
            } else {
                None
            }
        }
        AuthMethod::Session { session_key, .. } => {
            // Session keys use Ed25519 signatures. Restriction checks
            // (expiry, spending, targets, methods) are done in execute_operation.
            if signature.len() != 64 {
                return None;
            }
            let mut sig = [0u8; 64];
            sig.copy_from_slice(signature);
            solen_crypto::verify(session_key, msg, &sig).ok().map(|_| "session")
        }
        AuthMethod::Guardian { .. } => None, // Guardians don't sign transactions.
    }
}

/// Restrictions enforced on the contract sub-calls of a session-authorized
/// operation (only built when the session opted in via `restrict_subcalls`).
struct SubcallPolicy {
    allowed_targets: Vec<AccountId>,
    allowed_methods: Vec<String>,
}

/// Output of `validate_and_prepare`: the optional session-budget charge to
/// commit on success, and the optional sub-call policy to enforce while the
/// operation executes.
struct PreparedOp {
    session_charge: Option<(Vec<u8>, u128)>,
    subcall_policy: Option<SubcallPolicy>,
}

/// State key holding a session key's cumulative lifetime spend:
/// `session_spent/{owner_hex}/{session_pk_hex}`. The value is a u128 LE total.
fn session_spent_key(owner: &AccountId, session_key: &[u8; 32]) -> Vec<u8> {
    let mut key = b"session_spent/".to_vec();
    for b in owner {
        key.extend_from_slice(format!("{b:02x}").as_bytes());
    }
    key.push(b'/');
    for b in session_key {
        key.extend_from_slice(format!("{b:02x}").as_bytes());
    }
    key
}

/// Verify a WebAuthn/Passkey P-256 (secp256r1) ECDSA signature.
///
/// Signature format:
///   auth_data_len[2 LE] + authenticatorData[N] + client_data_json_len[2 LE] + clientDataJSON[M] + r[32] + s[32]
///
/// Verification:
///   1. Extract challenge from clientDataJSON, verify it matches base64url(msg)
///   2. Verify clientDataJSON type is "webauthn.get" and origin is allowed
///   3. Verify authenticatorData rpIdHash == SHA-256(rp_id) (if rp_id is set)
///   4. Compute signed_data = authenticatorData || SHA-256(clientDataJSON)
///   5. Verify P-256 ECDSA signature over SHA-256(signed_data)
fn verify_passkey(
    pk_x: &[u8; 32],
    pk_y: &[u8; 32],
    rp_id: &str,
    origins: &[String],
    msg: &[u8],
    signature: &[u8],
) -> bool {
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

    // 1b. The clientDataJSON type MUST be "webauthn.get" (an assertion).
    // Without this, a "webauthn.create" registration clientDataJSON carrying
    // the same challenge could be replayed to authenticate a transaction.
    match extract_json_string(client_data_str, "type") {
        Some("webauthn.get") => {}
        _ => return false,
    }

    // 1c. Bind the assertion's origin to the account's allowlist (if set).
    // Stops an assertion produced for a different (e.g. phishing) origin from
    // being replayed against this account. Empty allowlist = not enforced.
    if !origins.is_empty() {
        match extract_json_string(client_data_str, "origin") {
            Some(o) if origins.iter().any(|allowed| allowed == o) => {}
            _ => return false,
        }
    }

    // 2. Verify authenticatorData flags (UP bit must be set).
    if auth_data.len() < 37 {
        return false;
    }

    // 2b. Bind authenticatorData rpIdHash to SHA-256(rp_id) (if rp_id is set).
    // The first 32 bytes of authenticatorData are the RP ID hash; binding it
    // ensures the credential is being used for this Relying Party, not another.
    if !rp_id.is_empty() {
        let expected_rp_hash = Sha256::digest(rp_id.as_bytes());
        if auth_data[0..32] != expected_rp_hash[..] {
            return false;
        }
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
/// Rejects duplicate keys to prevent injection attacks where an attacker
/// provides two "challenge" fields and the parser picks the wrong one.
fn extract_json_string<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("\"{}\"", key);

    // Find first occurrence.
    let key_pos = json.find(&pattern)?;

    // Reject if there's a second occurrence (duplicate key injection).
    if json[key_pos + pattern.len()..].contains(&pattern) {
        return None;
    }

    let after_key = &json[key_pos + pattern.len()..];
    // Skip whitespace and colon
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_ws = after_colon.trim_start();
    // Expect opening quote
    let after_quote = after_ws.strip_prefix('"')?;
    // Find closing quote, handling escaped quotes.
    let mut end = 0;
    let bytes = after_quote.as_bytes();
    while end < bytes.len() {
        if bytes[end] == b'"' && (end == 0 || bytes[end - 1] != b'\\') {
            break;
        }
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    // Reject values containing backslashes (no legitimate challenge uses escapes).
    let value = &after_quote[..end];
    if value.contains('\\') {
        return None;
    }
    Some(value)
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

/// Check if an operation is system-authorized (signature = [0xFF]).
/// These are injected by the block proposer and bypass signature verification.
/// Currently used for: intent fulfillment and on-chain slashing.
fn is_system_authorized(op: &UserOperation) -> bool {
    op.signature == [0xFF]
        && op.actions.len() == 1
        && matches!(&op.actions[0], Action::Call { target, method, .. }
            if (solen_types::system::is_system_contract(target))
                && (method == "fulfill" || method == "slash"))
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
        // Stage the block in an in-memory overlay, then flush all of its writes
        // to the real store as ONE atomic, fsync'd batch. A crash mid-block then
        // leaves the store either fully before or fully after this block — never
        // a partial block (which would diverge the state root and force a
        // resync). The root is recomputed over the committed store afterward, so
        // it is unchanged and cannot diverge from other nodes.
        let (mut result, changes) = self.stage_block(store, operations, height);
        if let Err(e) = store.apply_batch_atomic(&changes, true) {
            tracing::error!(error = %e, height, "atomic block commit failed");
        }
        result.state_root = store.state_root();
        store.commit_root();
        result
    }

    /// Stage a block against an in-memory overlay WITHOUT touching the real
    /// store, returning the receipts/gas plus the staged change set (writes and
    /// deletes). The caller decides whether to commit. Note: `result.state_root`
    /// here is only the overlay's placeholder (base root) — the authoritative
    /// root is computed over the store after the changes are applied.
    fn stage_block(
        &self,
        store: &dyn StateStore,
        operations: &[UserOperation],
        height: u64,
    ) -> (BlockResult, std::collections::HashMap<Vec<u8>, Option<Vec<u8>>>) {
        let mut overlay = solen_storage::OverlayStore::new(store);

        let mut result = if operations.len() >= 200 {
            self.execute_block_parallel(&mut overlay, operations, height)
        } else {
            self.execute_block_sequential(&mut overlay, operations, height)
        };

        // Epoch rewards are part of this block's atomic unit too.
        if height > 0 && height % 100 == 0 {
            let reward_receipts = distribute_epoch_rewards_in_executor(&mut overlay, height);
            result.receipts.extend(reward_receipts);
        }

        (result, overlay.into_changes())
    }

    /// Capture the pre-commit value of every key in `changes` (so the block can
    /// be undone later), then atomically commit. Returns the reverse-delta. The
    /// capture is O(touched keys) — small per block, and cheap on any backend
    /// (unlike RocksDB's `snapshot()`, which copies the whole DB).
    fn commit_capturing_revert(
        &self,
        store: &mut dyn StateStore,
        changes: BlockRevert,
        height: u64,
    ) -> BlockRevert {
        let revert: BlockRevert = changes
            .keys()
            .map(|k| (k.clone(), store.get(k).ok().flatten()))
            .collect();
        if let Err(e) = store.apply_batch_atomic(&changes, true) {
            tracing::error!(error = %e, height, "atomic block commit failed");
        }
        revert
    }

    /// Execute and commit a block, returning the result and the reverse-delta
    /// needed to undo it (for the rollback journal). Used on the produce path
    /// and any path where we already trust the block.
    pub fn execute_block_journaled(
        &self,
        store: &mut dyn StateStore,
        operations: &[UserOperation],
        height: u64,
    ) -> (BlockResult, BlockRevert) {
        let (mut result, changes) = self.stage_block(&*store, operations, height);
        let revert = self.commit_capturing_revert(store, changes, height);
        result.state_root = store.state_root();
        store.commit_root();
        (result, revert)
    }

    /// Execute a block and commit it ONLY if the resulting state root matches
    /// `expected_root`. On mismatch the block's writes are reverted via a cheap
    /// reverse-delta over just the touched keys (NOT a full-store snapshot —
    /// RocksDB's `snapshot()` copies the entire DB), leaving the store exactly
    /// as it was. Returns `Some((result, revert))` when committed (the revert is
    /// for the rollback journal), `None` when reverted.
    ///
    /// This keeps a divergent block from a peer (wrong fork, lying proposer, or
    /// execution we disagree with) from corrupting local state — which is what
    /// used to leave a node permanently unable to apply the canonical chain and
    /// force a full-snapshot re-download to recover.
    pub fn execute_block_checked(
        &self,
        store: &mut dyn StateStore,
        operations: &[UserOperation],
        height: u64,
        expected_root: &solen_types::Hash,
    ) -> Option<(BlockResult, BlockRevert)> {
        let (mut result, changes) = self.stage_block(&*store, operations, height);
        let revert = self.commit_capturing_revert(store, changes, height);
        result.state_root = store.state_root();

        if &result.state_root == expected_root {
            store.commit_root();
            Some((result, revert))
        } else {
            // Roll back the just-applied writes — store returns to its prior state.
            if let Err(e) = store.apply_batch_atomic(&revert, true) {
                tracing::error!(error = %e, height, "block revert after root mismatch failed");
            }
            store.commit_root();
            None
        }
    }

    /// Sequential execution (original path).
    fn execute_block_sequential(
        &self,
        store: &mut dyn StateStore,
        operations: &[UserOperation],
        height: u64,
    ) -> BlockResult {
        let mut receipts = Vec::with_capacity(operations.len());
        let mut total_gas = 0u64;

        for op in operations {
            let receipt = self.execute_operation(store, op, height);
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
        height: u64,
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
        // Returns the auth method name if valid, or None if invalid.
        let validations: Vec<Option<&'static str>> = operations
            .par_iter()
            .zip(pre.par_iter())
            .map(|(op, (msg, auth))| {
                // System-authorized ops (intent fulfillment, slashing) bypass signature checks.
                if is_system_authorized(op) {
                    return Some("system");
                }
                let auth_methods = match auth {
                    Some(methods) => methods,
                    None => return None,
                };
                if auth_methods.is_empty() {
                    return None; // no auth methods = reject (accounts must have auth)
                }
                auth_methods.iter().find_map(|method| verify_auth(method, msg, &op.signature))
            })
            .collect();

        // Phase 2: sequential state application, skipping ops that failed validation.
        let mut receipts = Vec::with_capacity(operations.len());
        let mut total_gas = 0u64;

        for (i, op) in operations.iter().enumerate() {
            let auth_method_name = match validations[i] {
                Some(name) => name,
                None => {
                    receipts.push(ExecutionReceipt {
                        sender: op.sender,
                        nonce: op.nonce,
                        success: false,
                        gas_used: 0,
                        error: Some("signature verification failed".into()),
                        events: vec![],
                        auth_method: "none".to_string(),
                    });
                    continue;
                }
            };

            let mut receipt = self.execute_operation(store, op, height);
            receipt.auth_method = auth_method_name.to_string();
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
        height: u64,
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
                auth_method: "ed25519".to_string(),
            };
        }

        // Validate deploy code sizes and WASM validity.
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
                        auth_method: "ed25519".to_string(),
                    };
                }
                // Pre-validate WASM bytecode structure at deploy time.
                if let Err(e) = self.vm_runtime.validate_bytecode(code) {
                    return ExecutionReceipt {
                        sender: op.sender,
                        nonce: op.nonce,
                        success: false,
                        gas_used: 0,
                        error: Some(format!("invalid WASM bytecode: {}", e)),
                        events: vec![],
                        auth_method: "ed25519".to_string(),
                    };
                }
            }
        }

        // Validate signature against the account's auth methods.
        let (session_charge, subcall_policy) = {
            let mut state = StateManager::new(store);
            match self.validate_and_prepare(&mut state, op) {
                Ok(p) => (p.session_charge, p.subcall_policy),
                Err(e) => {
                    warn!(sender = ?op.sender[..4], error = %e, "operation validation failed");
                    return ExecutionReceipt {
                        sender: op.sender,
                        nonce: op.nonce,
                        success: false,
                        gas_used: 0,
                        error: Some(e.to_string()),
                        events: vec![],
                        auth_method: "ed25519".to_string(),
                    };
                }
            }
        };

        // Take a savepoint before any state mutation so a failed operation can
        // be rolled back completely — including the fee reserve below,
        // single-action contract writes, and system-contract keys. When the
        // block executes against the per-block overlay this is cheap (it clones
        // only the staged delta, since the base store is never mutated
        // mid-block); otherwise it falls back to a full snapshot.
        let savepoint = store.savepoint();

        // Reserve max_fee upfront to prevent spend-then-underpay attacks.
        // The reserved amount is refunded after execution, then actual fee is deducted.
        let max_possible_fee = {
            let mut state = StateManager::new(store);
            match state.get_account(&op.sender) {
                Ok(Some(mut sender_acct)) => {
                    let reserve = op.max_fee.min(sender_acct.balance);
                    sender_acct.balance -= reserve;
                    let _ = state.save_account(&sender_acct);
                    reserve
                }
                _ => 0,
            }
        };

        // Track native SOLEN transferred to each target within this op, for
        // msg_value(). Each Action::Call consumes and resets the counter for
        // its target, giving the called contract the sum of all Transfers to
        // it since the previous Call to the same target (or op start).
        let mut pending_transfers: std::collections::HashMap<AccountId, u128> =
            std::collections::HashMap::new();

        let mut action_failed = None;
        for action in &op.actions {
            let msg_value_for_action = match action {
                Action::Transfer { to, amount } => {
                    let entry = pending_transfers.entry(*to).or_insert(0);
                    *entry = entry.saturating_add(*amount);
                    0
                }
                Action::Call { target, .. } => pending_transfers.remove(target).unwrap_or(0),
                _ => 0,
            };

            // Check for system contract calls (need raw store access).
            // System contracts use a direct-debit model and do not consume msg_value.
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
            match self.execute_action(&mut state, &op.sender, action, msg_value_for_action, height, &mut events, subcall_policy.as_ref()) {
                Ok(gas) => gas_used += gas,
                Err(e) => {
                    action_failed = Some(e.to_string());
                    break;
                }
            }
        }

        // If any action failed, roll back ALL state changes from this operation.
        if let Some(err) = action_failed {
            // Restore the store to the pre-operation state. This reverts every
            // key the op wrote, across all prefixes (accounts, contract storage,
            // and system-contract keys: rollup batches, bridge markers, intents,
            // config), and removes any keys it created.
            store.restore_savepoint(savepoint);
            // The savepoint was taken after the nonce was consumed but before the
            // fee reserve, so the restore returns the full pre-fee balance and
            // leaves the nonce consumed (replay protection holds on failed ops).
            // Re-consume defensively (no-op if already consumed) and charge only
            // the actual gas used — never the reserved max_fee.
            {
                let mut state = StateManager::new(store);
                let _ = state.consume_nonce(&op.sender, op.nonce);
            }
            // H-01: cap the failed-op fee at the signed max_fee as well.
            let actual_fee = self.fee_config.calculate_fee(gas_used).min(op.max_fee);
            if actual_fee > 0 {
                let mut state = StateManager::new(store);
                if let Ok(mut acct) = state.require_account(&op.sender) {
                    acct.balance = acct.balance.saturating_sub(actual_fee);
                    let _ = state.save_account(&acct);
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
                auth_method: "ed25519".to_string(),
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
        // H-01: never charge the sender more than the max_fee they signed. The
        // signed payload commits to max_fee, so honoring it as a hard cap is the
        // authorization the sender granted (real clients set 100k–1M; 0 means
        // "authorize no fee"). This also closes the path that fed C-01: a
        // computed fee exceeding the reserve.
        let total_fee = fee_config.calculate_fee(gas_used).min(op.max_fee);
        if max_possible_fee > 0 || total_fee > 0 {
            let mut state = StateManager::new(store);
            if let Ok(Some(mut sender_acct)) = state.get_account(&op.sender) {
                // Refund the reserved max fee, then charge the actual fee — but
                // never more than the sender can actually pay. The amount truly
                // debited (fee_paid) is what we split between treasury and burn,
                // so credited + burned == debited exactly.
                //
                // C-01: previously the treasury was credited a share of the
                // *intended* total_fee even when the saturating debit charged the
                // sender less than that (insufficient balance), minting the
                // shortfall into existence. Splitting the actually-debited amount
                // makes fee settlement supply-conserving by construction.
                let available = sender_acct.balance.saturating_add(max_possible_fee);
                let fee_paid = total_fee.min(available);
                sender_acct.balance = available - fee_paid;
                save_or_warn(&mut state, &sender_acct);

                // Credit treasury with the non-burned portion of what was paid.
                // The burned remainder (fee_paid - treasury_share) is permanently
                // removed from circulation by not being credited to any account.
                // treasury_share = fee_paid * (10000 - burn_bps) / 10000 <= fee_paid,
                // so the credit can never exceed the debit.
                let treasury_share = fee_config.treasury_amount(fee_paid);
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
                    data: fee_paid.to_le_bytes().to_vec(),
                });
            }
        }

        // The op succeeded — commit the session key's cumulative budget increment.
        // (On the failure path above this is skipped, so reverts never burn budget.)
        if let Some((key, new_total)) = session_charge {
            if let Err(e) = store.put(&key, &new_total.to_le_bytes()) {
                warn!(error = %e, "failed to persist session budget ledger");
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
            auth_method: "ed25519".to_string(),
        }
    }

    /// Validate the operation: check account exists, verify nonce, verify signature.
    ///
    /// Returns an optional session-budget charge `(ledger_key, new_running_total)`
    /// that the caller must commit to state **only if the operation succeeds**, so
    /// that reverted operations do not consume a session key's lifetime budget.
    fn validate_and_prepare(
        &self,
        state: &mut StateManager<'_>,
        op: &UserOperation,
    ) -> Result<PreparedOp, ExecutionError> {
        let mut session_charge: Option<(Vec<u8>, u128)> = None;
        let mut subcall_policy: Option<SubcallPolicy> = None;
        let account = state
            .get_account(&op.sender)?
            .ok_or_else(|| ExecutionError::AccountNotFound(format!("{:?}", &op.sender[..4])))?;

        // System-authorized operations (signature = [0xFF]) skip signature checks.
        // These are created by the block proposer for intent fulfillment and slashing.
        let is_system_op = is_system_authorized(op);

        let matched_session = if is_system_op {
            // Skip signature and session key checks for intent operations.
            None
        } else {
            // Verify signature against one of the account's auth methods.
            let msg = self.operation_signing_message(op);
            let sig_valid = account.auth_methods.iter().any(|method| {
                verify_auth(method, &msg, &op.signature).is_some()
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
            session_key,
            expires_at,
            spending_limit,
            budget_total,
            allowed_targets,
            allowed_methods,
            restrict_subcalls,
            ..
        }) = matched_session
        {
            // If the session opted into sub-call restriction, carry its allowlist
            // forward so the executor enforces it on every contract sub-call too
            // (no-op when both lists are empty — empty means "all allowed").
            if *restrict_subcalls && (!allowed_targets.is_empty() || !allowed_methods.is_empty()) {
                subcall_policy = Some(SubcallPolicy {
                    allowed_targets: allowed_targets.clone(),
                    allowed_methods: allowed_methods.clone(),
                });
            }
            // Check expiry (read current block height from chain meta).
            let current_height = state.current_height().unwrap_or(0);
            if current_height > *expires_at {
                return Err(ExecutionError::State(StateError::AccountNotFound(
                    "session key expired".into(),
                )));
            }

            // Compute this operation's spend once — counts all balance-affecting
            // actions, including system contract calls (staking, bridge deposits,
            // etc.). Used by both the per-op cap and the cumulative budget.
            let this_spend: u128 = op.actions.iter().map(|a| match a {
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

            // Per-operation spend cap.
            if *spending_limit > 0 && this_spend > *spending_limit {
                return Err(ExecutionError::State(StateError::AccountNotFound(
                    format!("session spending limit exceeded: {} > {}", this_spend, spending_limit),
                )));
            }

            // Cumulative lifetime budget across every op signed by this session
            // key. The running total lives at session_spent/{owner_hex}/{pk_hex}.
            // We only verify here; execute_operation commits the new total if the
            // op succeeds, so a reverted op never burns budget.
            if *budget_total > 0 {
                let key = session_spent_key(&op.sender, session_key);
                let prior = state
                    .store_mut()
                    .get(&key)
                    .ok()
                    .flatten()
                    .filter(|v| v.len() >= 16)
                    .map(|v| {
                        let mut b = [0u8; 16];
                        b.copy_from_slice(&v[..16]);
                        u128::from_le_bytes(b)
                    })
                    .unwrap_or(0);
                let new_total = prior.saturating_add(this_spend);
                if new_total > *budget_total {
                    return Err(ExecutionError::State(StateError::AccountNotFound(format!(
                        "session budget exhausted: {} + {} > {}",
                        prior, this_spend, budget_total
                    ))));
                }
                if this_spend > 0 {
                    session_charge = Some((key, new_total));
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

            // Session keys must NEVER be able to perform privileged operations.
            // These require full account authorization (Ed25519/Threshold/Passkey).
            for action in &op.actions {
                match action {
                    Action::SetAuth { .. } => {
                        return Err(ExecutionError::State(StateError::AccountNotFound(
                            "session keys cannot modify account auth methods".into(),
                        )));
                    }
                    Action::Deploy { .. } => {
                        return Err(ExecutionError::State(StateError::AccountNotFound(
                            "session keys cannot deploy contracts".into(),
                        )));
                    }
                    Action::Call { target, method, .. } => {
                        // Block guardian recovery operations — a session key holder
                        // who is also a guardian could bypass SetAuth restrictions
                        // by initiating recovery to replace the account's auth.
                        if *target == solen_types::system::GUARDIAN_ADDRESS {
                            return Err(ExecutionError::State(StateError::AccountNotFound(
                                "session keys cannot call guardian recovery".into(),
                            )));
                        }
                        // Block governance operations that change proposal state.
                        // Session keys may vote but cannot create, finalize, or execute proposals.
                        if *target == solen_types::system::GOVERNANCE_ADDRESS
                            && (method.starts_with("propose") || method == "finalize" || method == "execute")
                        {
                            return Err(ExecutionError::State(StateError::AccountNotFound(
                                "session keys cannot create or execute governance proposals".into(),
                            )));
                        }
                    }
                    _ => {}
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

        // Consume nonce (skip for system-authorized ops).
        if !is_system_op {
            state.consume_nonce(&op.sender, op.nonce)?;
        }

        Ok(PreparedOp { session_charge, subcall_policy })
    }

    /// Compute the message that must be signed for an operation.
    /// Delegates to `UserOperation::signing_message` — that is the single
    /// source of truth for the signing-digest format.
    pub fn operation_signing_message(&self, op: &UserOperation) -> Vec<u8> {
        op.signing_message(self.chain_id)
    }

    /// Execute a single action within an operation.
    ///
    /// `msg_value` is the sum of unconsumed `Action::Transfer { to: <call-target> }`
    /// amounts preceding this action in the op; it is exposed to WASM contracts
    /// via the `msg_value` host function and is ignored for non-Call actions.
    fn execute_action(
        &self,
        state: &mut StateManager<'_>,
        sender: &AccountId,
        action: &Action,
        msg_value: u128,
        height: u64,
        events: &mut Vec<Event>,
        subcall_policy: Option<&SubcallPolicy>,
    ) -> Result<u64, ExecutionError> {
        match action {
            Action::Transfer { to, amount } => {
                // Check if recipient exists — account creation incurs a gas surcharge.
                let is_new_account = state.get_account(to)?.is_none();

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

                let gas = if is_new_account {
                    TRANSFER_GAS + ACCOUNT_CREATION_GAS
                } else {
                    TRANSFER_GAS
                };
                Ok(gas)
            }
            Action::Call {
                target,
                method,
                args,
            } => {
                // Total VM fuel this call and its entire queued sub-call tree
                // may consume. Bounds the 16-per-frame × depth-8 fan-out to a
                // finite amount of work regardless of how calls are nested.
                const MAX_CALL_TREE_FUEL: u64 = 64_000_000; // ~64 full-budget calls
                let mut fuel_budget = MAX_CALL_TREE_FUEL;
                self.dispatch_contract_call(
                    state,
                    sender,
                    target,
                    method.as_bytes(),
                    args,
                    msg_value,
                    height,
                    events,
                    0,
                    &mut fuel_budget,
                    subcall_policy,
                )
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

    /// Invoke a contract call — dispatches into the VM, persists storage,
    /// processes queued native transfers and queued contract→contract calls.
    ///
    /// Used both by `Action::Call` (depth=0, caller=op sender) and by the
    /// recursive dispatch of pending calls queued via `sdk::queue_call`
    /// (depth>0, caller=queueing contract).
    ///
    /// Returns accumulated gas (this call + all recursively-dispatched
    /// sub-calls). Propagates errors — which trigger the op's existing
    /// multi-action rollback.
    #[allow(clippy::too_many_arguments)]
    fn dispatch_contract_call(
        &self,
        state: &mut StateManager<'_>,
        caller: &AccountId,
        target: &AccountId,
        method: &[u8],
        args: &[u8],
        msg_value: u128,
        height: u64,
        events: &mut Vec<Event>,
        depth: u32,
        fuel_budget: &mut u64,
        subcall_policy: Option<&SubcallPolicy>,
    ) -> Result<u64, ExecutionError> {
        // Cap recursion from pending-call fan-out. Matches actor-style depth
        // budgets in other chains. Each queued call can itself queue more,
        // so this bounds the whole chain.
        const MAX_CALL_DEPTH: u32 = 8;
        if depth > MAX_CALL_DEPTH {
            return Err(ExecutionError::State(StateError::AccountNotFound(
                format!("call depth exceeded: {depth} > {MAX_CALL_DEPTH}"),
            )));
        }

        // Enforce a session key's sub-call allowlist (opt-in via
        // `restrict_subcalls`). The op's TOP-LEVEL call/target/method are already
        // checked in `validate_and_prepare`, so this only gates the queued
        // contract→contract sub-calls (depth > 0) of a restricted session.
        if depth > 0 {
            if let Some(policy) = subcall_policy {
                if !policy.allowed_targets.is_empty() && !policy.allowed_targets.contains(target) {
                    return Err(ExecutionError::State(StateError::AccountNotFound(
                        "session key not authorized for sub-call target".into(),
                    )));
                }
                if !policy.allowed_methods.is_empty()
                    && !policy.allowed_methods.iter().any(|m| m.as_bytes() == method)
                {
                    return Err(ExecutionError::State(StateError::AccountNotFound(
                        "session key not authorized for sub-call method".into(),
                    )));
                }
            }
        }

        // Queued calls to system contracts route through the same path that
        // top-level Action::Call uses (see ~line 575). The `caller` becomes the
        // system call's `sender`, so a contract can invoke STAKING_ADDRESS,
        // BRIDGE_ADDRESS, etc. on its own behalf. Without this branch, queued
        // calls to system addresses fall through to the `code_hash == 0` path
        // below and silently no-op. msg_value is intentionally ignored —
        // system contracts use a direct-debit model against caller balance,
        // matching the top-level routing.
        if solen_types::system::is_system_contract(target) {
            let method_str = std::str::from_utf8(method).map_err(|_| {
                ExecutionError::State(StateError::AccountNotFound(
                    "system call method not utf-8".into(),
                ))
            })?;
            let result = crate::system_calls::execute_system_call(
                state.store_mut(),
                caller,
                target,
                method_str,
                args,
            );
            events.extend(result.events);
            if let Some(err) = result.error {
                return Err(ExecutionError::State(StateError::AccountNotFound(
                    format!("system call failed: {err}"),
                )));
            }
            return Ok(CALL_BASE_GAS + result.gas_used);
        }

        let target_account = state.require_account(target)?;

        // If the account has no code, it's not a contract.
        let zero_hash = [0u8; 32];
        if target_account.code_hash == zero_hash {
            let method_str = String::from_utf8_lossy(method);
            events.push(Event {
                emitter: *target,
                topic: format!("call:{method_str}").into_bytes(),
                data: args.to_vec(),
            });
            return Ok(CALL_BASE_GAS);
        }

        let bytecode = state
            .load_bytecode(&target_account.code_hash)?
            .ok_or_else(|| {
                ExecutionError::State(StateError::AccountNotFound("bytecode not found".into()))
            })?;

        let contract_storage = state.load_contract_storage(target)?;

        // Build input: method name + null + args. Matches the dispatcher format
        // every Solen contract already uses.
        let mut input = Vec::with_capacity(method.len() + 1 + args.len());
        input.extend_from_slice(method);
        input.push(0);
        input.extend_from_slice(args);

        let ctx = solen_vm::host::HostContext::new(*caller, height)
            .with_contract_id(*target)
            .with_storage(contract_storage)
            .with_msg_value(msg_value)
            .with_self_balance(target_account.balance);

        // Draw from the operation's shared fuel budget so the entire queued
        // sub-call tree (up to 16 calls per frame × depth 8) cannot each get a
        // fresh 1M-fuel budget. The per-call cap stays at the VM default; the
        // shared budget bounds the whole tree.
        const MAX_FUEL_PER_CALL: u64 = 1_000_000;
        if *fuel_budget == 0 {
            return Err(ExecutionError::State(StateError::AccountNotFound(
                "operation fuel budget exhausted".into(),
            )));
        }
        let call_fuel = (*fuel_budget).min(MAX_FUEL_PER_CALL);
        let result = match self.vm_runtime.execute(
            &target_account.code_hash,
            &bytecode,
            &input,
            ctx,
            Some(call_fuel),
        ) {
            Ok(r) => r,
            Err(solen_vm::VmError::OutOfGas) => {
                return Err(ExecutionError::State(StateError::AccountNotFound(
                    "out of gas".into(),
                )));
            }
            Err(e) => return Err(ExecutionError::VmError(e)),
        };
        *fuel_budget = fuel_budget.saturating_sub(result.gas_used);

        state.save_contract_storage(target, &result.storage)?;

        // Process native SOLEN transfers from the contract.
        for transfer in &result.native_transfers {
            let mut contract_acct = state.require_account(target)?;
            if contract_acct.balance < transfer.amount {
                return Err(ExecutionError::State(StateError::AccountNotFound(
                    "contract insufficient balance for transfer".into(),
                )));
            }
            contract_acct.balance -= transfer.amount;
            state.save_account(&contract_acct)?;

            match state.require_account(&transfer.to) {
                Ok(mut recipient) => {
                    recipient.balance = recipient.balance.saturating_add(transfer.amount);
                    state.save_account(&recipient)?;
                }
                Err(_) => {
                    let new_acct = solen_types::account::Account {
                        id: transfer.to,
                        balance: transfer.amount,
                        nonce: 0,
                        code_hash: [0u8; 32],
                        auth_methods: vec![],
                    };
                    state.save_account(&new_acct)?;
                }
            }

            events.push(Event {
                emitter: *target,
                topic: b"native_transfer".to_vec(),
                data: {
                    let mut d = Vec::with_capacity(48);
                    d.extend_from_slice(&transfer.to);
                    d.extend_from_slice(&transfer.amount.to_le_bytes());
                    d
                },
            });
        }

        // Convert VM events to execution events.
        for vm_event in &result.events {
            events.push(Event {
                emitter: *target,
                topic: vm_event.topic.clone(),
                data: vm_event.data.clone(),
            });
        }

        let mut total_gas = CALL_BASE_GAS + result.gas_used;

        // Drain queued contract→contract calls. Each runs with caller=this
        // contract; any failure propagates, triggering the op's rollback.
        // msg_value is 0 for queued calls (no pre-queued Transfer mechanism
        // yet — can be added later if needed for SOLEN-attached sub-calls).
        for pending in &result.pending_calls {
            let sub_gas = self.dispatch_contract_call(
                state,
                target, // caller = the contract that queued
                &pending.target,
                &pending.method,
                &pending.args,
                0,
                height,
                events,
                depth + 1,
                fuel_budget, // same shared budget across the whole tree
                subcall_policy, // carry the session allowlist down the call tree
            )?;
            total_gas = total_gas.saturating_add(sub_gas);
        }

        Ok(total_gas)
    }

    /// Simulate an operation without modifying state. Returns the receipt
    /// that would result from execution. Uses a copy-on-write overlay
    /// instead of copying the entire database.
    pub fn simulate(
        &self,
        store: &dyn StateStore,
        op: &UserOperation,
        height: u64,
    ) -> ExecutionReceipt {
        let mut overlay = solen_storage::OverlayStore::new(store);
        self.execute_operation(&mut overlay, op, height)
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
        auth_method: "system".to_string(),
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

    /// Build a valid passkey signature blob over `msg`, bound to `rp_id` and
    /// `origin`, signed by `signing_key`. Lets the binding tests vary one field
    /// at a time. Returns (signature_blob, pk_x, pk_y).
    fn make_passkey_assertion(
        signing_key: &p256::ecdsa::SigningKey,
        rp_id: &str,
        origin: &str,
        msg: &[u8],
    ) -> (Vec<u8>, [u8; 32], [u8; 32]) {
        use p256::ecdsa::{signature::Signer, Signature};
        use sha2::{Digest, Sha256};

        // authenticatorData: rpIdHash[32] = SHA-256(rp_id), flags[1] = UP, counter[4].
        let mut auth_data = vec![0u8; 37];
        auth_data[0..32].copy_from_slice(&Sha256::digest(rp_id.as_bytes()));
        auth_data[32] = 0x01;

        let client_data = format!(
            r#"{{"type":"webauthn.get","challenge":"{}","origin":"{}"}}"#,
            base64url_encode(msg),
            origin
        );
        let client_data_bytes = client_data.as_bytes();

        let mut signed_data = Vec::new();
        signed_data.extend_from_slice(&auth_data);
        signed_data.extend_from_slice(&Sha256::digest(client_data_bytes));
        let sig: Signature = signing_key.sign(&signed_data);

        let mut blob = Vec::new();
        blob.extend_from_slice(&(auth_data.len() as u16).to_le_bytes());
        blob.extend_from_slice(&auth_data);
        blob.extend_from_slice(&(client_data_bytes.len() as u16).to_le_bytes());
        blob.extend_from_slice(client_data_bytes);
        blob.extend_from_slice(&sig.r().to_bytes());
        blob.extend_from_slice(&sig.s().to_bytes());

        let vk = signing_key.verifying_key();
        let pt = vk.to_encoded_point(false);
        let mut x = [0u8; 32];
        let mut y = [0u8; 32];
        x.copy_from_slice(pt.x().unwrap());
        y.copy_from_slice(pt.y().unwrap());
        (blob, x, y)
    }

    #[test]
    fn passkey_rp_id_and_origin_binding() {
        let sk = p256::ecdsa::SigningKey::random(&mut rand::thread_rng());
        let rp = "wallet.solenchain.io";
        let origin = "https://wallet.solenchain.io";
        let msg = b"transfer-op-signing-message";
        let (blob, x, y) = make_passkey_assertion(&sk, rp, origin, msg);
        let origins = vec![origin.to_string()];

        // Correct rp_id + allowed origin → accepted.
        assert!(verify_passkey(&x, &y, rp, &origins, msg, &blob));

        // Wrong rp_id (assertion's rpIdHash no longer matches) → rejected.
        assert!(!verify_passkey(&x, &y, "evil.example.com", &origins, msg, &blob));

        // Disallowed origin → rejected.
        let other_origins = vec!["https://phishing.example.com".to_string()];
        assert!(!verify_passkey(&x, &y, rp, &other_origins, msg, &blob));

        // Empty rp_id / origins → bindings not enforced (back-compat), still valid sig.
        assert!(verify_passkey(&x, &y, "", &[], msg, &blob));

        // An assertion minted for a different RP must not verify against this account.
        let (evil_blob, ex, ey) =
            make_passkey_assertion(&sk, "evil.example.com", "https://phishing.example.com", msg);
        assert!(!verify_passkey(&ex, &ey, rp, &origins, msg, &evil_blob));
    }

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
    fn execute_block_checked_commits_on_match_reverts_on_mismatch() {
        let (mut store, kp, alice, bob) = setup();
        let executor = zero_fee_executor();

        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob, amount: 200 }],
            max_fee: 1000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        // Root + balances BEFORE applying the block.
        let root_before = store.state_root();
        let alice_before = StateManager::new(&mut store).get_balance(&alice).unwrap();

        // Wrong expected root → block must be reverted, store fully restored.
        let bad_root = [0xABu8; 32];
        let rejected = executor.execute_block_checked(&mut store, &[op.clone()], 1, &bad_root);
        assert!(rejected.is_none(), "block with wrong expected root must be rejected");
        assert_eq!(store.state_root(), root_before, "root must be restored after revert");
        assert_eq!(
            StateManager::new(&mut store).get_balance(&alice).unwrap(),
            alice_before,
            "balances must be restored after revert"
        );

        // Compute the real root the block produces (on a throwaway store), then
        // pass it as expected → block must commit and move the balances.
        let mut probe = store.snapshot();
        let real_root = executor.execute_block_with_height(probe.as_mut(), &[op.clone()], 1).state_root;

        let committed = executor.execute_block_checked(&mut store, &[op], 1, &real_root);
        assert!(committed.is_some(), "block with correct expected root must commit");
        // The committed path returns the reverse-delta for the rollback journal.
        let (_res, revert) = committed.unwrap();
        assert!(!revert.is_empty(), "a non-empty block must yield a non-empty revert");
        assert_eq!(store.state_root(), real_root);
        assert_eq!(StateManager::new(&mut store).get_balance(&alice).unwrap(), alice_before - 200);
        assert_eq!(StateManager::new(&mut store).get_balance(&bob).unwrap(), 700);
    }

    /// Security (C-01 / H-01): fee settlement must never mint SOLEN. When the
    /// computed gas fee exceeds what the sender can actually pay, the treasury
    /// may only be credited what was truly debited — total supply must not grow.
    /// Pre-fix, the treasury was credited a share of the *intended* fee while the
    /// sender's debit was clamped to their balance, minting the shortfall.
    /// burn_rate_bps = 0 routes the whole fee to the treasury, so any mint is
    /// maximally visible there. max_fee = u128::MAX exercises the (formerly
    /// uncapped) fee path.
    #[test]
    fn fee_settlement_conserves_supply_when_sender_underpays() {
        let (mut store, kp, alice, bob) = setup();
        // Per-gas price high enough that the computed fee dwarfs alice's 10_000.
        let executor = BlockExecutor::with_fee_config(FeeConfig {
            base_fee_per_gas: 1_000_000,
            burn_rate_bps: 0, // entire fee -> treasury, so any mint shows there
            treasury_account: treasury_id(),
        });

        let supply = |store: &mut MemoryStore| -> u128 {
            let mut s = StateManager::new(store);
            s.get_balance(&alice).unwrap()
                + s.get_balance(&bob).unwrap()
                + s.get_balance(&treasury_id()).unwrap()
        };
        let supply_before = supply(&mut store);
        let alice_before = StateManager::new(&mut store).get_balance(&alice).unwrap();

        // A zero-value transfer succeeds even after the full balance is reserved,
        // so settlement runs the success path with a fee it cannot fully cover.
        let mut op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Transfer { to: bob, amount: 0 }],
            max_fee: u128::MAX,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let result = executor.execute_block_with_height(&mut store, &[op], 1);
        assert!(result.receipts[0].success, "the zero-value transfer must succeed");

        let alice_after = StateManager::new(&mut store).get_balance(&alice).unwrap();
        let treasury_after = StateManager::new(&mut store).get_balance(&treasury_id()).unwrap();
        let supply_after = supply(&mut store);

        // No mint: total supply must never increase.
        assert!(
            supply_after <= supply_before,
            "fee settlement minted: supply {supply_before} -> {supply_after}"
        );
        // The treasury credit may not exceed what the sender was actually debited.
        let alice_debited = alice_before - alice_after;
        assert!(
            treasury_after <= alice_debited,
            "treasury credited {treasury_after} but sender only paid {alice_debited}"
        );
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

        // Minimal valid WASM module with required exports.
        let code = wat::parse_str(r#"(module
            (memory (export "memory") 1)
            (func (export "call") (param i32 i32) (result i32) (i32.const 0))
        )"#).expect("WAT parse failed");
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
        assert_eq!(deployed.code_hash, blake3_hash(&code));
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

        let receipt = executor.simulate(&store, &op, 0);
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

    /// Fixture: a contract whose `call(input)` queues a single
    /// `STAKING_ADDRESS:delegate` with args = input[5..53] (i.e. the bytes
    /// after the leading `"init\0"` dispatcher prefix). Sized to forward
    /// exactly `validator[32] || amount[16]` to the system call.
    const QUEUE_DELEGATE_WAT: &str = r#"
    (module
        (import "env" "queue_contract_call" (func $queue_contract_call
            (param i32 i32 i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        ;; offset 0:  STAKING_ADDRESS = 0xFF * 31 ‖ 0x01
        ;; offset 32: "delegate" method name (8 bytes)
        (data (i32.const 0)
            "\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\ff\01")
        (data (i32.const 32) "delegate")
        (func (export "call") (param $input_ptr i32) (param $input_len i32) (result i32)
            ;; Skip the leading "init\0" (5 bytes). The remaining 48 bytes
            ;; passed by the caller are validator[32] || amount[16].
            (drop (call $queue_contract_call
                (i32.const 0)
                (i32.const 32)
                (i32.const 8)
                (i32.add (local.get $input_ptr) (i32.const 5))
                (i32.const 48)))
            (i32.const 0)
        )
    )
    "#;

    /// Fixture: a contract whose `call()` queues one contract→contract call to
    /// the fixed target B = 0x42*32 with method "ping" and no args. Used to test
    /// that a session key's sub-call allowlist gates queued calls.
    const QUEUE_TO_B_WAT: &str = r#"
    (module
        (import "env" "queue_contract_call" (func $queue_contract_call
            (param i32 i32 i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (data (i32.const 0)
            "\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42\42")
        (data (i32.const 32) "ping")
        (func (export "call") (param i32 i32) (result i32)
            (drop (call $queue_contract_call
                (i32.const 0) (i32.const 32) (i32.const 4) (i32.const 36) (i32.const 0)))
            (i32.const 0)
        )
    )
    "#;

    /// Build artifact for the agent-wallet devnet demo: emit the QUEUE_TO_B
    /// contract bytecode so the demo can deploy a real sub-call-queueing
    /// contract. Ignored by default; run with `--ignored --exact`.
    #[test]
    #[ignore]
    fn dump_queue_to_b_wasm() {
        let wasm = wat::parse_str(QUEUE_TO_B_WAT).expect("WAT parse failed");
        std::fs::create_dir_all("/tmp/solen-demo").unwrap();
        std::fs::write("/tmp/solen-demo/queue_to_b.wasm", &wasm).unwrap();
        eprintln!("wrote /tmp/solen-demo/queue_to_b.wasm ({} bytes)", wasm.len());
    }

    /// Deploy QUEUE_TO_B, grant alice a session key scoped to ONLY that contract
    /// with the given `restrict_subcalls`, then call the contract with the
    /// session key. Returns whether that session-key op succeeded. With
    /// restriction on, the queued sub-call to the non-allowlisted B must abort
    /// the op; with it off, the sub-call proceeds (top-level enforcement only).
    fn session_subcall_op_succeeds(restrict_subcalls: bool) -> bool {
        let (mut store, kp, alice, _treasury) = setup();
        let executor = zero_fee_executor();
        let session_kp = Keypair::generate();

        // Deploy the queueing contract (owner-signed).
        let wasm = wat::parse_str(QUEUE_TO_B_WAT).expect("WAT parse failed");
        let mut deploy = UserOperation {
            sender: alice, nonce: 0,
            actions: vec![Action::Deploy { code: wasm.to_vec(), salt: [0xCD; 32] }],
            max_fee: 200_000, signature: vec![],
        };
        sign_op(&kp, &executor, &mut deploy);
        let r = executor.execute_block(&mut store, &[deploy]);
        assert!(r.receipts[0].success, "deploy failed: {:?}", r.receipts[0]);
        let mut contract_id = [0u8; 32];
        contract_id.copy_from_slice(&r.receipts[0].events[0].data);

        // Make B = 0x42*32 exist (code-less) so an *unrestricted* queued sub-call
        // to it reaches the no-op path instead of failing on require_account.
        let mut transfer_b = UserOperation {
            sender: alice, nonce: 1,
            actions: vec![Action::Transfer { to: [0x42; 32], amount: 1 }],
            max_fee: 0, signature: vec![],
        };
        sign_op(&kp, &executor, &mut transfer_b);
        assert!(executor.execute_block(&mut store, &[transfer_b]).receipts[0].success, "transfer to B failed");

        // Add a session key allowed to call ONLY that contract.
        let session = AuthMethod::Session {
            session_key: session_kp.public_key(),
            expires_at: 999_999,
            spending_limit: 0,
            budget_total: 0,
            allowed_targets: vec![contract_id],
            allowed_methods: vec![],
            restrict_subcalls,
        };
        let mut setauth = UserOperation {
            sender: alice, nonce: 2,
            actions: vec![Action::SetAuth {
                auth_methods: vec![AuthMethod::Ed25519 { public_key: kp.public_key() }, session],
            }],
            max_fee: 0, signature: vec![],
        };
        sign_op(&kp, &executor, &mut setauth);
        let r2 = executor.execute_block(&mut store, &[setauth]);
        assert!(r2.receipts[0].success, "setauth failed: {:?}", r2.receipts[0]);

        // Session-key op: call the (allowed) contract, which queues a sub-call to
        // the non-allowlisted B.
        let mut callop = UserOperation {
            sender: alice, nonce: 3,
            actions: vec![Action::Call { target: contract_id, method: "call".into(), args: vec![] }],
            max_fee: 0, signature: vec![],
        };
        let msg = executor.operation_signing_message(&callop);
        callop.signature = session_kp.sign(&msg).to_vec();
        executor.execute_block(&mut store, &[callop]).receipts[0].success
    }

    #[test]
    fn session_subcall_allowlist_blocks_unlisted_target() {
        assert!(
            !session_subcall_op_succeeds(true),
            "restrict_subcalls must abort the queued sub-call to a non-allowlisted target",
        );
    }

    #[test]
    fn session_subcalls_proceed_without_restriction() {
        assert!(
            session_subcall_op_succeeds(false),
            "default (top-level only) must let the queued sub-call proceed",
        );
    }

    /// Setup with a richer alice (1 SOLEN at 8 decimals = 10^8 base units)
    /// and a pre-registered staking validator. Used by the queued-system-call
    /// tests since validator registration via the system contract requires
    /// MIN_VALIDATOR_STAKE = 500_000 SOLEN — far more than the standard
    /// `setup()` budgets.
    fn setup_with_validator() -> (MemoryStore, Keypair, AccountId, [u8; 32]) {
        use solen_system_contracts::staking::{StakingContract, MIN_VALIDATOR_STAKE};

        let mut store = MemoryStore::new();
        let kp = Keypair::generate();

        let alice_id = {
            let mut id = [0u8; 32];
            id[..4].copy_from_slice(b"alic");
            id
        };
        let validator_id: [u8; 32] = {
            let mut v = [0u8; 32];
            v[0] = 7;
            v
        };

        // Pre-populate the staking system contract with one validator.
        // Bypasses the registration system call (which would require staking
        // 500_000 SOLEN from a sender) — fine for unit tests.
        {
            let mut sc = StakingContract::new();
            sc.register_validator(validator_id, MIN_VALIDATOR_STAKE).unwrap();
            sc.save(&mut store);
        }

        apply_genesis(
            &mut store,
            vec![
                GenesisAccount {
                    id: alice_id,
                    balance: 1_000_000,
                    auth_methods: vec![AuthMethod::Ed25519 {
                        public_key: kp.public_key(),
                    }],
                },
                GenesisAccount {
                    id: treasury_id(),
                    balance: 0,
                    auth_methods: vec![],
                },
            ],
        )
        .unwrap();

        (store, kp, alice_id, validator_id)
    }

    /// Deploy `QUEUE_DELEGATE_WAT` from `alice` and return the contract id.
    fn deploy_queue_delegate(
        store: &mut MemoryStore,
        executor: &BlockExecutor,
        kp: &Keypair,
        alice: AccountId,
    ) -> AccountId {
        let wasm = wat::parse_str(QUEUE_DELEGATE_WAT).expect("WAT parse failed");
        let mut deploy_op = UserOperation {
            sender: alice,
            nonce: 0,
            actions: vec![Action::Deploy {
                code: wasm.to_vec(),
                salt: [0xAB; 32],
            }],
            max_fee: 200_000,
            signature: vec![],
        };
        sign_op(kp, executor, &mut deploy_op);
        let result = executor.execute_block(store, &[deploy_op]);
        assert!(
            result.receipts[0].success,
            "deploy failed: {:?}",
            result.receipts[0]
        );
        let mut contract_id = [0u8; 32];
        contract_id.copy_from_slice(&result.receipts[0].events[0].data);
        contract_id
    }

    #[test]
    fn queued_call_routes_to_staking_system_contract() {
        use solen_system_contracts::staking::StakingContract;

        let (mut store, kp, alice, validator) = setup_with_validator();
        let executor = zero_fee_executor();
        let contract_id = deploy_queue_delegate(&mut store, &executor, &kp, alice);

        // Ask the contract to delegate `amount` to `validator`.
        let amount: u128 = 100_000;
        let mut args = Vec::with_capacity(48);
        args.extend_from_slice(&validator);
        args.extend_from_slice(&amount.to_le_bytes());

        // The staking system call needs `caller.balance >= amount + MIN_FEE_RESERVE`
        // (10_000), so fund the contract slightly above `amount` and expect the
        // post-delegate balance to equal that overage.
        let funding = amount + 10_000;

        let mut op = UserOperation {
            sender: alice,
            nonce: 1,
            actions: vec![
                Action::Transfer {
                    to: contract_id,
                    amount: funding,
                },
                Action::Call {
                    target: contract_id,
                    method: "init".to_string(),
                    args,
                },
            ],
            max_fee: 200_000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let result = executor.execute_block(&mut store, &[op]);
        assert!(
            result.receipts[0].success,
            "op failed: {:?}",
            result.receipts[0]
        );

        // The contract is now the on-chain delegator-of-record for `amount`.
        let sc = StakingContract::load(&store);
        assert_eq!(
            sc.delegator_total_stake(&contract_id),
            amount,
            "contract should be the delegator on the staking system contract"
        );

        // Validator's `total_delegated` should reflect the delegation.
        let val = sc.get_validator(&validator).expect("validator");
        assert_eq!(val.total_delegated, amount);

        // Contract's account balance should be `funding - amount` — the
        // overage we left for MIN_FEE_RESERVE.
        let state = StateManager::new(&mut store);
        let acct = state.require_account(&contract_id).unwrap();
        assert_eq!(
            acct.balance, 10_000,
            "contract should retain only the fee-reserve overage"
        );
    }

    #[test]
    fn queued_system_call_failure_rolls_back_op() {
        use solen_system_contracts::staking::StakingContract;

        let (mut store, kp, alice, _validator) = setup_with_validator();
        let executor = zero_fee_executor();
        let contract_id = deploy_queue_delegate(&mut store, &executor, &kp, alice);

        // Snapshot pre-op state so we can assert nothing moved on rollback.
        let alice_balance_before = StateManager::new(&mut store)
            .get_balance(&alice)
            .unwrap();
        let contract_balance_before = StateManager::new(&mut store)
            .get_balance(&contract_id)
            .unwrap();

        // Address a delegation at a validator that does NOT exist. The queued
        // staking call returns `ValidatorNotFound`, my patch surfaces that as
        // `Err`, and the executor rolls the whole UserOp back per
        // `executor.rs:600` — so the Transfer must also revert and no
        // delegation should be recorded.
        let bogus_validator = [0xCC; 32];
        let amount: u128 = 100_000;
        let mut args = Vec::with_capacity(48);
        args.extend_from_slice(&bogus_validator);
        args.extend_from_slice(&amount.to_le_bytes());

        let mut op = UserOperation {
            sender: alice,
            nonce: 1,
            actions: vec![
                Action::Transfer {
                    to: contract_id,
                    amount: amount + 10_000,
                },
                Action::Call {
                    target: contract_id,
                    method: "init".to_string(),
                    args,
                },
            ],
            max_fee: 200_000,
            signature: vec![],
        };
        sign_op(&kp, &executor, &mut op);

        let result = executor.execute_block(&mut store, &[op]);
        assert!(!result.receipts[0].success, "op should have failed");

        // No delegation recorded.
        let sc = StakingContract::load(&store);
        assert_eq!(sc.delegator_total_stake(&contract_id), 0);

        // Transfer rolled back: contract balance unchanged from pre-op.
        let state = StateManager::new(&mut store);
        assert_eq!(
            state.get_balance(&contract_id).unwrap(),
            contract_balance_before,
            "Transfer to contract should have rolled back"
        );

        // Alice loses at most `max_fee` on a failed multi-action op — the
        // reservation taken at executor.rs:531 is held by the failure path
        // (only the success path refunds at line 668). The transfer to the
        // contract must not stack on top of that.
        let alice_after = state.get_balance(&alice).unwrap();
        assert!(
            alice_after >= alice_balance_before.saturating_sub(200_000),
            "alice should have lost at most max_fee (200_000); got {} → {}",
            alice_balance_before,
            alice_after
        );
    }
}
