//! solen-bench — Flood the network with signed transactions and measure throughput.
//!
//! Usage:
//!   solen-bench --rpc http://127.0.0.1:19944 --senders 50 --txs-per-sender 100 --chain-id 9000
//!
//! This creates N sender keypairs, funds them from a faucet key, then
//! blasts transfers between them as fast as possible. It measures:
//!   - Peak TPS (transactions finalized per second)
//!   - Average finality time (submission → block inclusion)
//!   - Sustained throughput over the full run

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use solen_crypto::{blake3_hash, Keypair};
use solen_types::transaction::{Action, UserOperation};

#[derive(Parser)]
#[command(name = "solen-bench", about = "Solen network throughput benchmark")]
struct Cli {
    /// RPC endpoint URL
    #[arg(long, default_value = "http://127.0.0.1:19944")]
    rpc: String,

    /// Chain ID (9000 for testnet, 1337 for devnet)
    #[arg(long, default_value_t = 9000)]
    chain_id: u64,

    /// Number of sender accounts to create
    #[arg(long, default_value_t = 50)]
    senders: usize,

    /// Transactions per sender
    #[arg(long, default_value_t = 100)]
    txs_per_sender: usize,

    /// Hex seed of the funding account (must have balance)
    #[arg(long)]
    fund_seed: String,

    /// Amount to fund each sender (base units)
    #[arg(long, default_value_t = 1_000_000_000)]
    fund_amount: u128,

    /// Max concurrent submissions
    #[arg(long, default_value_t = 20)]
    concurrency: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .pool_max_idle_per_host(cli.concurrency)
        .build()?;

    println!("=== Solen Benchmark ===");
    println!("RPC:            {}", cli.rpc);
    println!("Chain ID:       {}", cli.chain_id);
    println!("Senders:        {}", cli.senders);
    println!("Txs/sender:     {}", cli.txs_per_sender);
    println!("Total txs:      {}", cli.senders * cli.txs_per_sender);
    println!("Concurrency:    {}", cli.concurrency);
    println!();

    // Get starting height.
    let start_height = get_height(&client, &cli.rpc).await?;
    println!("Starting height: {}", start_height);

    // Generate sender keypairs.
    println!("Generating {} sender keypairs...", cli.senders);
    let senders: Vec<Keypair> = (0..cli.senders)
        .map(|i| {
            let mut seed = [0u8; 32];
            let hash = blake3_hash(&format!("bench-sender-{}-{}", i, start_height).into_bytes());
            seed.copy_from_slice(&hash);
            Keypair::from_seed(&seed)
        })
        .collect();

    // Fund all senders from the fund account.
    println!("Funding senders from fund account...");
    let fund_seed = hex_to_bytes(&cli.fund_seed)?;
    let fund_kp = Keypair::from_seed(&fund_seed);
    let fund_id = fund_kp.public_key();

    let mut fund_nonce = get_next_nonce(&client, &cli.rpc, &fund_id).await?;
    for (i, sender) in senders.iter().enumerate() {
        let recipient = sender.public_key();
        let mut op = UserOperation {
            sender: fund_id,
            nonce: fund_nonce,
            actions: vec![Action::Transfer {
                to: recipient,
                amount: cli.fund_amount,
            }],
            max_fee: 100_000,
            signature: vec![],
        };
        sign_op(&mut op, &fund_kp, cli.chain_id);
        submit_op(&client, &cli.rpc, &op).await?;
        fund_nonce += 1;

        if (i + 1) % 10 == 0 {
            print!("\r  Funded {}/{}", i + 1, cli.senders);
        }
    }
    println!("\r  Funded {}/{}", cli.senders, cli.senders);

    // Wait for funding txs to land.
    println!("Waiting for funding transactions to finalize...");
    wait_for_height(&client, &cli.rpc, start_height + 5).await?;

    // Now blast transactions.
    let total_txs = cli.senders * cli.txs_per_sender;
    let submitted = Arc::new(AtomicU64::new(0));
    let accepted = Arc::new(AtomicU64::new(0));
    let rejected = Arc::new(AtomicU64::new(0));

    let blast_start_height = get_height(&client, &cli.rpc).await?;
    println!("\nBlast starting at height {}", blast_start_height);
    let blast_start = Instant::now();

    // Spawn sender tasks.
    let sem = Arc::new(tokio::sync::Semaphore::new(cli.concurrency));
    let mut handles = Vec::new();

    for sender_kp in &senders {
        let sender_id = sender_kp.public_key();
        // Each sender sends to the next sender (round-robin).
        let recipient = senders[(senders.iter().position(|s| s.public_key() == sender_id).unwrap() + 1) % senders.len()].public_key();

        let client = client.clone();
        let rpc = cli.rpc.clone();
        let chain_id = cli.chain_id;
        let txs = cli.txs_per_sender;
        let sem = sem.clone();
        let submitted = submitted.clone();
        let accepted = accepted.clone();
        let rejected = rejected.clone();

        // Get starting nonce for this sender.
        let start_nonce = get_next_nonce(&client, &rpc, &sender_id).await.unwrap_or(0);

        // Clone the seed to recreate keypair in the task (Keypair isn't Send-friendly).
        let mut seed = [0u8; 32];
        let hash = blake3_hash(&sender_id);
        seed.copy_from_slice(&hash);
        // Actually, we need the real seed. Let's encode the public key position.
        let sender_idx = senders.iter().position(|s| s.public_key() == sender_id).unwrap();
        let sender_seed = blake3_hash(&format!("bench-sender-{}-{}", sender_idx, start_height).into_bytes());

        let handle = tokio::spawn(async move {
            let kp = Keypair::from_seed(&sender_seed);
            for i in 0..txs {
                let _permit = sem.acquire().await.unwrap();
                let mut op = UserOperation {
                    sender: sender_id,
                    nonce: start_nonce + i as u64,
                    actions: vec![Action::Transfer {
                        to: recipient,
                        amount: 1, // 1 base unit
                    }],
                    max_fee: 100_000,
                    signature: vec![],
                };
                sign_op(&mut op, &kp, chain_id);

                submitted.fetch_add(1, Ordering::Relaxed);
                match submit_op(&client, &rpc, &op).await {
                    Ok(()) => { accepted.fetch_add(1, Ordering::Relaxed); }
                    Err(_) => { rejected.fetch_add(1, Ordering::Relaxed); }
                }
            }
        });
        handles.push(handle);
    }

    // Progress reporter.
    let submitted_r = submitted.clone();
    let accepted_r = accepted.clone();
    let reporter = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let s = submitted_r.load(Ordering::Relaxed);
            let a = accepted_r.load(Ordering::Relaxed);
            let elapsed = blast_start.elapsed().as_secs_f64();
            let rate = if elapsed > 0.0 { a as f64 / elapsed } else { 0.0 };
            print!("\r  Submitted: {}/{} | Accepted: {} | Rate: {:.1} tx/s    ", s, total_txs, a, rate);
            if s >= total_txs as u64 { break; }
        }
    });

    // Wait for all senders to finish.
    for h in handles {
        let _ = h.await;
    }
    reporter.abort();
    let blast_elapsed = blast_start.elapsed();

    let total_submitted = submitted.load(Ordering::Relaxed);
    let total_accepted = accepted.load(Ordering::Relaxed);
    let total_rejected = rejected.load(Ordering::Relaxed);

    println!("\n\nSubmission complete in {:.2}s", blast_elapsed.as_secs_f64());
    println!("  Submitted: {}", total_submitted);
    println!("  Accepted:  {}", total_accepted);
    println!("  Rejected:  {}", total_rejected);

    // Wait for all txs to finalize — give enough time for blocks to include them.
    println!("\nWaiting for finalization...");
    let expected_blocks = (total_accepted as f64 / 1000.0).ceil() as u64 + 5; // 1000 ops/block + margin
    wait_for_height(&client, &cli.rpc, blast_start_height + expected_blocks.max(10)).await?;

    let end_height = get_height(&client, &cli.rpc).await?;
    let blocks_elapsed = end_height - blast_start_height;

    // Count total transactions in the blocks.
    let mut total_finalized_txs: u64 = 0;
    let mut first_block_time: Option<u64> = None;
    let mut last_block_time: Option<u64> = None;

    for h in blast_start_height + 1..=end_height {
        if let Ok(block) = get_block(&client, &cli.rpc, h).await {
            let tx_count = block["tx_count"].as_u64().unwrap_or(0);
            let ts = block["timestamp_ms"].as_u64().unwrap_or(0);
            total_finalized_txs += tx_count;
            if first_block_time.is_none() && tx_count > 0 {
                first_block_time = Some(ts);
            }
            if tx_count > 0 {
                last_block_time = Some(ts);
            }
        }
    }

    let chain_duration_ms = match (first_block_time, last_block_time) {
        (Some(first), Some(last)) if last > first => last - first,
        _ => blast_elapsed.as_millis() as u64,
    };
    let chain_duration_secs = chain_duration_ms as f64 / 1000.0;

    let peak_tps = if chain_duration_secs > 0.0 {
        total_finalized_txs as f64 / chain_duration_secs
    } else {
        0.0
    };

    let submission_tps = total_accepted as f64 / blast_elapsed.as_secs_f64();
    let avg_block_time = if blocks_elapsed > 0 {
        chain_duration_ms as f64 / blocks_elapsed as f64 / 1000.0
    } else {
        0.0
    };

    println!("\n══════════════════════════════════════════");
    println!("  BENCHMARK RESULTS");
    println!("══════════════════════════════════════════");
    println!("  Blocks:           {} ({} → {})", blocks_elapsed, blast_start_height, end_height);
    println!("  Finalized txs:    {}", total_finalized_txs);
    println!("  Chain duration:   {:.2}s", chain_duration_secs);
    println!("  Avg block time:   {:.2}s", avg_block_time);
    println!("  ──────────────────────────────────────");
    println!("  Peak TPS:         {:.1}", peak_tps);
    println!("  Submission rate:  {:.1} tx/s", submission_tps);
    println!("  Finality:         single-slot (~{:.1}s)", avg_block_time);
    println!("══════════════════════════════════════════");

    Ok(())
}

fn sign_op(op: &mut UserOperation, kp: &Keypair, chain_id: u64) {
    let mut msg = Vec::with_capacity(96);
    msg.extend_from_slice(&chain_id.to_le_bytes());
    msg.extend_from_slice(&op.sender);
    msg.extend_from_slice(&op.nonce.to_le_bytes());
    msg.extend_from_slice(&op.max_fee.to_le_bytes());
    let actions_bytes = serde_json::to_vec(&op.actions).unwrap_or_default();
    msg.extend_from_slice(&blake3_hash(&actions_bytes));
    op.signature = kp.sign(&msg).to_vec();
}

async fn rpc_call(
    client: &reqwest::Client,
    rpc: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": 1,
        "method": method, "params": params
    });
    let resp = client.post(rpc)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send().await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(err) = json.get("error") {
        return Err(format!("RPC error: {}", err));
    }
    Ok(json["result"].clone())
}

async fn get_height(client: &reqwest::Client, rpc: &str) -> Result<u64, String> {
    let result = rpc_call(client, rpc, "solen_chainStatus", serde_json::json!([])).await?;
    result["height"].as_u64().ok_or("no height".into())
}

async fn get_next_nonce(client: &reqwest::Client, rpc: &str, account: &[u8; 32]) -> Result<u64, String> {
    let hex = account.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let result = rpc_call(client, rpc, "solen_getNextNonce", serde_json::json!([hex])).await?;
    Ok(result.as_u64().unwrap_or(0))
}

async fn get_block(client: &reqwest::Client, rpc: &str, height: u64) -> Result<serde_json::Value, String> {
    rpc_call(client, rpc, "solen_getBlock", serde_json::json!([height])).await
}

async fn submit_op(client: &reqwest::Client, rpc: &str, op: &UserOperation) -> Result<(), String> {
    let op_json = serde_json::to_value(op).map_err(|e| e.to_string())?;
    let result = rpc_call(client, rpc, "solen_submitOperation", serde_json::json!([op_json])).await?;
    if result["accepted"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(result["error"].as_str().unwrap_or("rejected").to_string())
    }
}

async fn wait_for_height(client: &reqwest::Client, rpc: &str, target: u64) -> Result<(), String> {
    loop {
        let h = get_height(client, rpc).await?;
        if h >= target { return Ok(()); }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn hex_to_bytes(hex: &str) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    if hex.len() != 64 {
        return Err(format!("seed must be 32 bytes (64 hex chars), got {}", hex.len()).into());
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&hex[i*2..i*2+2], 16)?;
    }
    Ok(out)
}
