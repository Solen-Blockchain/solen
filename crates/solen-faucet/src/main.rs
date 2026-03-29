//! Solen Faucet — HTTP service that drips testnet tokens.
//!
//! Endpoints:
//!   POST /drip          { "account": "<hex or name>" }  → drip tokens
//!   GET  /status        → faucet balance, drip amount, cooldown
//!   GET  /health        → 200 OK
//!
//! Intended to run at faucet.solenchain.com

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use serde::{Deserialize, Serialize};
use solen_crypto::{blake3_hash, Keypair};
use solen_types::transaction::{Action, UserOperation};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "solen-faucet", version = "0.1.0")]
struct Cli {
    /// Faucet listen port.
    #[arg(long, default_value = "8080")]
    port: u16,

    /// RPC endpoint of the Solen node.
    #[arg(long, default_value = "http://127.0.0.1:29944")]
    rpc: String,

    /// Faucet account seed (32-byte hex).
    #[arg(long, default_value = "2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a")]
    seed: String,

    /// Faucet account name (for ID derivation).
    #[arg(long, default_value = "faucet")]
    account_name: String,

    /// Amount to drip per request (in base units, 1 SOLEN = 100000000).
    #[arg(long, default_value = "100000000")]
    drip_amount: u128,

    /// Cooldown per recipient in seconds.
    #[arg(long, default_value = "300")]
    cooldown: u64,

    /// Allowed origin for CORS (use * for any).
    #[arg(long, default_value = "*")]
    cors_origin: String,
}

#[derive(Clone)]
struct AppState {
    rpc_url: String,
    keypair: Arc<Keypair>,
    faucet_id: [u8; 32],
    drip_amount: u128,
    cooldown: Duration,
    /// Tracks last drip time per recipient hex.
    rate_limit: Arc<Mutex<HashMap<String, Instant>>>,
    http_client: reqwest::Client,
}

// ── Request/Response types ──────────────────────────────────────

#[derive(Deserialize)]
struct DripRequest {
    account: String,
}

#[derive(Serialize)]
struct DripResponse {
    success: bool,
    amount: String,
    recipient: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_secs: Option<u64>,
}

#[derive(Serialize)]
struct StatusResponse {
    faucet_account: String,
    drip_amount: String,
    cooldown_secs: u64,
    rpc_endpoint: String,
}

// ── Handlers ────────────────────────────────────────────────────

async fn handle_drip(
    State(state): State<AppState>,
    Json(req): Json<DripRequest>,
) -> (StatusCode, Json<DripResponse>) {
    let recipient_hex = resolve_account(&req.account);

    // Rate limit check.
    {
        let limits = state.rate_limit.lock().unwrap();
        if let Some(last) = limits.get(&recipient_hex) {
            let elapsed = last.elapsed();
            if elapsed < state.cooldown {
                let remaining = (state.cooldown - elapsed).as_secs();
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(DripResponse {
                        success: false,
                        amount: "0".into(),
                        recipient: recipient_hex,
                        error: Some(format!("rate limited, retry in {remaining}s")),
                        retry_after_secs: Some(remaining),
                    }),
                );
            }
        }
    }

    // Get faucet nonce.
    let faucet_hex = hex_encode(&state.faucet_id);
    let nonce = match get_nonce(&state.http_client, &state.rpc_url, &faucet_hex).await {
        Ok(n) => n,
        Err(e) => {
            warn!(error = %e, "failed to get faucet nonce");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(DripResponse {
                    success: false,
                    amount: "0".into(),
                    recipient: recipient_hex,
                    error: Some("node unavailable".into()),
                    retry_after_secs: None,
                }),
            );
        }
    };

    // Build and sign the transfer.
    let to = match hex_decode_32(&recipient_hex) {
        Ok(id) => id,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(DripResponse {
                    success: false,
                    amount: "0".into(),
                    recipient: recipient_hex,
                    error: Some("invalid account ID".into()),
                    retry_after_secs: None,
                }),
            );
        }
    };

    let mut op = UserOperation {
        sender: state.faucet_id,
        nonce,
        actions: vec![Action::Transfer {
            to,
            amount: state.drip_amount,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &state.keypair);

    // Submit.
    match submit_op(&state.http_client, &state.rpc_url, &op).await {
        Ok(true) => {
            // Record rate limit.
            state
                .rate_limit
                .lock()
                .unwrap()
                .insert(recipient_hex.clone(), Instant::now());

            info!(
                recipient = %recipient_hex[..16],
                amount = state.drip_amount,
                "drip successful"
            );

            (
                StatusCode::OK,
                Json(DripResponse {
                    success: true,
                    amount: state.drip_amount.to_string(),
                    recipient: recipient_hex,
                    error: None,
                    retry_after_secs: None,
                }),
            )
        }
        Ok(false) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(DripResponse {
                success: false,
                amount: "0".into(),
                recipient: recipient_hex,
                error: Some("transaction rejected by node".into()),
                retry_after_secs: None,
            }),
        ),
        Err(e) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(DripResponse {
                success: false,
                amount: "0".into(),
                recipient: recipient_hex,
                error: Some(format!("submission failed: {e}")),
                retry_after_secs: None,
            }),
        ),
    }
}

async fn handle_status(State(state): State<AppState>) -> Json<StatusResponse> {
    Json(StatusResponse {
        faucet_account: hex_encode(&state.faucet_id),
        drip_amount: state.drip_amount.to_string(),
        cooldown_secs: state.cooldown.as_secs(),
        rpc_endpoint: state.rpc_url.clone(),
    })
}

async fn handle_health() -> StatusCode {
    StatusCode::OK
}

// ── RPC helpers ─────────────────────────────────────────────────

async fn get_nonce(
    client: &reqwest::Client,
    rpc_url: &str,
    account_hex: &str,
) -> anyhow::Result<u64> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "solen_getAccount",
        "params": [account_hex],
        "id": 1
    });

    let resp: serde_json::Value = client.post(rpc_url).json(&body).send().await?.json().await?;

    resp["result"]["nonce"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("failed to parse nonce"))
}

async fn submit_op(
    client: &reqwest::Client,
    rpc_url: &str,
    op: &UserOperation,
) -> anyhow::Result<bool> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "solen_submitOperation",
        "params": [op],
        "id": 1
    });

    let resp: serde_json::Value = client.post(rpc_url).json(&body).send().await?.json().await?;

    Ok(resp["result"]["accepted"].as_bool().unwrap_or(false))
}

// ── Helpers ─────────────────────────────────────────────────────

fn sign_op(op: &mut UserOperation, kp: &Keypair) {
    let mut msg = Vec::new();
    msg.extend_from_slice(&op.sender);
    msg.extend_from_slice(&op.nonce.to_le_bytes());
    msg.extend_from_slice(&op.max_fee.to_le_bytes());
    let actions_bytes = serde_json::to_vec(&op.actions).unwrap_or_default();
    msg.extend_from_slice(&blake3_hash(&actions_bytes));
    op.signature = kp.sign(&msg).to_vec();
}

fn resolve_account(input: &str) -> String {
    let clean = input.strip_prefix("0x").unwrap_or(input);
    if clean.len() == 64 && clean.chars().all(|c| c.is_ascii_hexdigit()) {
        return clean.to_string();
    }
    // Treat as a name.
    let mut id = [0u8; 32];
    let bytes = input.as_bytes();
    let len = bytes.len().min(32);
    id[..len].copy_from_slice(&bytes[..len]);
    hex_encode(&id)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode_32(s: &str) -> anyhow::Result<[u8; 32]> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    let bytes: Vec<u8> = (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect::<anyhow::Result<Vec<u8>>>()?;
    let mut arr = [0u8; 32];
    if bytes.len() != 32 {
        anyhow::bail!("expected 32 bytes");
    }
    arr.copy_from_slice(&bytes);
    Ok(arr)
}

// ── Main ────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let seed = hex_decode_32(&cli.seed)?;
    let keypair = Arc::new(Keypair::from_seed(&seed));
    // Faucet account ID = its public key.
    let faucet_id = keypair.public_key();

    info!(
        port = cli.port,
        rpc = %cli.rpc,
        faucet = hex_encode(&faucet_id),
        drip = cli.drip_amount,
        cooldown_secs = cli.cooldown,
        "starting Solen faucet"
    );

    let state = AppState {
        rpc_url: cli.rpc.clone(),
        keypair,
        faucet_id,
        drip_amount: cli.drip_amount,
        cooldown: Duration::from_secs(cli.cooldown),
        rate_limit: Arc::new(Mutex::new(HashMap::new())),
        http_client: reqwest::Client::new(),
    };

    let app = Router::new()
        .route("/drip", post(handle_drip))
        .route("/status", get(handle_status))
        .route("/health", get(handle_health))
        .with_state(state);

    let addr: SocketAddr = format!("0.0.0.0:{}", cli.port).parse()?;
    info!(%addr, "faucet listening");
    info!("Drip endpoint: POST /drip {{\"account\": \"<name or hex>\"}}");
    info!("Status:        GET  /status");
    info!("Health:        GET  /health");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
