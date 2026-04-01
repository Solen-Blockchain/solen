//! REST API for the block explorer.

use std::sync::{Arc, RwLock};

use axum::extract::{Path, Query, State};
use axum::response::Json;
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use solen_consensus::engine::ConsensusEngine;
use tracing::info;

use crate::store::{IndexStore, IndexedBlock, IndexedEvent, IndexedTx};

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
    let mut store = state.store.write().unwrap();

    // Accept any code hash — source can be published for any deployed contract.

    let source = crate::store::ContractSource {
        code_hash: code_hash.clone(),
        source_code: body.source_code,
        language: body.language.unwrap_or_else(|| "rust".to_string()),
        compiler_version: body.compiler_version.unwrap_or_else(|| "unknown".to_string()),
        published_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        verified: false,
    };

    store.contract_sources.insert(code_hash, source);
    Json(serde_json::json!({"success": true}))
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
