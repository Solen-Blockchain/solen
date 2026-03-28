//! REST API for the block explorer.

use std::sync::{Arc, RwLock};

use axum::extract::{Path, Query, State};
use axum::response::Json;
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::store::{IndexStore, IndexedBlock, IndexedEvent, IndexedTx};

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<RwLock<IndexStore>>,
}

#[derive(Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    limit: usize,
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

/// Build the explorer REST API router.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/status", get(get_status))
        .route("/api/blocks", get(get_blocks))
        .route("/api/blocks/{height}", get(get_block))
        .route("/api/accounts/{account}/txs", get(get_account_txs))
        .route("/api/events", get(get_events))
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
        .get_recent_blocks(params.limit)
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

async fn get_account_txs(
    State(state): State<ApiState>,
    Path(account): Path<String>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<IndexedTx>> {
    let store = state.store.read().unwrap();
    let txs: Vec<IndexedTx> = store
        .get_account_txs(&account, params.limit)
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
        .get_recent_events(params.limit)
        .into_iter()
        .cloned()
        .collect();
    Json(events)
}

/// Start the explorer API server.
pub async fn start_explorer_api(
    addr: std::net::SocketAddr,
    store: Arc<RwLock<IndexStore>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let state = ApiState { store };
    let app = router(state);

    info!(%addr, "explorer API started");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
