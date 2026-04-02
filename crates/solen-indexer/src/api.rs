//! REST API for the block explorer.

use std::sync::{Arc, RwLock};

use axum::extract::{Path, Query, State};
use axum::response::Json;
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use solen_consensus::engine::ConsensusEngine;
use tracing::info;

use crate::store::{IndexStore, IndexedBatch, IndexedBlock, IndexedEvent, IndexedIntent, IndexedRollup, IndexedTx};

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<RwLock<IndexStore>>,
    pub engine: Option<Arc<ConsensusEngine>>,
}

#[derive(Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub latest_height: u64,
    pub total_blocks: usize,
    pub total_txs: usize,
    pub total_events: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatorResponse {
    pub id: String,
    pub stake: String,
    pub self_stake: String,
    pub delegated: String,
    pub status: String,
    pub missed_blocks: u64,
    pub commission_pct: String,
    pub is_genesis: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidatorSetResponse {
    pub validators: Vec<ValidatorResponse>,
    pub total_active_stake: String,
    pub active_count: usize,
    pub total_count: usize,
}

/// Build the explorer REST API router.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/status", get(get_status))
        .route("/api/blocks", get(get_blocks))
        .route("/api/blocks/:height", get(get_block))
        .route("/api/blocks/:height/txs", get(get_block_txs))
        .route("/api/tx/:height/:index", get(get_tx))
        .route("/api/txs", get(get_recent_txs))
        .route("/api/accounts/:account/txs", get(get_account_txs))
        .route("/api/events", get(get_events))
        .route("/api/validators", get(get_validators))
        .route("/api/validators/stats", get(get_validator_stats))
        .route("/api/accounts/:account/tokens", get(get_account_tokens))
        .route("/api/contracts/:code_hash/source", get(get_contract_source).post(publish_contract_source))
        .route("/api/contracts", get(get_contracts))
        .route("/api/contracts/:contract/holders", get(get_token_holders))
        .route("/api/intents", get(get_fulfilled_intents))
        .route("/api/rollups", get(get_rollups))
        .route("/api/rollups/:rollup_id", get(get_rollup))
        .route("/api/rollups/:rollup_id/batches", get(get_rollup_batches))
        .with_state(state)
}

async fn get_status(State(state): State<ApiState>) -> Json<StatusResponse> {
    let store = state.store.read().unwrap();
    Json(StatusResponse {
        latest_height: store.latest_height,
        total_blocks: store.blocks.len(),
        total_txs: store.transactions.len(),
        total_events: store.events.len(),
    })
}

async fn get_blocks(
    State(state): State<ApiState>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<IndexedBlock>> {
    let store = state.store.read().unwrap();
    let blocks: Vec<IndexedBlock> = store
        .get_recent_blocks_paged(params.limit, params.offset)
        .into_iter()
        .cloned()
        .collect();
    Json(blocks)
}

async fn get_block(
    State(state): State<ApiState>,
    Path(height): Path<u64>,
) -> Json<Option<IndexedBlock>> {
    let store = state.store.read().unwrap();
    Json(store.get_block(height).cloned())
}

async fn get_block_txs(
    State(state): State<ApiState>,
    Path(height): Path<u64>,
) -> Json<Vec<IndexedTx>> {
    let store = state.store.read().unwrap();
    Json(store.get_block_txs(height).into_iter().cloned().collect())
}

async fn get_tx(
    State(state): State<ApiState>,
    Path((height, index)): Path<(u64, usize)>,
) -> Json<Option<IndexedTx>> {
    let store = state.store.read().unwrap();
    Json(store.get_tx(height, index).cloned())
}

async fn get_recent_txs(
    State(state): State<ApiState>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<IndexedTx>> {
    let store = state.store.read().unwrap();
    Json(store.get_recent_txs_paged(params.limit, params.offset).into_iter().cloned().collect())
}

async fn get_account_txs(
    State(state): State<ApiState>,
    Path(account): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<IndexedTx>> {
    let store = state.store.read().unwrap();
    let txs: Vec<IndexedTx> = store
        .get_account_txs_paged(&account, params.limit, params.offset)
        .into_iter()
        .cloned()
        .collect();
    Json(txs)
}

async fn get_events(
    State(state): State<ApiState>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<IndexedEvent>> {
    let store = state.store.read().unwrap();
    let events: Vec<IndexedEvent> = store
        .get_recent_events_paged(params.limit, params.offset)
        .into_iter()
        .cloned()
        .collect();
    Json(events)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn get_validators(State(state): State<ApiState>) -> Json<ValidatorSetResponse> {
    let Some(engine) = &state.engine else {
        return Json(ValidatorSetResponse {
            validators: vec![],
            total_active_stake: "0".to_string(),
            active_count: 0,
            total_count: 0,
        });
    };

    let vs = engine.validator_set();
    let vs = vs.read().unwrap();

    // Load staking contract for commission and delegation data.
    let staking = {
        let store = engine.store();
        let store = store.read().unwrap();
        solen_system_contracts::staking::StakingContract::load(store.as_ref())
    };

    let validators: Vec<ValidatorResponse> = vs
        .all()
        .iter()
        .map(|v| {
            let staking_info = staking.get_validator(&v.id);
            let commission_bps = staking_info.map(|s| s.commission_rate_bps).unwrap_or(1000);
            let self_stake = staking_info.map(|s| s.self_stake).unwrap_or(v.stake);
            let delegated = staking_info.map(|s| s.total_delegated).unwrap_or(0);
            let is_genesis = staking_info.map(|s| s.is_genesis).unwrap_or(false);

            ValidatorResponse {
                id: hex_encode(&v.id),
                stake: v.stake.to_string(),
                self_stake: self_stake.to_string(),
                delegated: delegated.to_string(),
                status: match v.status {
                    solen_consensus::validator::ValidatorStatus::Active => "Active".to_string(),
                    solen_consensus::validator::ValidatorStatus::Jailed => "Jailed".to_string(),
                    solen_consensus::validator::ValidatorStatus::Exiting => "Exiting".to_string(),
                },
                missed_blocks: v.missed_blocks,
                commission_pct: format!("{:.1}%", commission_bps as f64 / 100.0),
                is_genesis,
            }
        })
        .collect();

    let total_count = validators.len();

    Json(ValidatorSetResponse {
        validators,
        total_active_stake: vs.total_active_stake().to_string(),
        active_count: vs.active_count(),
        total_count,
    })
}

/// Start the explorer API server.
#[derive(Serialize)]
struct ValidatorStats {
    validator: String,
    blocks_proposed: u64,
    last_proposed_height: u64,
    uptime_pct: f64,
}

async fn get_validator_stats(
    State(state): State<ApiState>,
) -> Json<Vec<ValidatorStats>> {
    let store = state.store.read().unwrap();
    let total_blocks = store.latest_height.max(1);

    // Get all known proposers.
    let mut stats: Vec<ValidatorStats> = store.blocks_proposed
        .iter()
        .map(|(validator, &count)| {
            let last = store.last_proposed.get(validator).copied().unwrap_or(0);
            // Approximate expected blocks: total / active_validators.
            // We don't know exact count, so just show raw numbers and
            // percentage of all blocks this validator proposed.
            let uptime_pct = (count as f64 / total_blocks as f64) * 100.0;
            ValidatorStats {
                validator: validator.clone(),
                blocks_proposed: count,
                last_proposed_height: last,
                uptime_pct,
            }
        })
        .collect();

    stats.sort_by(|a, b| b.blocks_proposed.cmp(&a.blocks_proposed));
    Json(stats)
}

async fn get_token_holders(
    State(state): State<ApiState>,
    Path(contract): Path<String>,
) -> Json<Vec<String>> {
    let store = state.store.read().unwrap();
    Json(store.get_token_holders(&contract))
}

async fn get_contract_source(
    State(state): State<ApiState>,
    Path(code_hash): Path<String>,
) -> Json<Option<crate::store::ContractSource>> {
    let store = state.store.read().unwrap();
    Json(store.contract_sources.get(&code_hash).cloned())
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct PublishSourceRequest {
    code_hash: String,
    source_code: String,
    language: Option<String>,
    compiler_version: Option<String>,
}

async fn publish_contract_source(
    State(state): State<ApiState>,
    Path(code_hash): Path<String>,
    Json(body): Json<PublishSourceRequest>,
) -> Json<serde_json::Value> {
    let source_code = body.source_code.clone();
    let language = body.language.unwrap_or_else(|| "rust".to_string());
    let compiler_version = body.compiler_version.unwrap_or_else(|| "unknown".to_string());
    let expected_hash = code_hash.clone();

    // Try to verify by compiling the source.
    let verified = if language == "rust" {
        verify_rust_contract(&source_code, &expected_hash)
    } else {
        false
    };

    let mut store = state.store.write().unwrap();
    let source = crate::store::ContractSource {
        code_hash: code_hash.clone(),
        source_code,
        language,
        compiler_version,
        published_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        verified,
    };

    store.contract_sources.insert(code_hash, source);
    Json(serde_json::json!({"success": true, "verified": verified}))
}

/// Compile Rust contract source and verify bytecode hash matches.
fn verify_rust_contract(source_code: &str, expected_hash: &str) -> bool {
    use std::process::Command;

    let tmp = match tempfile::TempDir::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "contract verification: failed to create temp dir");
            return false;
        }
    };

    let src_dir = tmp.path().join("src");
    if std::fs::create_dir_all(&src_dir).is_err() {
        return false;
    }

    // Write Cargo.toml.
    let cargo_toml = r#"[package]
name = "verify-contract"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
solen-contract-sdk = { path = "${SDK_PATH}" }

[profile.release]
opt-level = "z"
lto = true
strip = true
"#;
    // Find the SDK path — check common locations.
    let candidates = [
        std::path::PathBuf::from("/opt/solen/crates/solen-contract-sdk"),
        std::path::PathBuf::from("/root/solen/crates/solen-contract-sdk"),
        std::path::PathBuf::from("/home/solen/solen/crates/solen-contract-sdk"),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .map(|p| p.join("../../crates/solen-contract-sdk"))
            .unwrap_or_default(),
        std::path::PathBuf::from("crates/solen-contract-sdk"),
    ];
    for c in &candidates {
        let exists = c.join("Cargo.toml").exists();
        tracing::info!(path = %c.display(), exists, "contract verification: checking SDK candidate");
    }
    let sdk_path = candidates.iter()
        .find(|p| p.join("Cargo.toml").exists())
        .cloned()
        .unwrap_or_else(|| std::path::PathBuf::from("crates/solen-contract-sdk"));

    tracing::info!(sdk_path = %sdk_path.display(), "contract verification: using SDK path");

    let cargo_toml = cargo_toml.replace("${SDK_PATH}", &sdk_path.to_string_lossy());

    if std::fs::write(tmp.path().join("Cargo.toml"), &cargo_toml).is_err() {
        return false;
    }

    if std::fs::write(src_dir.join("lib.rs"), source_code).is_err() {
        return false;
    }

    // Find cargo binary.
    let cargo_bin = [
        std::path::PathBuf::from("/home/solen/.cargo/bin/cargo"),
        std::path::PathBuf::from("/root/.cargo/bin/cargo"),
        std::path::PathBuf::from("cargo"),
    ].into_iter()
        .find(|p| p.exists() || p.to_str() == Some("cargo"))
        .unwrap_or_else(|| std::path::PathBuf::from("cargo"));

    tracing::info!(cargo = %cargo_bin.display(), "contract verification: compiling");

    // Compile.
    let output = Command::new(&cargo_bin)
        .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
        .current_dir(tmp.path())
        .env("CARGO_TARGET_DIR", tmp.path().join("target"))
        .output();

    let output = match output {
        Ok(o) => o,
        Err(_) => return false,
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!(stderr = %stderr, "contract verification: compilation failed");
        return false;
    }

    // Find the WASM output.
    let wasm_path = tmp.path()
        .join("target/wasm32-unknown-unknown/release/verify_contract.wasm");

    let wasm_bytes = match std::fs::read(&wasm_path) {
        Ok(b) => b,
        Err(_) => return false,
    };

    // Hash and compare.
    let hash = solen_crypto::blake3_hash(&wasm_bytes);
    let hash_hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();

    if hash_hex == expected_hash {
        tracing::info!("contract verification: VERIFIED — bytecode matches");
        true
    } else {
        tracing::warn!(
            expected = expected_hash,
            actual = hash_hex,
            "contract verification: hash mismatch"
        );
        false
    }
}

async fn get_account_tokens(
    State(state): State<ApiState>,
    Path(account): Path<String>,
) -> Json<Vec<String>> {
    let store = state.store.read().unwrap();
    Json(store.get_account_tokens(&account))
}

async fn get_contracts(
    State(state): State<ApiState>,
) -> Json<Vec<String>> {
    let store = state.store.read().unwrap();
    Json(store.get_contracts())
}

async fn get_fulfilled_intents(
    State(state): State<ApiState>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<IndexedIntent>> {
    let store = state.store.read().unwrap();
    Json(store.get_recent_intents(params.limit).into_iter().cloned().collect())
}

async fn get_rollups(State(state): State<ApiState>) -> Json<Vec<IndexedRollup>> {
    let store = state.store.read().unwrap();
    Json(store.get_rollups().into_iter().cloned().collect())
}

#[derive(Serialize)]
struct RollupDetailResponse {
    #[serde(flatten)]
    rollup: IndexedRollup,
    total_batches: usize,
    latest_batch: Option<IndexedBatch>,
}

async fn get_rollup(
    State(state): State<ApiState>,
    Path(rollup_id): Path<u64>,
) -> Json<Option<RollupDetailResponse>> {
    let store = state.store.read().unwrap();
    let rollup = match store.get_rollup(rollup_id) {
        Some(r) => r.clone(),
        None => return Json(None),
    };
    // Check indexed batches first, then fall back to proof registry.
    let mut total_batches = store.get_rollup_batch_count(rollup_id);
    let mut latest_batch = store.get_rollup_batches(rollup_id, 1).first().cloned().cloned();
    drop(store);

    if total_batches == 0 {
        if let Some(engine) = &state.engine {
            let registry_arc = engine.proof_registry();
            let reg = registry_arc.read().unwrap();
            total_batches = reg.batch_count(rollup_id);
            if let Some(b) = reg.get_verified_batches(rollup_id, 1).first() {
                latest_batch = Some(IndexedBatch {
                    rollup_id: b.rollup_id,
                    batch_index: b.batch_index,
                    state_root: hex_encode(&b.state_root),
                    data_hash: hex_encode(&b.data_hash),
                    verified: true,
                    block_height: 0,
                    tx_index: 0,
                });
            }
        }
    }

    Json(Some(RollupDetailResponse {
        rollup,
        total_batches,
        latest_batch,
    }))
}

async fn get_rollup_batches(
    State(state): State<ApiState>,
    Path(rollup_id): Path<u64>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<IndexedBatch>> {
    // First check the indexed store (from on-chain events).
    let store = state.store.read().unwrap();
    let indexed = store.get_rollup_batches(rollup_id, params.limit);
    if !indexed.is_empty() {
        return Json(indexed.into_iter().cloned().collect());
    }
    drop(store);

    // Fall back to the engine's proof registry (for batches submitted via RPC).
    if let Some(engine) = &state.engine {
        let registry_arc = engine.proof_registry();
        let reg = registry_arc.read().unwrap();
        let batches: Vec<IndexedBatch> = reg
            .get_verified_batches(rollup_id, params.limit)
            .into_iter()
            .map(|b| IndexedBatch {
                rollup_id: b.rollup_id,
                batch_index: b.batch_index,
                state_root: hex_encode(&b.state_root),
                data_hash: hex_encode(&b.data_hash),
                verified: true,
                block_height: 0,
                tx_index: 0,
            })
            .collect();
        if !batches.is_empty() {
            return Json(batches);
        }
    }

    Json(vec![])
}

pub async fn start_explorer_api(
    addr: std::net::SocketAddr,
    store: Arc<RwLock<IndexStore>>,
    engine: Option<Arc<ConsensusEngine>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = ApiState { store, engine };
    let app = router(state);

    info!(%addr, "explorer API started");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
