//! RPC method implementations using jsonrpsee proc macros.

use std::sync::Arc;

use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use serde::{Deserialize, Serialize};
use solen_consensus::engine::ConsensusEngine;
use solen_execution::state::ReadonlyStateManager;
use solen_intents::types::{Constraint, Intent};
use solen_types::encoding::{account_to_base58, hex_decode as encoding_hex_decode, hex_encode, parse_address};
use solen_types::rollup::BatchCommitment;
use solen_types::transaction::UserOperation;

/// Account info returned by the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub id: String,
    pub balance: String,
    pub nonce: u64,
    pub code_hash: String,
}

/// Block info returned by the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockInfo {
    pub height: u64,
    pub epoch: u64,
    pub parent_hash: String,
    pub state_root: String,
    pub transactions_root: String,
    pub receipts_root: String,
    pub proposer: String,
    pub timestamp_ms: u64,
    pub tx_count: usize,
    pub gas_used: u64,
}

/// Simulation result returned by the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub success: bool,
    pub gas_used: u64,
    pub error: Option<String>,
    pub events: Vec<EventInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventInfo {
    pub emitter: String,
    pub topic: String,
}

/// Submit result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResult {
    pub accepted: bool,
    pub error: Option<String>,
}

/// Chain status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainStatus {
    pub height: u64,
    pub state_root: String,
    pub pending_ops: usize,
    /// Total tokens allocated at genesis (base units).
    pub total_allocation: String,
    /// Total tokens currently staked (base units).
    pub total_staked: String,
    /// Tokens currently in circulation (not locked in system pools).
    pub total_circulation: String,
    /// Governance-configurable parameters (current on-chain values).
    #[serde(default)]
    pub config: ChainConfig,
}

/// Governance-configurable chain parameters.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChainConfig {
    pub block_time_ms: u64,
    pub min_validator_stake: String,
    pub unbonding_period_epochs: u64,
    pub epoch_length: u64,
    pub base_fee_per_gas: String,
    pub burn_rate_bps: u64,
}

/// Validator info returned by the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorInfo {
    pub address: String,
    pub self_stake: String,
    pub total_delegated: String,
    pub total_stake: String,
    pub is_active: bool,
    pub is_genesis: bool,
    pub commission_bps: u64,
}

/// Staking info for an account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingInfo {
    pub total_delegated: String,
    pub delegations: Vec<DelegationInfo>,
    pub pending_undelegations: usize,
}

/// A single delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationInfo {
    pub validator: String,
    pub amount: String,
}

/// Read-only contract call result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallViewResult {
    pub success: bool,
    pub return_data: String,
    pub gas_used: u64,
    pub error: Option<String>,
}

/// Governance proposal info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernanceProposalInfo {
    pub id: u64,
    pub proposer: String,
    pub action: String,
    pub description: String,
    pub status: String,
    pub voting_end_epoch: u64,
    pub execute_after_epoch: u64,
    pub total_for: String,
    pub total_against: String,
    pub vote_count: usize,
}

/// Vesting schedule info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VestingInfo {
    pub has_schedule: bool,
    pub total_amount: String,
    pub vested: String,
    pub claimed: String,
    pub claimable: String,
    pub vesting_type: String,
}

/// Intent submission request (from RPC clients).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentRequest {
    pub sender: String,
    pub constraints: Vec<ConstraintInfo>,
    pub max_fee: String,
    pub expiry_height: u64,
    pub signature: String,
    pub tip: String,
}

/// Constraint info for RPC serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConstraintInfo {
    MinBalance { account: String, min_amount: String },
    MaxSpend { account: String, max_amount: String },
    RequireTransfer { from: String, to: String, min_amount: String },
    RequireCall { target: String, method: String },
    Custom { verifier: String, data: String },
}

/// Intent submission result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentSubmitResult {
    pub accepted: bool,
    pub intent_id: Option<u64>,
    pub error: Option<String>,
}

/// Pending intent info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentInfo {
    pub id: u64,
    pub sender: String,
    pub constraints: Vec<ConstraintInfo>,
    pub max_fee: String,
    pub expiry_height: u64,
    pub tip: String,
    pub status: String,
}

/// Solution submission request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolutionRequest {
    pub intent_id: u64,
    pub solver: String,
    pub operations: Vec<UserOperation>,
    pub claimed_tip: String,
    pub score: u64,
    /// Hex-encoded Ed25519 signature proving solver identity.
    #[serde(default)]
    pub signature: Option<String>,
}

/// Solution submission result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolutionSubmitResult {
    pub accepted: bool,
    pub error: Option<String>,
}

/// Sponsorship check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SponsorshipResult {
    pub sponsored: bool,
    pub paymaster: Option<String>,
    pub max_gas: Option<String>,
    pub reason: Option<String>,
}

/// Rollup status info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupStatusInfo {
    pub rollup_id: u64,
    pub registered: bool,
    pub last_verified_state_root: Option<String>,
    pub last_batch_index: Option<u64>,
}

/// Batch submission request (hex-encoded fields).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchSubmitRequest {
    pub rollup_id: u64,
    pub batch_index: u64,
    pub state_root: String,
    pub data_hash: String,
    pub proof: String,
}

/// Batch submission result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchSubmitResult {
    pub accepted: bool,
    pub verified: bool,
    pub error: Option<String>,
}

/// State snapshot info for fast sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub height: u64,
    pub epoch: u64,
    pub state_root: String,
    pub entries: u64,
    pub compressed_bytes: usize,
    pub uncompressed_bytes: usize,
    /// Base64-encoded compressed snapshot data.
    pub data: String,
    /// The latest finalized checkpoint (with 2/3+ validator attestations).
    /// Syncing nodes verify the snapshot state_root matches this checkpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<SnapshotCheckpoint>,
}

/// Finalized checkpoint included in snapshot responses for verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotCheckpoint {
    pub height: u64,
    pub epoch: u64,
    pub block_hash: String,
    pub state_root: String,
    /// Validator attestations: [(validator_id_base58, signature_hex)].
    pub attestations: Vec<(String, String)>,
}

/// Verified batch info returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedBatchInfo {
    pub rollup_id: u64,
    pub batch_index: u64,
    pub state_root: String,
    pub data_hash: String,
    pub pre_state_root: String,
}

#[rpc(server)]
pub trait SolenApi {
    #[method(name = "solen_getBalance")]
    fn get_balance(&self, account_id: String) -> RpcResult<String>;

    #[method(name = "solen_getAccount")]
    fn get_account(&self, account_id: String) -> RpcResult<AccountInfo>;

    /// Get the next usable nonce for an account, accounting for pending
    /// transactions in the mempool. Clients should use this instead of
    /// reading the nonce from getAccount when sending multiple transactions.
    #[method(name = "solen_getNextNonce")]
    fn get_next_nonce(&self, account_id: String) -> RpcResult<u64>;

    #[method(name = "solen_getBlock")]
    fn get_block(&self, height: u64) -> RpcResult<BlockInfo>;

    #[method(name = "solen_getLatestBlock")]
    fn get_latest_block(&self) -> RpcResult<BlockInfo>;

    #[method(name = "solen_submitOperation")]
    fn submit_operation(&self, op: UserOperation) -> RpcResult<SubmitResult>;

    #[method(name = "solen_simulateOperation")]
    fn simulate_operation(&self, op: UserOperation) -> RpcResult<SimulationResult>;

    #[method(name = "solen_chainStatus")]
    fn chain_status(&self) -> RpcResult<ChainStatus>;

    #[method(name = "solen_getValidators")]
    fn get_validators(&self) -> RpcResult<Vec<ValidatorInfo>>;

    #[method(name = "solen_getStakingInfo")]
    fn get_staking_info(&self, account_id: String) -> RpcResult<StakingInfo>;

    #[method(name = "solen_getGovernanceProposals")]
    fn get_governance_proposals(&self) -> RpcResult<Vec<GovernanceProposalInfo>>;

    /// Read-only contract call — no signature needed, no state changes.
    #[method(name = "solen_callView")]
    fn call_view(
        &self,
        contract_id: String,
        method: String,
        args: Option<String>,
    ) -> RpcResult<CallViewResult>;

    #[method(name = "solen_getVestingInfo")]
    fn get_vesting_info(&self, account_id: String) -> RpcResult<VestingInfo>;

    /// Submit an intent for solver resolution.
    #[method(name = "solen_submitIntent")]
    fn submit_intent(&self, intent: IntentRequest) -> RpcResult<IntentSubmitResult>;

    /// Get pending intents available for solvers.
    #[method(name = "solen_getPendingIntents")]
    fn get_pending_intents(&self, limit: Option<usize>) -> RpcResult<Vec<IntentInfo>>;

    /// Submit a solution for an intent.
    #[method(name = "solen_submitSolution")]
    fn submit_solution(&self, solution: SolutionRequest) -> RpcResult<SolutionSubmitResult>;

    /// Check if a paymaster will sponsor an operation's fees.
    #[method(name = "solen_checkSponsorship")]
    fn check_sponsorship(&self, op: UserOperation) -> RpcResult<SponsorshipResult>;

    /// Get rollup registration info and latest state commitment.
    #[method(name = "solen_getRollupStatus")]
    fn get_rollup_status(&self, rollup_id: u64) -> RpcResult<RollupStatusInfo>;

    /// Submit a rollup batch commitment for verification.
    #[method(name = "solen_submitBatch")]
    fn submit_batch(&self, batch: BatchSubmitRequest) -> RpcResult<BatchSubmitResult>;

    /// Get verified batches for a rollup.
    #[method(name = "solen_getRollupBatches")]
    fn get_rollup_batches(&self, rollup_id: u64, limit: Option<usize>) -> RpcResult<Vec<VerifiedBatchInfo>>;

    /// Get a compressed state snapshot for fast sync.
    #[method(name = "solen_getSnapshot")]
    fn get_snapshot(&self) -> RpcResult<SnapshotInfo>;
}

/// Cached snapshot to avoid regenerating on every request.
struct CachedSnapshot {
    height: u64,
    info: SnapshotInfo,
}

/// Per-method rate limiter: tracks call counts per second.
struct RpcRateLimiter {
    /// (call_count, window_start)
    submit_ops: std::sync::Mutex<(u64, std::time::Instant)>,
    submit_solutions: std::sync::Mutex<(u64, std::time::Instant)>,
    snapshots: std::sync::Mutex<(u64, std::time::Instant)>,
}

impl RpcRateLimiter {
    fn new() -> Self {
        let now = std::time::Instant::now();
        Self {
            submit_ops: std::sync::Mutex::new((0, now)),
            submit_solutions: std::sync::Mutex::new((0, now)),
            snapshots: std::sync::Mutex::new((0, now)),
        }
    }

    /// Check and increment rate limit. Returns true if allowed.
    fn check(counter: &std::sync::Mutex<(u64, std::time::Instant)>, max_per_sec: u64) -> bool {
        let mut guard = counter.lock().unwrap_or_else(|e| e.into_inner());
        if guard.1.elapsed() > std::time::Duration::from_secs(1) {
            *guard = (1, std::time::Instant::now());
            true
        } else {
            guard.0 += 1;
            guard.0 <= max_per_sec
        }
    }
}

/// Implementation of the Solen RPC API.
pub struct SolenRpc {
    engine: Arc<ConsensusEngine>,
    snapshot_cache: Arc<std::sync::Mutex<Option<CachedSnapshot>>>,
    rate_limiter: Arc<RpcRateLimiter>,
}

/// Minimum blocks between snapshot regenerations.
const SNAPSHOT_CACHE_INTERVAL: u64 = 500;

impl SolenRpc {
    pub fn new(engine: Arc<ConsensusEngine>) -> Self {
        let cache = Arc::new(std::sync::Mutex::new(None));

        // Pre-warm snapshot cache in background after the node settles.
        let engine_bg = engine.clone();
        let cache_bg = cache.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(30));

            let height = engine_bg.height();
            if height == 0 { return; }

            tracing::info!(height, "pre-warming snapshot cache...");

            let (store_snapshot, epoch) = {
                let store = engine_bg.store();
                let store = store.read().unwrap();
                let snap = store.snapshot();
                let chain = engine_bg.chain();
                let chain = chain.read().unwrap();
                let epoch = chain.last().map(|b| b.header.epoch).unwrap_or(0);
                (snap, epoch)
            };

            match solen_consensus::snapshot::create_snapshot(store_snapshot.as_ref(), height, epoch) {
                Ok(data) => {
                    if let Ok(meta) = solen_consensus::snapshot::read_snapshot_meta(&data) {
                        let entries = store_snapshot.len() as u64;
                        let compressed_bytes = data.len() - 56;
                        let b64 = base64_encode(&data);
                        let info = SnapshotInfo {
                            height,
                            epoch,
                            state_root: hex_encode(&meta.state_root),
                            entries,
                            compressed_bytes,
                            uncompressed_bytes: meta.uncompressed_size,
                            data: b64,
                            checkpoint: None, // cache doesn't have engine access
                        };
                        *cache_bg.lock().unwrap() = Some(CachedSnapshot { height, info });
                        tracing::info!(height, "snapshot cache warmed");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "snapshot pre-warm failed"),
            }
        });

        Self {
            engine,
            snapshot_cache: cache,
            rate_limiter: Arc::new(RpcRateLimiter::new()),
        }
    }
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 { result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char); } else { result.push('='); }
        if chunk.len() > 2 { result.push(CHARS[(triple & 0x3F) as usize] as char); } else { result.push('='); }
    }
    result
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ErrorObjectOwned> {
    encoding_hex_decode(s).map_err(|e| {
        ErrorObjectOwned::owned(-32602, e, None::<()>)
    })
}

fn parse_account_id(s: &str) -> RpcResult<[u8; 32]> {
    parse_address(s).map_err(|e| {
        ErrorObjectOwned::owned(-32602, e, None::<()>)
    })
}

fn read_config_u64(store: &dyn solen_storage::StateStore, key: &[u8]) -> Option<u64> {
    store.get(key).ok().flatten().and_then(|data| {
        if data.len() >= 8 {
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&data[..8]);
            Some(u64::from_le_bytes(buf))
        } else {
            None
        }
    })
}

fn read_config_u128(store: &dyn solen_storage::StateStore, key: &[u8]) -> Option<u128> {
    store.get(key).ok().flatten().and_then(|data| {
        if data.len() >= 16 {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&data[..16]);
            Some(u128::from_le_bytes(buf))
        } else {
            None
        }
    })
}

fn internal_error(msg: impl ToString) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32603, msg.to_string(), None::<()>)
}

impl SolenApiServer for SolenRpc {
    fn get_balance(&self, account_id: String) -> RpcResult<String> {
        let id = parse_account_id(&account_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let state = ReadonlyStateManager::new(store.as_ref());
        let balance = state.get_balance(&id).map_err(|e| internal_error(e))?;
        Ok(balance.to_string())
    }

    fn get_account(&self, account_id: String) -> RpcResult<AccountInfo> {
        let id = parse_account_id(&account_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let state = ReadonlyStateManager::new(store.as_ref());
        let account = state
            .get_account(&id)
            .map_err(|e| internal_error(e))?
            .ok_or_else(|| {
                ErrorObjectOwned::owned(-32001, "account not found", None::<()>)
            })?;

        Ok(AccountInfo {
            id: account_to_base58(&account.id),
            balance: account.balance.to_string(),
            nonce: account.nonce,
            code_hash: hex_encode(&account.code_hash),
        })
    }

    fn get_next_nonce(&self, account_id: String) -> RpcResult<u64> {
        let id = parse_account_id(&account_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let state = ReadonlyStateManager::new(store.as_ref());
        let on_chain_nonce = state
            .get_account(&id)
            .map_err(|e| internal_error(e))?
            .map(|a| a.nonce)
            .unwrap_or(0);

        let pending = self.engine.mempool().pending_count_for_sender(&id) as u64;
        Ok(on_chain_nonce + pending)
    }

    fn get_block(&self, height: u64) -> RpcResult<BlockInfo> {
        let block = self.engine.get_block(height).ok_or_else(|| {
            ErrorObjectOwned::owned(-32001, "block not found", None::<()>)
        })?;

        Ok(block_to_info(&block))
    }

    fn get_latest_block(&self) -> RpcResult<BlockInfo> {
        let block = self.engine.latest_block().ok_or_else(|| {
            ErrorObjectOwned::owned(-32001, "no blocks yet", None::<()>)
        })?;

        Ok(block_to_info(&block))
    }

    fn submit_operation(&self, op: UserOperation) -> RpcResult<SubmitResult> {
        // Rate limit: max 50 submissions per second globally.
        if !RpcRateLimiter::check(&self.rate_limiter.submit_ops, 50) {
            return Ok(SubmitResult {
                accepted: false,
                error: Some("rate limited — too many submissions, try again".to_string()),
            });
        }

        // Validate nonce before accepting into mempool.
        {
            let store = self.engine.store();
            let store = store.read().map_err(|e| internal_error(e.to_string()))?;
            let mut account_key = b"acc/".to_vec();
            account_key.extend_from_slice(&op.sender);
            match store.get(&account_key) {
                Ok(Some(data)) => {
                    if let Ok(account) = borsh::from_slice::<solen_types::account::Account>(&data) {
                        if op.nonce < account.nonce {
                            return Ok(SubmitResult {
                                accepted: false,
                                error: Some(format!(
                                    "nonce too low: got {}, expected >= {}",
                                    op.nonce, account.nonce
                                )),
                            });
                        }
                        if op.nonce > account.nonce + 16 {
                            return Ok(SubmitResult {
                                accepted: false,
                                error: Some(format!(
                                    "nonce too far ahead: got {}, current is {}",
                                    op.nonce, account.nonce
                                )),
                            });
                        }
                        // Reject operations from accounts with zero balance.
                        // Prevents mempool spam from unfunded accounts.
                        if account.balance == 0 {
                            return Ok(SubmitResult {
                                accepted: false,
                                error: Some("sender has zero balance".to_string()),
                            });
                        }
                    }
                }
                Ok(None) => {
                    return Ok(SubmitResult {
                        accepted: false,
                        error: Some("sender account not found".to_string()),
                    });
                }
                Err(_) => {}
            }
        }

        let accepted = self.engine.mempool().submit(op);
        Ok(SubmitResult {
            accepted,
            error: if accepted {
                None
            } else {
                Some("mempool full or duplicate".to_string())
            },
        })
    }

    fn simulate_operation(&self, op: UserOperation) -> RpcResult<SimulationResult> {
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let receipt = self.engine.simulate(&op, store.as_ref());

        Ok(SimulationResult {
            success: receipt.success,
            gas_used: receipt.gas_used,
            error: receipt.error,
            events: receipt
                .events
                .iter()
                .map(|e| EventInfo {
                    emitter: account_to_base58(&e.emitter),
                    topic: String::from_utf8_lossy(&e.topic).to_string(),
                })
                .collect(),
        })
    }

    fn get_governance_proposals(&self) -> RpcResult<Vec<GovernanceProposalInfo>> {
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let gov = solen_system_contracts::governance::GovernanceContract::load(store.as_ref());

        let proposals = gov.proposals.iter().map(|p| {
            GovernanceProposalInfo {
                id: p.id,
                proposer: account_to_base58(&p.proposer),
                action: format!("{:?}", p.action),
                description: p.description.clone(),
                status: format!("{:?}", p.status),
                voting_end_epoch: p.voting_end_epoch,
                execute_after_epoch: p.execute_after_epoch,
                total_for: p.total_for.to_string(),
                total_against: p.total_against.to_string(),
                vote_count: p.votes.len(),
            }
        }).collect();

        Ok(proposals)
    }

    fn call_view(
        &self,
        contract_id: String,
        method: String,
        args: Option<String>,
    ) -> RpcResult<CallViewResult> {
        let target = parse_account_id(&contract_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;

        // Build input: method\0args
        let args_bytes = match &args {
            Some(hex) => {
                let decoded = hex_decode(hex)?;
                if decoded.len() > 1_000_000 {
                    return Err(ErrorObjectOwned::owned(-32602, "args too large (max 1MB)", None::<()>));
                }
                decoded
            }
            None => vec![],
        };
        let mut input = method.as_bytes().to_vec();
        input.push(0);
        input.extend_from_slice(&args_bytes);

        // Load contract account.
        let state = ReadonlyStateManager::new(store.as_ref());
        let account = state.get_account(&target).map_err(|e| internal_error(e))?
            .ok_or_else(|| ErrorObjectOwned::owned(-32001, "account not found", None::<()>))?;

        let zero_hash = [0u8; 32];
        if account.code_hash == zero_hash {
            return Err(ErrorObjectOwned::owned(-32001, "account has no contract code", None::<()>));
        }

        // Load bytecode.
        let code_key = {
            let mut k = b"code/".to_vec();
            k.extend_from_slice(&account.code_hash);
            k
        };
        let bytecode = store.get(&code_key)
            .map_err(|e| internal_error(e))?
            .ok_or_else(|| internal_error("bytecode not found"))?;

        // Load contract storage.
        let manifest_key = {
            let mut k = b"cs/".to_vec();
            k.extend_from_slice(&target);
            k.push(b'/');
            k.extend_from_slice(b"__keys__");
            k
        };
        let mut contract_storage = std::collections::HashMap::new();
        if let Ok(Some(manifest_data)) = store.get(&manifest_key) {
            if let Ok(keys) = serde_json::from_slice::<Vec<Vec<u8>>>(&manifest_data) {
                for key in keys {
                    let mut store_key = b"cs/".to_vec();
                    store_key.extend_from_slice(&target);
                    store_key.push(b'/');
                    store_key.extend_from_slice(&key);
                    if let Ok(Some(val)) = store.get(&store_key) {
                        contract_storage.insert(key, val);
                    }
                }
            }
        }

        // Execute in VM with read-only context (caller = zero address for view calls).
        let ctx = solen_vm::host::HostContext {
            caller: [0u8; 32],
            block_height: self.engine.height(),
            storage: contract_storage,
            events: Vec::new(),
            return_data: Vec::new(),
        };

        let vm = solen_vm::runtime::VmRuntime::new()
            .map_err(|e| internal_error(e))?;

        // Use a reduced fuel limit for view calls (read-only, no state changes).
        // Prevents CPU amplification attacks via expensive callView loops.
        const VIEW_FUEL_LIMIT: u64 = 500_000;
        match vm.execute(&account.code_hash, &bytecode, &input, ctx, Some(VIEW_FUEL_LIMIT)) {
            Ok(result) => Ok(CallViewResult {
                success: true,
                return_data: hex_encode(&result.return_data),
                gas_used: result.gas_used,
                error: None,
            }),
            Err(e) => Ok(CallViewResult {
                success: false,
                return_data: String::new(),
                gas_used: 0,
                error: Some(e.to_string()),
            }),
        }
    }

    fn chain_status(&self) -> RpcResult<ChainStatus> {
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;

        let state = ReadonlyStateManager::new(store.as_ref());

        // Read total supply stored at genesis.
        let total_allocation = match store.get(b"__total_supply__") {
            Ok(Some(data)) if data.len() >= 16 => {
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&data[..16]);
                u128::from_le_bytes(buf)
            }
            _ => 0,
        };

        // Circulation = total supply minus all system/fund account balances and staked tokens.
        use solen_types::system::*;
        let non_circulating_addresses = [
            TREASURY_ADDRESS,
            STAKING_POOL_ADDRESS,
            ECOSYSTEM_FUND_ADDRESS,
            COMMUNITY_ADDRESS,
            LIQUIDITY_ADDRESS,
            TEAM_POOL_ADDRESS,
            INVESTOR_POOL_ADDRESS,
            STAKING_ADDRESS,
            GOVERNANCE_ADDRESS,
            BRIDGE_ADDRESS,
            INTENT_ADDRESS,
            VESTING_ADDRESS,
        ];
        let non_circulating: u128 = non_circulating_addresses
            .iter()
            .map(|addr| state.get_balance(addr).unwrap_or(0))
            .sum();

        // Staked tokens are also not circulating.
        let staking =
            solen_system_contracts::staking::StakingContract::load(store.as_ref());
        let total_staked: u128 = staking.validators.iter().map(|v| v.total_stake()).sum();

        let total_circulation = total_allocation.saturating_sub(non_circulating).saturating_sub(total_staked);

        // Read governance-configurable parameters.
        let config_block_time = read_config_u64(store.as_ref(), b"__config_block_time__")
            .unwrap_or(self.engine.config().block_time_ms);
        let config_min_stake = read_config_u128(store.as_ref(), b"__config_min_validator_stake__")
            .unwrap_or(solen_system_contracts::staking::DEFAULT_MIN_VALIDATOR_STAKE);
        let config_unbonding = staking.unbonding_period
            .unwrap_or(solen_system_contracts::staking::DEFAULT_UNBONDING_PERIOD);
        let config_burn_rate = read_config_u64(store.as_ref(), b"__config_burn_rate__")
            .unwrap_or(5000); // default 50%
        let config_base_fee = read_config_u128(store.as_ref(), b"__config_base_fee__")
            .unwrap_or(1); // default: 1 base unit per gas

        Ok(ChainStatus {
            height: self.engine.height(),
            state_root: hex_encode(&store.state_root()),
            pending_ops: self.engine.mempool().len(),
            total_allocation: total_allocation.to_string(),
            total_staked: total_staked.to_string(),
            total_circulation: total_circulation.to_string(),
            config: ChainConfig {
                block_time_ms: config_block_time,
                min_validator_stake: config_min_stake.to_string(),
                unbonding_period_epochs: config_unbonding,
                epoch_length: 100,
                base_fee_per_gas: config_base_fee.to_string(),
                burn_rate_bps: config_burn_rate,
            },
        })
    }

    fn get_validators(&self) -> RpcResult<Vec<ValidatorInfo>> {
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let staking =
            solen_system_contracts::staking::StakingContract::load(store.as_ref());

        let validators: Vec<ValidatorInfo> = staking
            .validators
            .iter()
            .map(|v| ValidatorInfo {
                address: account_to_base58(&v.id),
                self_stake: v.self_stake.to_string(),
                total_delegated: v.total_delegated.to_string(),
                total_stake: v.total_stake().to_string(),
                is_active: v.is_active,
                is_genesis: v.is_genesis,
                commission_bps: v.commission_rate_bps,
            })
            .collect();

        Ok(validators)
    }

    fn get_staking_info(&self, account_id: String) -> RpcResult<StakingInfo> {
        let id = parse_account_id(&account_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let staking =
            solen_system_contracts::staking::StakingContract::load(store.as_ref());

        let delegations: Vec<DelegationInfo> = staking
            .delegations
            .iter()
            .filter(|d| d.delegator == id)
            .map(|d| DelegationInfo {
                validator: account_to_base58(&d.validator),
                amount: d.amount.to_string(),
            })
            .collect();

        let total: u128 = delegations
            .iter()
            .map(|d| d.amount.parse::<u128>().unwrap_or(0))
            .sum();

        let pending = staking
            .undelegations
            .iter()
            .filter(|u| u.delegator == id)
            .count();

        Ok(StakingInfo {
            total_delegated: total.to_string(),
            delegations,
            pending_undelegations: pending,
        })
    }

    fn get_vesting_info(&self, account_id: String) -> RpcResult<VestingInfo> {
        let id = parse_account_id(&account_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let vesting =
            solen_system_contracts::vesting::VestingContract::load(store.as_ref());

        match vesting.get_schedule(&id) {
            Some(schedule) => {
                // Read current epoch from chain meta.
                let current_epoch = match store.get(b"__chain_meta__") {
                    Ok(Some(data)) if data.len() >= 16 => {
                        let mut h = [0u8; 8];
                        h.copy_from_slice(&data[..8]);
                        u64::from_le_bytes(h) / 100
                    }
                    _ => 0,
                };

                Ok(VestingInfo {
                    has_schedule: true,
                    total_amount: schedule.total_amount.to_string(),
                    vested: schedule.vested_at(current_epoch).to_string(),
                    claimed: schedule.claimed.to_string(),
                    claimable: schedule.claimable_at(current_epoch).to_string(),
                    vesting_type: format!("{:?}", schedule.vesting_type),
                })
            }
            None => Ok(VestingInfo {
                has_schedule: false,
                total_amount: "0".into(),
                vested: "0".into(),
                claimed: "0".into(),
                claimable: "0".into(),
                vesting_type: "".into(),
            }),
        }
    }

    fn submit_intent(&self, req: IntentRequest) -> RpcResult<IntentSubmitResult> {
        if req.constraints.len() > 100 {
            return Err(ErrorObjectOwned::owned(-32602, "too many constraints (max 100)", None::<()>));
        }
        let sender = parse_account_id(&req.sender)?;
        let signature = hex_decode(&req.signature)?;
        let max_fee: u128 = req.max_fee.parse().map_err(|_| {
            ErrorObjectOwned::owned(-32602, "invalid max_fee", None::<()>)
        })?;
        let tip: u128 = req.tip.parse().map_err(|_| {
            ErrorObjectOwned::owned(-32602, "invalid tip", None::<()>)
        })?;

        // Convert constraints from RPC format to internal format.
        let constraints: Result<Vec<Constraint>, _> = req.constraints.iter().map(|c| {
            constraint_from_info(c)
        }).collect();
        let constraints = constraints?;

        let intent = Intent {
            id: 0, // assigned by pool
            sender,
            constraints,
            max_fee,
            expiry_height: req.expiry_height,
            signature,
            tip,
        };

        let pool = self.engine.intent_pool();
        match pool.submit(intent) {
            Ok(id) => Ok(IntentSubmitResult {
                accepted: true,
                intent_id: Some(id),
                error: None,
            }),
            Err(e) => Ok(IntentSubmitResult {
                accepted: false,
                intent_id: None,
                error: Some(e.to_string()),
            }),
        }
    }

    fn get_pending_intents(&self, limit: Option<usize>) -> RpcResult<Vec<IntentInfo>> {
        let pool = self.engine.intent_pool();
        let pending = pool.pending_intents();
        let limit = limit.unwrap_or(50).min(500);

        let intents: Vec<IntentInfo> = pending.into_iter().take(limit).map(|i| {
            IntentInfo {
                id: i.id,
                sender: account_to_base58(&i.sender),
                constraints: i.constraints.iter().map(|c| constraint_to_info(c)).collect(),
                max_fee: i.max_fee.to_string(),
                expiry_height: i.expiry_height,
                tip: i.tip.to_string(),
                status: "Pending".to_string(),
            }
        }).collect();

        Ok(intents)
    }

    fn submit_solution(&self, req: SolutionRequest) -> RpcResult<SolutionSubmitResult> {
        // Rate limit: max 20 solution submissions per second.
        if !RpcRateLimiter::check(&self.rate_limiter.submit_solutions, 20) {
            return Ok(SolutionSubmitResult {
                accepted: false,
                error: Some("rate limited — too many solution submissions".to_string()),
            });
        }

        let solver = parse_account_id(&req.solver)?;
        let claimed_tip: u128 = req.claimed_tip.parse().map_err(|_| {
            ErrorObjectOwned::owned(-32602, "invalid claimed_tip", None::<()>)
        })?;

        // Solver signature is mandatory for RPC submissions.
        // Only the built-in solver (in-process) may omit the signature.
        let sig_bytes = match &req.signature {
            Some(hex) if !hex.is_empty() => hex_decode(hex).unwrap_or_default(),
            _ => {
                return Ok(SolutionSubmitResult {
                    accepted: false,
                    error: Some("solver signature is required".to_string()),
                });
            }
        };

        let solution = solen_intents::types::Solution {
            intent_id: req.intent_id,
            solver,
            operations: req.operations,
            claimed_tip,
            score: req.score,
            signature: sig_bytes,
        };

        let pool = self.engine.intent_pool();
        match pool.submit_solution(solution) {
            Ok(()) => Ok(SolutionSubmitResult {
                accepted: true,
                error: None,
            }),
            Err(e) => Ok(SolutionSubmitResult {
                accepted: false,
                error: Some(e.to_string()),
            }),
        }
    }

    fn check_sponsorship(&self, op: UserOperation) -> RpcResult<SponsorshipResult> {
        // Check if any registered paymaster contract is willing to sponsor this operation.
        // Paymasters are contracts that implement a `willSponsor` view method.
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;

        // Look for paymaster registry in state.
        let paymaster_key = b"__paymasters__";
        let paymasters: Vec<[u8; 32]> = match store.get(paymaster_key) {
            Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or_default(),
            _ => vec![],
        };

        if paymasters.is_empty() {
            return Ok(SponsorshipResult {
                sponsored: false,
                paymaster: None,
                max_gas: None,
                reason: Some("no paymasters registered".to_string()),
            });
        }

        // Simulate a view call to each paymaster's willSponsor method.
        let op_bytes = serde_json::to_vec(&op).unwrap_or_default();
        for pm in &paymasters {
            let state = ReadonlyStateManager::new(store.as_ref());
            let account = match state.get_account(pm) {
                Ok(Some(a)) if a.code_hash != [0u8; 32] => a,
                _ => continue,
            };

            let mut input = b"willSponsor\0".to_vec();
            input.extend_from_slice(&op_bytes);

            let code_key = {
                let mut k = b"code/".to_vec();
                k.extend_from_slice(&account.code_hash);
                k
            };
            let bytecode = match store.get(&code_key) {
                Ok(Some(b)) => b,
                _ => continue,
            };

            let ctx = solen_vm::host::HostContext {
                caller: [0u8; 32],
                block_height: self.engine.height(),
                storage: std::collections::HashMap::new(),
                events: Vec::new(),
                return_data: Vec::new(),
            };

            let vm = match solen_vm::runtime::VmRuntime::new() {
                Ok(v) => v,
                Err(_) => continue,
            };

            if let Ok(result) = vm.execute(&account.code_hash, &bytecode, &input, ctx, None) {
                if !result.return_data.is_empty() && result.return_data[0] == 1 {
                    let max_gas = if result.return_data.len() >= 17 {
                        let mut buf = [0u8; 16];
                        buf.copy_from_slice(&result.return_data[1..17]);
                        Some(u128::from_le_bytes(buf).to_string())
                    } else {
                        None
                    };

                    return Ok(SponsorshipResult {
                        sponsored: true,
                        paymaster: Some(account_to_base58(pm)),
                        max_gas,
                        reason: None,
                    });
                }
            }
        }

        Ok(SponsorshipResult {
            sponsored: false,
            paymaster: None,
            max_gas: None,
            reason: Some("no paymaster willing to sponsor".to_string()),
        })
    }

    fn get_rollup_status(&self, rollup_id: u64) -> RpcResult<RollupStatusInfo> {
        // Check in-memory proof registry first.
        let registry = self.engine.proof_registry();
        let registry = registry.read().map_err(|e| internal_error(e.to_string()))?;
        let last_state_root = registry.last_state_root(rollup_id);

        if last_state_root.is_some() {
            let batch_count = registry.batch_count(rollup_id);
            let last_batch_index = if batch_count > 0 {
                registry.get_verified_batches(rollup_id, 1).first().map(|b| b.batch_index)
            } else {
                None
            };
            return Ok(RollupStatusInfo {
                rollup_id,
                registered: true,
                last_verified_state_root: last_state_root.map(|r| hex_encode(&r)),
                last_batch_index,
            });
        }
        drop(registry);

        // Fall back to on-chain registration state.
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let reg_key = format!("__rollup_{}__", rollup_id);
        let registered = match store.get(reg_key.as_bytes()) {
            Ok(Some(_)) => true,
            _ => false,
        };

        Ok(RollupStatusInfo {
            rollup_id,
            registered,
            last_verified_state_root: None,
            last_batch_index: None,
        })
    }

    fn submit_batch(&self, req: BatchSubmitRequest) -> RpcResult<BatchSubmitResult> {
        let state_root = parse_hash(&req.state_root)?;
        let data_hash = parse_hash(&req.data_hash)?;
        let proof = hex_decode(&req.proof)?;
        if proof.len() > 1_000_000 {
            return Err(ErrorObjectOwned::owned(-32602, "proof too large (max 1MB)", None::<()>));
        }

        let commitment = BatchCommitment {
            rollup_id: req.rollup_id,
            batch_index: req.batch_index,
            state_root,
            data_hash,
            proof,
        };

        // If the rollup is registered on-chain but not in the in-memory registry,
        // auto-register it so batch verification can proceed.
        {
            let registry = self.engine.proof_registry();
            let mut registry = registry.write().map_err(|e| internal_error(e.to_string()))?;
            if registry.last_state_root(req.rollup_id).is_none() {
                let store = self.engine.store();
                let store = store.read().map_err(|e| internal_error(e.to_string()))?;
                let reg_key = format!("__rollup_{}__", req.rollup_id);
                if let Ok(Some(data)) = store.get(reg_key.as_bytes()) {
                    if let Ok(info) = serde_json::from_slice::<serde_json::Value>(&data) {
                        let proof_type = info["proof_type"].as_str().unwrap_or("mock");
                        let genesis_root = if let Some(hex) = info["genesis_state_root"].as_str() {
                            let bytes: Vec<u8> = (0..hex.len()).step_by(2)
                                .filter_map(|i| u8::from_str_radix(&hex[i..i+2], 16).ok())
                                .collect();
                            let mut root = [0u8; 32];
                            if bytes.len() == 32 { root.copy_from_slice(&bytes); }
                            root
                        } else {
                            [0u8; 32]
                        };
                        let _ = registry.register_rollup(req.rollup_id, proof_type, genesis_root);
                    }
                }
            }
        }

        let registry = self.engine.proof_registry();
        let mut registry = registry.write().map_err(|e| internal_error(e.to_string()))?;

        match registry.verify_batch(&commitment) {
            Ok(verified) => Ok(BatchSubmitResult {
                accepted: true,
                verified,
                error: if verified { None } else { Some("proof verification failed".to_string()) },
            }),
            Err(e) => Ok(BatchSubmitResult {
                accepted: false,
                verified: false,
                error: Some(e.to_string()),
            }),
        }
    }

    fn get_snapshot(&self) -> RpcResult<SnapshotInfo> {
        // Rate limit: max 2 snapshot requests per second (10 MB responses).
        if !RpcRateLimiter::check(&self.rate_limiter.snapshots, 2) {
            return Err(ErrorObjectOwned::owned(
                -32000,
                "rate limited — snapshot requests throttled",
                None::<()>,
            ));
        }

        let height = self.engine.height();

        // Return cached snapshot if it's recent enough.
        {
            let cache = self.snapshot_cache.lock().unwrap();
            if let Some(ref cached) = *cache {
                if height.saturating_sub(cached.height) < SNAPSHOT_CACHE_INTERVAL {
                    return Ok(cached.info.clone());
                }
            }
        }

        // Generate a fresh snapshot.
        // Take a CoW snapshot of the store so we don't hold the write lock
        // during the expensive scan + compress. Block production continues.
        let (store_snapshot, epoch) = {
            let store = self.engine.store();
            let store = store.read().map_err(|e| internal_error(e.to_string()))?;
            let snap = store.snapshot();
            let epoch = {
                let chain = self.engine.chain();
                let chain = chain.read().map_err(|e| internal_error(e.to_string()))?;
                chain.last().map(|b| b.header.epoch).unwrap_or(0)
            };
            (snap, epoch)
            // store read lock released here
        };

        let data = solen_consensus::snapshot::create_snapshot(store_snapshot.as_ref(), height, epoch)
            .map_err(|e| internal_error(e.to_string()))?;

        // Cap snapshot size before base64 encoding. The JSON-RPC response limit
        // is 10 MB, and base64 adds ~33% overhead, so max compressed snapshot is ~7 MB.
        // For larger states, nodes should use direct P2P sync instead of RPC snapshots.
        const MAX_SNAPSHOT_RESPONSE: usize = 7 * 1024 * 1024; // 7 MB compressed
        if data.len() > MAX_SNAPSHOT_RESPONSE {
            return Err(internal_error(format!(
                "snapshot too large ({} MB), use direct sync instead",
                data.len() / (1024 * 1024)
            )));
        }

        let meta = solen_consensus::snapshot::read_snapshot_meta(&data)
            .map_err(|e| internal_error(e.to_string()))?;

        let entries = store_snapshot.len() as u64;
        let compressed_bytes = data.len() - 56;
        let b64 = base64_encode(&data);

        // Include the latest finalized checkpoint for snapshot verification.
        let checkpoint = {
            let cp_store = self.engine.finalized_checkpoints();
            let cp = cp_store.read().unwrap();
            cp.latest.as_ref().map(|fc| SnapshotCheckpoint {
                height: fc.height,
                epoch: fc.epoch,
                block_hash: hex_encode(&fc.block_hash),
                state_root: hex_encode(&fc.state_root),
                attestations: fc.attestations.iter()
                    .map(|(v, sig)| (account_to_base58(v), hex_encode(sig)))
                    .collect(),
            })
        };

        let info = SnapshotInfo {
            height,
            epoch,
            state_root: hex_encode(&meta.state_root),
            entries,
            compressed_bytes,
            uncompressed_bytes: meta.uncompressed_size,
            data: b64,
            checkpoint,
        };

        // Cache it.
        {
            let mut cache = self.snapshot_cache.lock().unwrap();
            *cache = Some(CachedSnapshot { height, info: info.clone() });
        }

        Ok(info)
    }

    fn get_rollup_batches(&self, rollup_id: u64, limit: Option<usize>) -> RpcResult<Vec<VerifiedBatchInfo>> {
        let registry = self.engine.proof_registry();
        let registry = registry.read().map_err(|e| internal_error(e.to_string()))?;
        let batches = registry.get_verified_batches(rollup_id, limit.unwrap_or(50).min(500));
        Ok(batches
            .into_iter()
            .map(|b| VerifiedBatchInfo {
                rollup_id: b.rollup_id,
                batch_index: b.batch_index,
                state_root: hex_encode(&b.state_root),
                data_hash: hex_encode(&b.data_hash),
                pre_state_root: hex_encode(&b.pre_state_root),
            })
            .collect())
    }
}

fn parse_hash(s: &str) -> RpcResult<[u8; 32]> {
    let bytes = hex_decode(s)?;
    if bytes.len() != 32 {
        return Err(ErrorObjectOwned::owned(
            -32602,
            format!("hash must be 32 bytes, got {}", bytes.len()),
            None::<()>,
        ));
    }
    let mut h = [0u8; 32];
    h.copy_from_slice(&bytes);
    Ok(h)
}

fn constraint_from_info(c: &ConstraintInfo) -> RpcResult<Constraint> {
    match c {
        ConstraintInfo::MinBalance { account, min_amount } => Ok(Constraint::MinBalance {
            account: parse_account_id(account)?,
            min_amount: min_amount.parse().map_err(|_| {
                ErrorObjectOwned::owned(-32602, "invalid min_amount", None::<()>)
            })?,
        }),
        ConstraintInfo::MaxSpend { account, max_amount } => Ok(Constraint::MaxSpend {
            account: parse_account_id(account)?,
            max_amount: max_amount.parse().map_err(|_| {
                ErrorObjectOwned::owned(-32602, "invalid max_amount", None::<()>)
            })?,
        }),
        ConstraintInfo::RequireTransfer { from, to, min_amount } => Ok(Constraint::RequireTransfer {
            from: parse_account_id(from)?,
            to: parse_account_id(to)?,
            min_amount: min_amount.parse().map_err(|_| {
                ErrorObjectOwned::owned(-32602, "invalid min_amount", None::<()>)
            })?,
        }),
        ConstraintInfo::RequireCall { target, method } => Ok(Constraint::RequireCall {
            target: parse_account_id(target)?,
            method: method.clone(),
        }),
        ConstraintInfo::Custom { verifier, data } => Ok(Constraint::Custom {
            verifier: parse_account_id(verifier)?,
            data: hex_decode(data)?,
        }),
    }
}

fn constraint_to_info(c: &Constraint) -> ConstraintInfo {
    match c {
        Constraint::MinBalance { account, min_amount } => ConstraintInfo::MinBalance {
            account: account_to_base58(account),
            min_amount: min_amount.to_string(),
        },
        Constraint::MaxSpend { account, max_amount } => ConstraintInfo::MaxSpend {
            account: account_to_base58(account),
            max_amount: max_amount.to_string(),
        },
        Constraint::RequireTransfer { from, to, min_amount } => ConstraintInfo::RequireTransfer {
            from: account_to_base58(from),
            to: account_to_base58(to),
            min_amount: min_amount.to_string(),
        },
        Constraint::RequireCall { target, method } => ConstraintInfo::RequireCall {
            target: account_to_base58(target),
            method: method.clone(),
        },
        Constraint::Custom { verifier, data } => ConstraintInfo::Custom {
            verifier: account_to_base58(verifier),
            data: hex_encode(data),
        },
    }
}

fn block_to_info(block: &solen_consensus::engine::FinalizedBlock) -> BlockInfo {
    BlockInfo {
        height: block.header.height,
        epoch: block.header.epoch,
        parent_hash: hex_encode(&block.header.parent_hash),
        state_root: hex_encode(&block.header.state_root),
        transactions_root: hex_encode(&block.header.transactions_root),
        receipts_root: hex_encode(&block.header.receipts_root),
        proposer: account_to_base58(&block.header.proposer),
        timestamp_ms: block.header.timestamp_ms,
        tx_count: block.result.receipts.len(),
        gas_used: block.result.gas_used,
    }
}
