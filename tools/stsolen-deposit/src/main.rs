//! One-shot stSOLEN depositor.
//!
//! Submits a single atomic UserOperation that:
//!   1. `Transfer(stsolen, amount)` — moves SOLEN from depositor to the contract.
//!   2. `Call(stsolen, "deposit", &[])` — the contract reads `msg_value`,
//!      mints stSOLEN at the current rate, and queues a `STAKING_ADDRESS:delegate`
//!      for the round-robin-selected operator.
//!
//! Reusable for any deposit, not just the first one. Validates that the
//! account has enough balance for `amount + max_fee` before submitting.
//!
//! Example (bootstrap):
//!   stsolen-deposit \
//!     --depositor-seed ./stsolen-bootstrap.seed \
//!     --stsolen bee37513c713e55113115dda2ae41d1ddd67802d99610708ec289130c1c8edc5 \
//!     --amount 10000000000 \
//!     --live --confirm

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
#[command(about = "Deposit SOLEN into the stSOLEN contract (Transfer + Call(deposit) atomic op)")]
struct Cli {
    #[arg(long, env = "SOLEN_RPC", default_value = "https://rpc.solenchain.io")]
    rpc: String,

    /// Chain ID. 1 = mainnet (default), 9000 = testnet.
    #[arg(long, env = "SOLEN_CHAIN_ID", default_value_t = 1)]
    chain_id: u64,

    /// Path to a file containing the 32-byte hex depositor seed.
    #[arg(long)]
    depositor_seed: PathBuf,

    /// 32-byte hex stSOLEN contract address.
    #[arg(long)]
    stsolen: String,

    /// Amount to deposit, in base units (8 decimals, so 10^8 = 1 SOLEN).
    /// First-ever deposit must be ≥ 11_100 (the bootstrap-burn threshold).
    #[arg(long)]
    amount: u128,

    /// Max-fee for the UserOperation, in base units.
    #[arg(long, default_value_t = 1_000_000)]
    max_fee: u128,

    /// Wait for confirmation via `solen_submitOperationConfirm` (default: just
    /// submit and exit).
    #[arg(long, default_value_t = false)]
    confirm: bool,

    /// Actually submit. Default prints the plan without sending anything.
    #[arg(long, default_value_t = false)]
    live: bool,
}

// ── JSON-RPC plumbing ─────────────────────────────────────────────────

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
        resp.result
            .ok_or_else(|| anyhow!("rpc {method}: empty result"))
    }

    fn next_nonce(&self, account: &AccountId) -> Result<u64> {
        self.call("solen_getNextNonce", (hex::encode(account),))
    }

    fn get_balance(&self, account: &AccountId) -> Result<u128> {
        let s: String = self.call("solen_getBalance", (hex::encode(account),))?;
        s.parse::<u128>().context("parse balance")
    }

    fn submit_operation(&self, op: &UserOperation) -> Result<SubmitResult> {
        self.call("solen_submitOperation", (op,))
    }

    fn submit_operation_confirm(
        &self,
        op: &UserOperation,
        timeout_secs: u64,
    ) -> Result<SubmitConfirmResult> {
        self.call("solen_submitOperationConfirm", (op, timeout_secs))
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

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

fn decode_32_seed(path: &std::path::Path) -> Result<[u8; 32]> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let bytes = hex::decode(raw.trim().trim_start_matches("0x"))
        .context("decode seed hex")?;
    if bytes.len() != 32 {
        bail!("seed file must contain 32 bytes (got {})", bytes.len());
    }
    let mut s = [0u8; 32];
    s.copy_from_slice(&bytes);
    Ok(s)
}

// ── Main ────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();

    let stsolen = decode_account("stsolen", &cli.stsolen)?;
    let seed = decode_32_seed(&cli.depositor_seed)?;
    let kp = Keypair::from_seed(&seed);
    let depositor: AccountId = kp.public_key();

    if cli.amount == 0 {
        bail!("--amount must be > 0");
    }

    info!(
        depositor = %hex::encode(depositor),
        stsolen = %hex::encode(stsolen),
        amount_base_units = cli.amount,
        amount_solen = format!("{}.{:08}", cli.amount / 100_000_000, cli.amount % 100_000_000),
        max_fee = cli.max_fee,
        chain_id = cli.chain_id,
        live = cli.live,
        confirm = cli.confirm,
        "stsolen deposit plan"
    );

    if !cli.live {
        info!("dry-run: re-run with --live to submit");
        return Ok(());
    }

    let rpc = RpcClient::new(cli.rpc);

    // Pre-flight: verify the depositor has enough balance for amount + max_fee.
    let bal = rpc.get_balance(&depositor)?;
    let need = cli.amount.saturating_add(cli.max_fee);
    info!(balance = bal, need, "depositor pre-flight");
    if bal < need {
        bail!(
            "depositor balance {} < amount + max_fee ({}); fund the account before retrying",
            bal,
            need
        );
    }

    let nonce = rpc.next_nonce(&depositor)?;
    info!(nonce, "fetched depositor nonce");

    let mut op = UserOperation {
        sender: depositor,
        nonce,
        actions: vec![
            Action::Transfer { to: stsolen, amount: cli.amount },
            Action::Call {
                target: stsolen,
                method: "deposit".into(),
                args: vec![],
            },
        ],
        max_fee: cli.max_fee,
        signature: vec![],
    };
    let msg = op.signing_message(cli.chain_id);
    op.signature = kp.sign(&msg).to_vec();

    if cli.confirm {
        let result = rpc.submit_operation_confirm(&op, 60)?;
        if !result.accepted {
            bail!(
                "submit rejected: {}",
                result.error.unwrap_or_else(|| "(no error)".into())
            );
        }
        if !result.confirmed {
            bail!(
                "not confirmed within timeout: {}",
                result.error.unwrap_or_else(|| "(no error)".into())
            );
        }
        if !result.success {
            bail!(
                "deposit reverted on-chain (tx {}): {}",
                result.tx_hash,
                result.error.unwrap_or_else(|| "(no error)".into())
            );
        }
        info!(
            block_height = result.block_height,
            tx_hash = %result.tx_hash,
            "deposit confirmed"
        );
    } else {
        let result = rpc.submit_operation(&op)?;
        if !result.accepted {
            bail!(
                "submit rejected: {}",
                result.error.unwrap_or_else(|| "(no error)".into())
            );
        }
        info!("deposit submitted; check the explorer or re-run with --confirm");
    }

    Ok(())
}
