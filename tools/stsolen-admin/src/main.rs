//! Generic stSOLEN admin/utility caller. Signs an arbitrary
//! `Call(target, method, args)` with the given seed and submits.
//!
//! Built for stSOLEN admin ops (pause, set_treasury, set_slash_oracle,
//! settle_shortfall, etc.) but works for any contract on any Solen network.
//!
//! Example — pause v1:
//!   stsolen-admin --signer-seed ./owner.seed \
//!     --target bee37513c713e55113115dda2ae41d1ddd67802d99610708ec289130c1c8edc5 \
//!     --method pause --live --confirm

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use solen_crypto::Keypair;
use solen_types::transaction::{Action, UserOperation};
use solen_types::AccountId;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tracing::info;

#[derive(Parser)]
#[command(about = "Send a single Call(target, method, args) UserOp signed by a seed file")]
struct Cli {
    #[arg(long, env = "SOLEN_RPC", default_value = "https://rpc.solenchain.io")]
    rpc: String,

    #[arg(long, env = "SOLEN_CHAIN_ID", default_value_t = 1)]
    chain_id: u64,

    /// 32-byte hex Ed25519 seed file. The corresponding pubkey signs the op.
    #[arg(long)]
    signer_seed: PathBuf,

    /// 32-byte hex contract address.
    #[arg(long)]
    target: String,

    /// Method name (e.g. "pause", "settle_shortfall").
    #[arg(long)]
    method: String,

    /// Hex-encoded args. Empty string for no-arg methods.
    #[arg(long, default_value = "")]
    args: String,

    #[arg(long, default_value_t = 1_000_000)]
    max_fee: u128,

    #[arg(long, default_value_t = false)]
    confirm: bool,

    #[arg(long, default_value_t = false)]
    live: bool,
}

#[derive(Serialize)]
struct JsonRpcRequest<'a, P: Serialize> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: P,
}

#[derive(Deserialize, Debug)]
struct JsonRpcResponse<R> {
    result: Option<R>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Deserialize, Debug)]
struct SubmitResult {
    accepted: bool,
    error: Option<String>,
}

#[derive(Deserialize, Debug)]
struct SubmitConfirmResult {
    accepted: bool,
    confirmed: bool,
    success: bool,
    block_height: u64,
    tx_hash: String,
    error: Option<String>,
}

struct RpcClient {
    http: reqwest::blocking::Client,
    url: String,
}

impl RpcClient {
    fn new(url: String) -> Self {
        Self {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("build reqwest"),
            url,
        }
    }
    fn call<P: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R> {
        let req = JsonRpcRequest { jsonrpc: "2.0", id: 1, method, params };
        let resp: JsonRpcResponse<R> = self
            .http
            .post(&self.url)
            .json(&req)
            .send()
            .with_context(|| format!("POST {} {method}", self.url))?
            .json()
            .context("decode rpc response")?;
        if let Some(e) = resp.error {
            bail!("rpc {method}: {} (code {})", e.message, e.code);
        }
        resp.result.ok_or_else(|| anyhow!("rpc {method}: empty result"))
    }
    fn next_nonce(&self, account: &AccountId) -> Result<u64> {
        self.call("solen_getNextNonce", (hex::encode(account),))
    }
    fn submit(&self, op: &UserOperation) -> Result<SubmitResult> {
        self.call("solen_submitOperation", (op,))
    }
    fn submit_confirm(&self, op: &UserOperation, t: u64) -> Result<SubmitConfirmResult> {
        self.call("solen_submitOperationConfirm", (op, t))
    }
}

fn decode_account(label: &str, s: &str) -> Result<AccountId> {
    let raw = hex::decode(s.trim().trim_start_matches("0x"))
        .with_context(|| format!("decode --{label} hex"))?;
    if raw.len() != 32 {
        bail!("--{label} must decode to 32 bytes (got {})", raw.len());
    }
    let mut a = [0u8; 32];
    a.copy_from_slice(&raw);
    Ok(a)
}

fn decode_seed(path: &std::path::Path) -> Result<[u8; 32]> {
    let raw = fs::read_to_string(path)?;
    let bytes = hex::decode(raw.trim().trim_start_matches("0x"))?;
    if bytes.len() != 32 {
        bail!("seed file must be 32 bytes hex (got {})", bytes.len());
    }
    let mut s = [0u8; 32];
    s.copy_from_slice(&bytes);
    Ok(s)
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();

    let target = decode_account("target", &cli.target)?;
    let seed = decode_seed(&cli.signer_seed)?;
    let kp = Keypair::from_seed(&seed);
    let signer: AccountId = kp.public_key();
    let args_bytes: Vec<u8> = if cli.args.is_empty() {
        vec![]
    } else {
        hex::decode(cli.args.trim().trim_start_matches("0x"))
            .context("decode --args hex")?
    };

    info!(
        signer = %hex::encode(signer),
        target = %hex::encode(target),
        method = %cli.method,
        args_bytes = args_bytes.len(),
        max_fee = cli.max_fee,
        live = cli.live,
        confirm = cli.confirm,
        "stsolen-admin call plan"
    );

    if !cli.live {
        info!("dry-run; pass --live to submit");
        return Ok(());
    }

    let rpc = RpcClient::new(cli.rpc);
    let nonce = rpc.next_nonce(&signer)?;
    info!(nonce, "fetched signer nonce");

    let mut op = UserOperation {
        sender: signer,
        nonce,
        actions: vec![Action::Call {
            target,
            method: cli.method.clone(),
            args: args_bytes,
        }],
        max_fee: cli.max_fee,
        signature: vec![],
    };
    let msg = op.signing_message(cli.chain_id);
    op.signature = kp.sign(&msg).to_vec();

    if cli.confirm {
        let r = rpc.submit_confirm(&op, 60)?;
        if !r.accepted {
            bail!("submit rejected: {}", r.error.unwrap_or_else(|| "(no error)".into()));
        }
        if !r.confirmed {
            bail!("not confirmed: {}", r.error.unwrap_or_else(|| "(no error)".into()));
        }
        if !r.success {
            bail!(
                "reverted on-chain (tx {}): {}",
                r.tx_hash,
                r.error.unwrap_or_else(|| "(no error)".into())
            );
        }
        info!(
            block_height = r.block_height,
            tx_hash = %r.tx_hash,
            "call confirmed"
        );
    } else {
        let r = rpc.submit(&op)?;
        if !r.accepted {
            bail!("submit rejected: {}", r.error.unwrap_or_else(|| "(no error)".into()));
        }
        info!("submitted; check the explorer or rerun with --confirm");
    }

    Ok(())
}
