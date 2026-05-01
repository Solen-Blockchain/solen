//! One-shot stSOLEN deployer.
//!
//! Submits a single atomic UserOperation that:
//!   1. Deploys the stSOLEN wasm with the given salt.
//!   2. Calls `init(treasury[32] || slash_oracle[32])` on the new contract.
//!   3. Calls `set_operator(index[8] || validator[32])` for each operator.
//!   4. Calls `set_op_count(count[8])` to activate the allowlist.
//!
//! All in one UserOp — atomic. If any step fails, the whole deploy unwinds.
//! Caller cap is 16 actions per op (executor.rs `MAX_ACTIONS_PER_OP`); with
//! 11 operators we use 14, well under the limit.
//!
//! The signer of this UserOperation becomes stSOLEN's `owner` (per `do_init`
//! in the contract). Treasury and slash-oracle are configured at the same
//! time. After deploy, the only roles the owner needs to populate later are
//! optional fee/cap tweaks.
//!
//! Example:
//!   stsolen-deploy \
//!     --rpc https://rpc.solenchain.io \
//!     --chain-id 1 \
//!     --owner-seed ./owner.seed \
//!     --stsolen-wasm ../../examples/contracts/stsolen/target/wasm32-unknown-unknown/release/solen_stsolen.wasm \
//!     --treasury 24eb73c83142435bed0cfc4b6fa9a6b6824d70f1f6e9553c5510df87712cab22 \
//!     --slash-oracle 6a36fcadf8c203033bc17da3e80c0439389b0fbe781ae1dfbdd14e3cde68e7be \
//!     --operators 14b069...,2291df...,...,53f999... \
//!     --live

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use solen_crypto::{blake3_hash, Keypair};
use solen_types::transaction::{Action, UserOperation};
use solen_types::AccountId;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tracing::info;

const MAX_OPERATORS_PER_OP: usize = 11; // Leaves 5 slots for deploy/init/set_op_count/headroom.

#[derive(Parser)]
#[command(about = "Deploy + init + populate operators on the stSOLEN contract in one atomic UserOp")]
struct Cli {
    #[arg(long, env = "SOLEN_RPC", default_value = "https://rpc.solenchain.io")]
    rpc: String,

    /// Chain ID. 1 = mainnet (default), 9000 = testnet.
    #[arg(long, env = "SOLEN_CHAIN_ID", default_value_t = 1)]
    chain_id: u64,

    /// Path to a file containing the 32-byte hex owner seed. This key signs
    /// the UserOp and becomes the contract `owner`.
    #[arg(long)]
    owner_seed: PathBuf,

    /// Path to the stSOLEN release wasm.
    #[arg(long)]
    stsolen_wasm: PathBuf,

    /// 32-byte hex treasury account — receives stSOLEN minted as protocol fee.
    #[arg(long)]
    treasury: String,

    /// 32-byte hex slash-oracle account — the only key authorized to call
    /// `report_slash`.
    #[arg(long)]
    slash_oracle: String,

    /// Comma-separated list of 32-byte hex validator IDs. Indexed in the
    /// contract's allowlist in the order given. The contract's per-op cap is
    /// 16 actions; we use 14 for an 11-operator deploy, so list ≤ 11 here.
    #[arg(long)]
    operators: String,

    /// Optional 32-byte hex salt. Defaults to all-zeros, which means repeated
    /// runs with the same wasm + owner will collide.
    #[arg(long)]
    salt: Option<String>,

    /// Max-fee for the deploy UserOperation. Generous default; the actual
    /// gas cost is bounded by the WASM size + the small per-Call overhead.
    #[arg(long, default_value_t = 50_000_000)]
    max_fee: u128,

    /// Wait for confirmation via `solen_submitOperationConfirm` (default: just
    /// submit and exit).
    #[arg(long, default_value_t = false)]
    confirm: bool,

    /// Actually submit. Default prints the plan and the predicted contract
    /// address without sending anything.
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

// ── Address derivation ─────────────────────────────────────────────

/// Predict the deployed contract address. Same formula as the executor.
fn derive_contract_addr(sender: &AccountId, salt: &[u8; 32], code: &[u8]) -> AccountId {
    let code_hash = blake3_hash(code);
    let mut preimage = Vec::with_capacity(96);
    preimage.extend_from_slice(sender);
    preimage.extend_from_slice(salt);
    preimage.extend_from_slice(&code_hash);
    blake3_hash(&preimage)
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

fn parse_operators(s: &str) -> Result<Vec<AccountId>> {
    let parts: Vec<&str> = s.split(',').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        bail!("--operators must list at least one validator");
    }
    if parts.len() > MAX_OPERATORS_PER_OP {
        bail!(
            "--operators lists {} entries; max {} per single deploy op (16-action UserOp budget minus deploy/init/set_op_count)",
            parts.len(),
            MAX_OPERATORS_PER_OP
        );
    }
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| decode_account(&format!("operators[{i}]"), p))
        .collect()
}

fn build_init_args(treasury: &AccountId, slash_oracle: &AccountId) -> Vec<u8> {
    let mut args = Vec::with_capacity(64);
    args.extend_from_slice(treasury);
    args.extend_from_slice(slash_oracle);
    args
}

fn build_set_operator_args(index: u64, validator: &AccountId) -> Vec<u8> {
    let mut args = Vec::with_capacity(40);
    args.extend_from_slice(&index.to_le_bytes());
    args.extend_from_slice(validator);
    args
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

    let treasury = decode_account("treasury", &cli.treasury)?;
    let slash_oracle = decode_account("slash-oracle", &cli.slash_oracle)?;
    let operators = parse_operators(&cli.operators)?;
    let owner_seed = decode_32_seed(&cli.owner_seed)?;
    let owner_kp = Keypair::from_seed(&owner_seed);
    let owner_pk: AccountId = owner_kp.public_key();

    let wasm = fs::read(&cli.stsolen_wasm)
        .with_context(|| format!("read {}", cli.stsolen_wasm.display()))?;

    let salt = if let Some(s) = &cli.salt {
        let raw = hex::decode(s.trim().trim_start_matches("0x"))
            .context("decode --salt hex")?;
        if raw.len() != 32 {
            bail!("--salt must decode to 32 bytes");
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&raw);
        out
    } else {
        [0u8; 32]
    };

    let predicted_addr = derive_contract_addr(&owner_pk, &salt, &wasm);

    info!(
        owner = %hex::encode(owner_pk),
        treasury = %hex::encode(treasury),
        slash_oracle = %hex::encode(slash_oracle),
        operator_count = operators.len(),
        wasm_bytes = wasm.len(),
        predicted_stsolen_addr = %hex::encode(predicted_addr),
        live = cli.live,
        confirm = cli.confirm,
        max_fee = cli.max_fee,
        chain_id = cli.chain_id,
        "stsolen deploy plan"
    );
    for (i, op) in operators.iter().enumerate() {
        info!(slot = i, operator = %hex::encode(op), "operator");
    }

    if !cli.live {
        info!("dry-run: re-run with --live to submit");
        return Ok(());
    }

    // Build the actions: deploy, init, set_operator × N, set_op_count.
    let mut actions: Vec<Action> = Vec::with_capacity(3 + operators.len());
    actions.push(Action::Deploy { code: wasm, salt });
    actions.push(Action::Call {
        target: predicted_addr,
        method: "init".into(),
        args: build_init_args(&treasury, &slash_oracle),
    });
    for (i, op) in operators.iter().enumerate() {
        actions.push(Action::Call {
            target: predicted_addr,
            method: "set_operator".into(),
            args: build_set_operator_args(i as u64, op),
        });
    }
    actions.push(Action::Call {
        target: predicted_addr,
        method: "set_op_count".into(),
        args: (operators.len() as u64).to_le_bytes().to_vec(),
    });

    let rpc = RpcClient::new(cli.rpc);
    let nonce = rpc.next_nonce(&owner_pk)?;
    info!(nonce, action_count = actions.len(), "fetched owner nonce");

    let mut op = UserOperation {
        sender: owner_pk,
        nonce,
        actions,
        max_fee: cli.max_fee,
        signature: vec![],
    };
    let msg = op.signing_message(cli.chain_id);
    op.signature = owner_kp.sign(&msg).to_vec();

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
                "deploy reverted on-chain (tx {}): {}",
                result.tx_hash,
                result.error.unwrap_or_else(|| "(no error)".into())
            );
        }
        info!(
            block_height = result.block_height,
            tx_hash = %result.tx_hash,
            stsolen = %hex::encode(predicted_addr),
            "deploy + init + operators confirmed"
        );
    } else {
        let result = rpc.submit_operation(&op)?;
        if !result.accepted {
            bail!(
                "submit rejected: {}",
                result.error.unwrap_or_else(|| "(no error)".into())
            );
        }
        info!(
            stsolen = %hex::encode(predicted_addr),
            "deploy submitted; check the explorer or re-run with --confirm"
        );
    }

    println!("{}", hex::encode(predicted_addr));
    Ok(())
}
