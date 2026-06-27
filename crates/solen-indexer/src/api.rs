//! REST API for the block explorer.

use std::sync::{Arc, RwLock};

use axum::extract::{Path, Query, State};
use axum::response::{Json, IntoResponse};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use solen_consensus::engine::ConsensusEngine;
use tracing::info;

use solen_types::encoding::{account_to_base58, hex_encode, parse_address};

use crate::store::{IndexStore, IndexedBatch, IndexedBlock, IndexedEvent, IndexedIntent, IndexedRollup, IndexedTx};

/// Pre-sorted richlist rows `(id, balance, staked)` with the time the snapshot
/// was built. Lets `/api/richlist` serve a TTL-cached result instead of
/// scanning + deserializing + sorting the whole account table per request.
type RichlistCache = Arc<RwLock<Option<(std::time::Instant, Vec<([u8; 32], u128, u128)>)>>>;

#[derive(Clone)]
pub struct ApiState {
    pub store: Arc<RwLock<IndexStore>>,
    pub engine: Option<Arc<ConsensusEngine>>,
    /// Rolling buffer feeding `/api/stsolen/apy`. Empty until the sampler
    /// task fills it.
    pub stsolen_apy: Arc<crate::stsolen_apy::ApySamples>,
    /// TTL cache for the richlist (see `RichlistCache`).
    pub richlist_cache: RichlistCache,
}

#[derive(Deserialize)]
pub struct PaginationParams {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

/// Query params for `/api/events`. All filters are optional — unset = no
/// filtering on that dimension. `contract` accepts hex (64 chars, with or
/// without `0x`) or Base58; it's normalized to the Base58 form events are
/// stored under.
#[derive(Deserialize)]
pub struct EventsQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
    contract: Option<String>,
    topic: Option<String>,
    from_height: Option<u64>,
    to_height: Option<u64>,
}

fn default_limit() -> usize {
    20
}

/// Hard upper bound on any page size, regardless of the client-supplied
/// `limit`. Prevents a single request from forcing the indexer to clone and
/// serialize its entire in-memory log under the store read lock.
const MAX_PAGE_LIMIT: usize = 100;

/// Cap on how many of an account's transactions the transfers endpoint scans
/// per request. Bounds per-request work; transfers older than this window are
/// reached via `offset` paging rather than a single unbounded scan.
const MAX_TRANSFER_SCAN: usize = 2000;

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
        .route("/api/tx/hash/:tx_hash", get(get_tx_by_hash))
        .route("/api/txs", get(get_recent_txs))
        .route("/api/accounts/:account/txs", get(get_account_txs))
        .route("/api/accounts/:account/transfers", get(get_account_transfers))
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
        .route("/api/totalsupply", get(get_total_supply))
        .route("/api/circulatingsupply", get(get_circulating_supply))
        .route("/api/richlist", get(get_richlist))
        .route("/api/stsolen/apy", get(crate::stsolen_apy::get_stsolen_apy))
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
    Query(mut params): Query<PaginationParams>,
) -> Json<Vec<IndexedBlock>> {
    params.limit = params.limit.min(MAX_PAGE_LIMIT);
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

/// Lookup by `tx_hash` (= `blake3(block_height_le ‖ tx_index_le ‖ sender ‖ nonce_le)`,
/// hex-encoded). Accepts 64 hex chars with or without a `0x` prefix;
/// case-insensitive.
async fn get_tx_by_hash(
    State(state): State<ApiState>,
    Path(tx_hash): Path<String>,
) -> Json<Option<IndexedTx>> {
    let store = state.store.read().unwrap();
    Json(store.get_tx_by_hash(&tx_hash).cloned())
}

async fn get_recent_txs(
    State(state): State<ApiState>,
    Query(mut params): Query<PaginationParams>,
) -> Json<Vec<IndexedTx>> {
    params.limit = params.limit.min(MAX_PAGE_LIMIT);
    let store = state.store.read().unwrap();
    Json(store.get_recent_txs_paged(params.limit, params.offset).into_iter().cloned().collect())
}

async fn get_account_txs(
    State(state): State<ApiState>,
    Path(account): Path<String>,
    Query(mut params): Query<PaginationParams>,
) -> Json<Vec<IndexedTx>> {
    params.limit = params.limit.min(MAX_PAGE_LIMIT);
    // Accept both hex and base58 addresses — normalize to base58 for lookup.
    let lookup = if let Ok(id) = parse_address(&account) {
        account_to_base58(&id)
    } else {
        account
    };
    let store = state.store.read().unwrap();
    let txs: Vec<IndexedTx> = store
        .get_account_txs_paged(&lookup, params.limit, params.offset)
        .into_iter()
        .cloned()
        .collect();
    Json(txs)
}

/// Single transfer projected from an indexed tx's `transfer` event.
#[derive(Debug, Clone, Serialize)]
pub struct TransferRow {
    /// "{block_height}-{tx_index}-{event_index}".
    pub txid: String,
    /// Hex-encoded `blake3(block_height_le ‖ tx_index_le ‖ sender ‖ nonce_le)`
    /// — the same `tx_hash` exposed by the consensus engine and `/api/tx/...`.
    /// Empty on rows whose underlying tx was indexed before the hash was
    /// recorded.
    pub tx_hash: String,
    pub block_height: u64,
    pub tx_index: usize,
    pub event_index: usize,
    /// Event emitter. For native SOLEN transfers this equals `sender`; for
    /// token contract transfers it's the contract address.
    pub emitter: String,
    pub sender: String,
    pub recipient: String,
    /// Base units, decimal string (preserves u128 precision).
    pub amount: String,
    /// Human-readable SOLEN amount with 8 decimals (e.g. "10.00000000").
    pub amount_solen: String,
    /// Tx fee in base units, attributed once per tx to the primary outgoing
    /// transfer (where `emitter == sender`). Other rows return "0".
    pub fee: String,
    pub fee_solen: String,
    pub success: bool,
    pub timestamp_ms: u64,
}

#[derive(Deserialize)]
pub struct TransferQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
    /// "in" (recipient = account), "out" (sender = account), or "all" (default).
    direction: Option<String>,
}

const SOLEN_BASE_UNITS_PER_TOKEN: u128 = 100_000_000;
const SOLEN_DECIMALS: usize = 8;

fn format_solen(base_units: u128) -> String {
    let whole = base_units / SOLEN_BASE_UNITS_PER_TOKEN;
    let frac = base_units % SOLEN_BASE_UNITS_PER_TOKEN;
    format!("{whole}.{frac:0>width$}", width = SOLEN_DECIMALS)
}

/// Parse 32 hex chars as little-endian u128.
fn decode_u128_le_hex(hex: &str) -> Option<u128> {
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for i in 0..16 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(u128::from_le_bytes(bytes))
}

/// Decode a `transfer` event's data field. Layout: recipient[32] ‖ amount[16, LE u128].
fn decode_transfer_data(hex: &str) -> Option<(String, u128)> {
    if hex.len() < 96 {
        return None;
    }
    let mut recipient = [0u8; 32];
    for i in 0..32 {
        recipient[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    let amount = decode_u128_le_hex(&hex[64..96])?;
    Some((account_to_base58(&recipient), amount))
}

async fn get_account_transfers(
    State(state): State<ApiState>,
    Path(account): Path<String>,
    Query(params): Query<TransferQuery>,
) -> Json<Vec<TransferRow>> {
    let lookup = if let Ok(id) = parse_address(&account) {
        account_to_base58(&id)
    } else {
        account
    };
    let direction = params.direction.as_deref().unwrap_or("all");
    let store = state.store.read().unwrap();

    // Walk this account's tx index newest-first, project transfer events,
    // filter by direction, then paginate at the row level. The tx scan is
    // bounded (MAX_TRANSFER_SCAN) so a hot account cannot force an unbounded
    // load; the page size is capped at MAX_PAGE_LIMIT.
    let txs = store.get_account_txs_paged(&lookup, MAX_TRANSFER_SCAN, 0);
    let mut rows: Vec<TransferRow> = Vec::new();
    let mut skipped = 0usize;
    let limit = params.limit.min(MAX_PAGE_LIMIT).max(1);
    let offset = params.offset;

    for tx in txs {
        // Per-tx fee amount (0 if no fee event).
        let tx_fee: u128 = tx.events.iter()
            .find(|e| e.topic == "fee" && e.data.len() >= 32)
            .and_then(|e| decode_u128_le_hex(&e.data[..32]))
            .unwrap_or(0);

        let block_ts = store.get_block(tx.block_height).map(|b| b.timestamp_ms).unwrap_or(0);
        let mut fee_attributed = false;

        for (ev_idx, ev) in tx.events.iter().enumerate() {
            if ev.topic != "transfer" { continue; }
            let Some((recipient, amount)) = decode_transfer_data(&ev.data) else { continue };

            let is_out = tx.sender == lookup;
            let is_in = recipient == lookup;
            if !is_out && !is_in { continue; }
            match direction {
                "in" if !is_in => continue,
                "out" if !is_out => continue,
                _ => {}
            }

            // Attribute the fee once per tx, to the first transfer emitted by
            // the tx sender (the primary outgoing). Contract fan-out rows get 0.
            let fee = if !fee_attributed && ev.emitter == tx.sender {
                fee_attributed = true;
                tx_fee
            } else {
                0
            };

            if skipped < offset {
                skipped += 1;
                continue;
            }

            rows.push(TransferRow {
                txid: format!("{}-{}-{}", tx.block_height, tx.index, ev_idx),
                tx_hash: tx.tx_hash.clone(),
                block_height: tx.block_height,
                tx_index: tx.index,
                event_index: ev_idx,
                emitter: ev.emitter.clone(),
                sender: tx.sender.clone(),
                recipient,
                amount: amount.to_string(),
                amount_solen: format_solen(amount),
                fee: fee.to_string(),
                fee_solen: format_solen(fee),
                success: tx.success,
                timestamp_ms: block_ts,
            });

            if rows.len() >= limit {
                return Json(rows);
            }
        }
    }
    Json(rows)
}

async fn get_events(
    State(state): State<ApiState>,
    Query(mut params): Query<EventsQuery>,
) -> Json<Vec<IndexedEvent>> {
    params.limit = params.limit.min(MAX_PAGE_LIMIT);
    // Normalize the contract filter: callers may pass hex (with or without
    // `0x`) or Base58. Events are stored with Base58 emitters.
    let emitter_b58 = params.contract.as_deref().and_then(|raw| {
        parse_address(raw).ok().map(|bytes| account_to_base58(&bytes))
    });
    let store = state.store.read().unwrap();
    let events: Vec<IndexedEvent> = store
        .get_events_filtered(
            emitter_b58.as_deref(),
            params.topic.as_deref(),
            params.from_height,
            params.to_height,
            params.limit,
            params.offset,
        )
        .into_iter()
        .cloned()
        .collect();
    Json(events)
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
                id: account_to_base58(&v.id),
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
    // Check in-memory index first.
    let store = state.store.read().unwrap();
    if let Some(src) = store.contract_sources.get(&code_hash) {
        return Json(Some(src.clone()));
    }
    drop(store);

    // Fall back to persistent storage (survives restarts).
    if let Some(engine) = &state.engine {
        let estore = engine.store();
        let estore = estore.read().unwrap();
        let key = format!("source/{}", code_hash);
        if let Ok(Some(data)) = estore.get(key.as_bytes()) {
            if let Ok(src) = serde_json::from_slice::<crate::store::ContractSource>(&data) {
                // Cache in memory for future lookups.
                drop(estore);
                let mut idx = state.store.write().unwrap();
                idx.contract_sources.insert(code_hash, src.clone());
                return Json(Some(src));
            }
        }
    }
    Json(None)
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct PublishSourceRequest {
    code_hash: String,
    source_code: String,
    language: Option<String>,
    compiler_version: Option<String>,
}

/// Largest source blob accepted by the publish endpoint.
const MAX_SOURCE_BYTES: usize = 256 * 1024;

/// Global single-flight guard for on-node contract compilation. Compiling
/// attacker-supplied Rust is expensive; this ensures at most ONE compile runs
/// at a time so the public node can't be flooded into parallel `cargo build`s.
static SOURCE_COMPILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// On-node compilation is OFF unless explicitly enabled by the operator.
/// Even when enabled it is gated (contract must exist on-chain, single-flight,
/// size-capped). When disabled, source is still stored but left unverified —
/// verification should happen out-of-band (reproducible CI build).
fn source_compile_enabled() -> bool {
    std::env::var("SOLEN_ENABLE_SOURCE_COMPILE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// True only if a contract with this code hash is actually deployed on-chain.
/// Prevents spending compile cycles on arbitrary attacker-chosen hashes.
fn contract_bytecode_exists(state: &ApiState, code_hash_hex: &str) -> bool {
    let Some(engine) = &state.engine else { return false };
    let Ok(hash_bytes) = parse_address(code_hash_hex) else { return false };
    let mut key = b"code/".to_vec();
    key.extend_from_slice(&hash_bytes);
    let estore = engine.store();
    let estore = estore.read().unwrap();
    matches!(estore.get(&key), Ok(Some(_)))
}

async fn publish_contract_source(
    State(state): State<ApiState>,
    Path(code_hash): Path<String>,
    Json(body): Json<PublishSourceRequest>,
) -> Json<serde_json::Value> {
    // Validate and rate-limit source publishing.
    {
        // Basic validation: code_hash must be valid hex and 64 chars.
        if code_hash.len() != 64 || !code_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Json(serde_json::json!({"success": false, "error": "invalid code_hash format"}));
        }

        // Bound the stored/compiled source size.
        if body.source_code.len() > MAX_SOURCE_BYTES {
            return Json(serde_json::json!({"success": false, "error": "source too large"}));
        }

        // H-14: only accept source for a contract that ACTUALLY EXISTS on-chain.
        // The endpoint is unauthenticated and the per-code_hash rate limit is
        // trivially bypassed with fresh hashes, so without this an attacker could
        // persist up to 256KB of arbitrary data under any of 2^256 hashes
        // (durable storage-bloat DoS). Binding to deployed bytecode caps the
        // total set of storable sources to the real contract population.
        if !contract_bytecode_exists(&state, &code_hash) {
            return Json(serde_json::json!({"success": false, "error": "no deployed contract with this code_hash"}));
        }

        // Rate limit: reject if source was published recently (within 1 hour).
        let store = state.store.read().unwrap();
        if let Some(existing) = store.contract_sources.get(&code_hash) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now.saturating_sub(existing.published_at) < 3600 {
                return Json(serde_json::json!({"success": false, "error": "source already published recently"}));
            }
        }
    }

    let source_code = body.source_code.clone();
    let language = body.language.unwrap_or_else(|| "rust".to_string());
    let compiler_version = body.compiler_version.unwrap_or_else(|| "unknown".to_string());
    let expected_hash = code_hash.clone();

    // Verify by compiling ONLY when on-node compilation is explicitly enabled,
    // the contract actually exists on-chain, and no other compile is running.
    // Otherwise the source is stored unverified (verify out-of-band).
    let verified = if language == "rust"
        && source_compile_enabled()
        && contract_bytecode_exists(&state, &expected_hash)
    {
        match SOURCE_COMPILE_LOCK.try_lock() {
            Ok(_guard) => verify_rust_contract(&source_code, &expected_hash),
            Err(_) => false, // another compile in progress — don't queue work
        }
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

    store.contract_sources.insert(code_hash.clone(), source.clone());
    drop(store);

    // Persist to engine store so it survives restarts.
    if let Some(engine) = &state.engine {
        let estore = engine.store();
        let mut estore = estore.write().unwrap();
        let key = format!("source/{}", code_hash);
        if let Ok(data) = serde_json::to_vec(&source) {
            let _ = estore.put(key.as_bytes(), &data);
        }
    }

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
    let lookup = if let Ok(id) = parse_address(&account) {
        account_to_base58(&id)
    } else {
        account
    };
    let store = state.store.read().unwrap();
    Json(store.get_account_tokens(&lookup))
}

async fn get_contracts(
    State(state): State<ApiState>,
) -> Json<Vec<String>> {
    let store = state.store.read().unwrap();
    Json(store.get_contracts())
}

async fn get_fulfilled_intents(
    State(state): State<ApiState>,
    Query(mut params): Query<PaginationParams>,
) -> Json<Vec<IndexedIntent>> {
    params.limit = params.limit.min(MAX_PAGE_LIMIT);
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
    Query(mut params): Query<PaginationParams>,
) -> Json<Vec<IndexedBatch>> {
    params.limit = params.limit.min(MAX_PAGE_LIMIT);
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

fn format_supply(raw: u128) -> String {
    let whole = raw / 100_000_000;
    let frac = raw % 100_000_000;
    if frac == 0 { format!("{whole}") } else { format!("{whole}.{frac:08}") }
}

async fn get_total_supply(State(state): State<ApiState>) -> impl IntoResponse {
    let engine = match &state.engine {
        Some(e) => e,
        None => return "0".to_string(),
    };
    let store = engine.store();
    let store = store.read().unwrap();
    let total = match store.get(b"__total_supply__") {
        Ok(Some(data)) if data.len() >= 16 => {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&data[..16]);
            u128::from_le_bytes(buf)
        }
        _ => 0,
    };
    format_supply(total)
}

async fn get_circulating_supply(State(state): State<ApiState>) -> impl IntoResponse {
    let engine = match &state.engine {
        Some(e) => e,
        None => return "0".to_string(),
    };
    let store = engine.store();
    let store = store.read().unwrap();

    let total = match store.get(b"__total_supply__") {
        Ok(Some(data)) if data.len() >= 16 => {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&data[..16]);
            u128::from_le_bytes(buf)
        }
        _ => 0,
    };

    use solen_types::system::*;
    let non_circ_addrs = [
        TREASURY_ADDRESS, STAKING_POOL_ADDRESS, ECOSYSTEM_FUND_ADDRESS,
        COMMUNITY_ADDRESS, LIQUIDITY_ADDRESS, TEAM_POOL_ADDRESS,
        INVESTOR_POOL_ADDRESS, BRIDGE_ADDRESS, VESTING_ADDRESS,
    ];
    let state_mgr = solen_execution::state::ReadonlyStateManager::new(store.as_ref());
    let non_circ: u128 = non_circ_addrs.iter()
        .map(|addr| state_mgr.get_balance(addr).unwrap_or(0))
        .sum();

    let staking = solen_system_contracts::staking::StakingContract::load(store.as_ref());
    let total_staked: u128 = staking.validators.iter().map(|v| v.total_stake()).sum();

    let circulating = total.saturating_sub(non_circ).saturating_sub(total_staked);
    format_supply(circulating)
}

#[derive(Serialize)]
struct RichListEntry {
    rank: usize,
    address: String,
    balance: String,
    staked: String,
    total: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

fn system_account_label(id: &[u8; 32]) -> Option<&'static str> {
    use solen_types::system::*;
    match *id {
        _ if *id == TREASURY_ADDRESS => Some("Treasury"),
        _ if *id == STAKING_POOL_ADDRESS => Some("Staking Rewards Pool"),
        _ if *id == ECOSYSTEM_FUND_ADDRESS => Some("Ecosystem Fund"),
        _ if *id == COMMUNITY_ADDRESS => Some("Community & Airdrops"),
        _ if *id == LIQUIDITY_ADDRESS => Some("Liquidity & Market Making"),
        _ if *id == TEAM_POOL_ADDRESS => Some("Team Vesting Pool"),
        _ if *id == INVESTOR_POOL_ADDRESS => Some("Investor Pool"),
        _ if *id == BRIDGE_ADDRESS => Some("Bridge Vault"),
        _ if *id == VESTING_ADDRESS => Some("Vesting Contract"),
        _ => None,
    }
}

/// Project a page of richlist rows from the pre-sorted snapshot.
fn richlist_page(
    accounts: &[([u8; 32], u128, u128)],
    offset: usize,
    limit: usize,
) -> Vec<RichListEntry> {
    accounts.iter()
        .skip(offset)
        .take(limit)
        .enumerate()
        .map(|(i, (id, balance, staked))| {
            let label = system_account_label(id).map(|s| s.to_string()).or_else(|| {
                if *staked > 0 { Some("Validator".to_string()) } else { None }
            });
            RichListEntry {
                rank: offset + i + 1,
                address: account_to_base58(id),
                balance: balance.to_string(),
                staked: staked.to_string(),
                total: (balance + staked).to_string(),
                label,
            }
        })
        .collect()
}

async fn get_richlist(
    State(state): State<ApiState>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<RichListEntry>> {
    const RICHLIST_TTL: std::time::Duration = std::time::Duration::from_secs(30);
    let limit = params.limit.min(100);

    // Fast path: serve from the cached snapshot if it's still fresh, without
    // touching the engine store. The whole-table scan + sort happens at most
    // once per TTL instead of once per request.
    {
        let guard = state.richlist_cache.read().unwrap();
        if let Some((built, accounts)) = guard.as_ref() {
            if built.elapsed() < RICHLIST_TTL {
                return Json(richlist_page(accounts, params.offset, limit));
            }
        }
    }

    // Cache miss / stale: rebuild the sorted snapshot.
    let engine = match &state.engine {
        Some(e) => e,
        None => return Json(vec![]),
    };
    let mut accounts: Vec<([u8; 32], u128, u128)> = {
        let store = engine.store();
        let store = store.read().unwrap();
        let entries = match store.scan_prefix(b"acc/") {
            Ok(e) => e,
            Err(_) => return Json(vec![]),
        };
        let staking = solen_system_contracts::staking::StakingContract::load(store.as_ref());
        let mut accounts = Vec::new();
        for (key, value) in &entries {
            if key.len() != 36 { continue; } // "acc/" + 32 bytes
            if let Ok(account) = borsh::from_slice::<solen_types::account::Account>(value) {
                let mut id = [0u8; 32];
                id.copy_from_slice(&key[4..]);
                let staked = staking.validators.iter()
                    .find(|v| v.id == id)
                    .map(|v| v.self_stake)
                    .unwrap_or(0);
                let total = account.balance + staked;
                if total > 0 {
                    accounts.push((id, account.balance, staked));
                }
            }
        }
        accounts
    };

    // Sort by total (balance + staked) descending.
    accounts.sort_by(|a, b| (b.1 + b.2).cmp(&(a.1 + a.2)));

    let result = richlist_page(&accounts, params.offset, limit);
    *state.richlist_cache.write().unwrap() = Some((std::time::Instant::now(), accounts));
    Json(result)
}

pub async fn start_explorer_api(
    addr: std::net::SocketAddr,
    store: Arc<RwLock<IndexStore>>,
    engine: Option<Arc<ConsensusEngine>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stsolen_apy = Arc::new(crate::stsolen_apy::ApySamples::new());
    if let Some(e) = engine.as_ref() {
        crate::stsolen_apy::spawn_sampler(stsolen_apy.clone(), e.clone());
    }
    let state = ApiState {
        store,
        engine,
        stsolen_apy,
        richlist_cache: Arc::new(RwLock::new(None)),
    };
    let app = router(state);

    info!(%addr, "explorer API started");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
