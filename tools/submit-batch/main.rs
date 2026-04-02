//! Submit a mock rollup batch to the L1 RPC endpoint.
//!
//! Usage: cargo run -p submit-batch -- [--rpc URL] [--rollup-id ID] [--batches N]

use solen_crypto::blake3_hash;
use solen_rollup_kit::batch::BatchPublisher;
use solen_rollup_kit::prover::{MockProver, ProverBackend};
use solen_rollup_kit::sequencer::{L2Transaction, Sequencer, SequencerConfig};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    let rpc_url = args.iter().position(|a| a == "--rpc")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("https://testnet-rpc.solenchain.io");

    let rollup_id: u64 = args.iter().position(|a| a == "--rollup-id")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    let num_batches: usize = args.iter().position(|a| a == "--batches")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);

    println!("=== Mock Batch Submission ===");
    println!("RPC:       {}", rpc_url);
    println!("Rollup ID: {}", rollup_id);
    println!("Batches:   {}", num_batches);
    println!();

    // First, check rollup status to get the current state root.
    let status = rpc_call(rpc_url, "solen_getRollupStatus", &serde_json::json!([rollup_id]))?;
    let registered = status["result"]["registered"].as_bool().unwrap_or(false);
    if !registered {
        eprintln!("Error: Rollup {} is not registered", rollup_id);
        std::process::exit(1);
    }

    let mut current_state_root = if let Some(hex_str) = status["result"]["last_verified_state_root"].as_str() {
        let mut root = [0u8; 32];
        let bytes: Vec<u8> = (0..hex_str.len()).step_by(2)
            .filter_map(|i| u8::from_str_radix(&hex_str[i..i+2], 16).ok())
            .collect();
        if bytes.len() == 32 { root.copy_from_slice(&bytes); }
        root
    } else {
        [0u8; 32] // genesis
    };

    println!("Current state root: {}", hex(&current_state_root));
    println!();

    let prover = MockProver;
    let publisher = BatchPublisher::new(rollup_id);
    let sequencer = Sequencer::new(SequencerConfig {
        rollup_id,
        max_batch_size: 10,
        ..Default::default()
    });

    for batch_num in 1..=num_batches {
        println!("--- Batch {} ---", batch_num);

        // Create some mock L2 transactions.
        for i in 0..3 {
            let mut sender = [0u8; 32];
            sender[0] = (batch_num % 256) as u8;
            sender[1] = i as u8;
            sequencer.submit(L2Transaction {
                sender,
                nonce: i,
                data: format!("batch{}:tx{}", batch_num, i).into_bytes(),
                gas_limit: 100_000,
            }).unwrap();
        }

        let batch = sequencer.produce_batch().unwrap();
        let batch_data = BatchPublisher::compress_batch(&batch).unwrap();

        // Compute post-state (simulated execution).
        let post_state_root = {
            let mut input = Vec::new();
            input.extend_from_slice(&current_state_root);
            input.extend_from_slice(&batch_data);
            blake3_hash(&input)
        };

        // Generate mock proof.
        let proof = prover.generate_proof(&current_state_root, &post_state_root, &batch_data).unwrap();
        let data_hash = blake3_hash(&batch_data);

        println!("  Txs:        {}", batch.transactions.len());
        println!("  Pre-state:  {}", hex(&current_state_root));
        println!("  Post-state: {}", hex(&post_state_root));
        println!("  Data hash:  {}", hex(&data_hash));
        println!("  Proof:      {}", hex(&proof));

        // Submit via RPC.
        let req = serde_json::json!([{
            "rollup_id": rollup_id,
            "batch_index": batch.batch_index,
            "state_root": hex(&post_state_root),
            "data_hash": hex(&data_hash),
            "proof": hex(&proof),
        }]);

        let result = rpc_call(rpc_url, "solen_submitBatch", &req)?;
        let accepted = result["result"]["accepted"].as_bool().unwrap_or(false);
        let verified = result["result"]["verified"].as_bool().unwrap_or(false);
        let error = result["result"]["error"].as_str();

        if accepted && verified {
            println!("  Result:     VERIFIED");
            current_state_root = post_state_root;
        } else if let Some(err) = error {
            println!("  Result:     FAILED - {}", err);
        } else {
            println!("  Result:     {:?}", result["result"]);
        }
        println!();
    }

    // Final status check.
    let final_status = rpc_call(rpc_url, "solen_getRollupStatus", &serde_json::json!([rollup_id]))?;
    println!("=== Final Status ===");
    println!("{}", serde_json::to_string_pretty(&final_status["result"])?);

    Ok(())
}

fn rpc_call(url: &str, method: &str, params: &serde_json::Value) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let output = std::process::Command::new("curl")
        .args(["-s", "-X", "POST", url])
        .args(["-H", "Content-Type: application/json"])
        .arg("-d")
        .arg(body.to_string())
        .output()?;

    if !output.status.success() {
        return Err(format!("curl failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }

    Ok(serde_json::from_slice(&output.stdout)?)
}
