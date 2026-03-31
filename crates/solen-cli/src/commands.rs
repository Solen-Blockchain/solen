//! CLI command implementations.

use anyhow::Result;
use solen_crypto::{blake3_hash, Keypair};
use solen_types::transaction::{Action, UserOperation};

use crate::rpc::RpcClient;
use crate::wallet::{self, hex_decode, hex_encode};

// ── Status ──────────────────────────────────────────────────────

pub async fn cmd_status(rpc: &RpcClient) -> Result<()> {
    let status = rpc.chain_status().await?;
    let block = rpc.get_latest_block().await?;

    println!("Solen Network Status");
    println!("────────────────────────────────────────");
    println!("  Height:      {}", status.height);
    println!("  State root:  {}...{}", &status.latest_state_root[..12], &status.latest_state_root[status.latest_state_root.len()-8..]);
    println!("  Pending ops: {}", status.pending_ops);
    println!("  Epoch:       {}", block.epoch);
    println!("  Proposer:    {}...", &block.proposer[..16]);
    println!("  Gas used:    {}", block.gas_used);

    Ok(())
}

// ── Balance ─────────────────────────────────────────────────────

// ── Validators ──────────────────────────────────────────────────

pub async fn cmd_validators(rpc: &RpcClient) -> Result<()> {
    let validators = rpc.get_validators().await?;

    if validators.is_empty() {
        println!("No validators registered.");
        return Ok(());
    }

    println!(
        "{:<6} {:<18} {:>14} {:>14} {:>14}",
        "STATUS", "ADDRESS", "SELF STAKE", "DELEGATED", "TOTAL"
    );
    println!("{}", "─".repeat(70));

    for v in &validators {
        let status = if v.is_active {
            if v.is_genesis { "GENSIS" } else { "ACTIVE" }
        } else {
            "INACTV"
        };

        println!(
            "{:<6} {}...  {:>14} {:>14} {:>14}",
            status,
            &v.address[..16],
            v.self_stake,
            v.total_delegated,
            v.total_stake,
        );
    }

    println!("\n{} validators ({} active)",
        validators.len(),
        validators.iter().filter(|v| v.is_active).count(),
    );

    Ok(())
}

// ── Claim Vesting ───────────────────────────────────────────────

pub async fn cmd_claim_vesting(rpc: &RpcClient, from: &str, chain_id: u64) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Vesting system contract address.
    let vesting_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x06;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Call {
            target: vesting_addr,
            method: "claim".to_string(),
            args: vec![],
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Vesting claim submitted successfully.");
        println!("  Vested tokens will be credited to your account.");
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

// ── Stake ───────────────────────────────────────────────────────

// ── Governance ─────────────────────────────────────────────────

pub async fn cmd_propose_block_time(
    rpc: &RpcClient,
    from: &str,
    new_block_time_ms: u64,
    description: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    let mut args = Vec::new();
    args.extend_from_slice(&new_block_time_ms.to_le_bytes());
    args.extend_from_slice(description.as_bytes());

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Call {
            target: gov_addr,
            method: "propose_set_block_time".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Proposal submitted: change block time to {}ms", new_block_time_ms);
        println!("  Description: {}", description);
        println!("\nVoting period: 14 epochs. Use `solen vote` to vote.");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_vote(
    rpc: &RpcClient,
    from: &str,
    proposal_id: u64,
    support: bool,
    weight: u128,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    let mut args = Vec::new();
    args.extend_from_slice(&proposal_id.to_le_bytes());
    args.push(if support { 1 } else { 0 });
    args.extend_from_slice(&weight.to_le_bytes());

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Call {
            target: gov_addr,
            method: "vote".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Vote submitted: {} on proposal #{}", if support { "YES" } else { "NO" }, proposal_id);
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_finalize_proposal(
    rpc: &RpcClient,
    from: &str,
    proposal_id: u64,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Call {
            target: gov_addr,
            method: "finalize".to_string(),
            args: proposal_id.to_le_bytes().to_vec(),
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Proposal #{} finalized.", proposal_id);
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_execute_proposal(
    rpc: &RpcClient,
    from: &str,
    proposal_id: u64,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Call {
            target: gov_addr,
            method: "execute".to_string(),
            args: proposal_id.to_le_bytes().to_vec(),
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Proposal #{} executed!", proposal_id);
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_register_validator(
    rpc: &RpcClient,
    from: &str,
    amount: u128,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Build args: amount[16 LE]
    let amount_bytes = amount.to_le_bytes();
    let args = hex_encode(&amount_bytes);

    let staking_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x01;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Call {
            target: staking_addr,
            method: "register".to_string(),
            args: hex_decode(&args)?,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Validator registered successfully.");
        println!("  Validator ID: {}", sender_hex);
        println!("  Self-stake:   {} SOLEN", format_solen(amount));
        println!("\nStart your validator node with --validator-seed to begin producing blocks.");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_stake(
    rpc: &RpcClient,
    from: &str,
    validator: &str,
    amount: u128,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let validator_id = resolve_account_id(validator)?;
    let validator_bytes = hex_decode(&validator_id)?;

    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Build args: validator[32] + amount[16]
    let mut args = Vec::new();
    args.extend_from_slice(&validator_bytes);
    args.extend_from_slice(&amount.to_le_bytes());


    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Call {
            target: {
                let mut t = [0xFFu8; 32];
                t[31] = 0x01;
                t
            },
            method: "delegate".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Stake submitted successfully.");
        println!("  Delegated {} SOLEN to {}...", format_solen(amount), &validator_id[..16]);
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_unstake(
    rpc: &RpcClient,
    from: &str,
    validator: &str,
    amount: u128,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let validator_id = resolve_account_id(validator)?;
    let validator_bytes = hex_decode(&validator_id)?;

    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Build args: validator[32] + amount[16] + epoch[8]
    let mut args = Vec::new();
    args.extend_from_slice(&validator_bytes);
    args.extend_from_slice(&amount.to_le_bytes());
    args.extend_from_slice(&0u64.to_le_bytes()); // epoch placeholder

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Call {
            target: {
                let mut t = [0xFFu8; 32];
                t[31] = 0x01;
                t
            },
            method: "undelegate".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Unstake submitted successfully.");
        println!("  Undelegating {} SOLEN from {}...", format_solen(amount), &validator_id[..16]);
        println!("  Funds available after unbonding period (7 epochs).");
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

// ── Balance ─────────────────────────────────────────────────────

pub async fn cmd_balance(rpc: &RpcClient, account: &str) -> Result<()> {
    let account_id = resolve_account_id(account)?;
    let balance = rpc.get_balance(&account_id).await?;
    println!("{}", balance);
    Ok(())
}

// ── Account ─────────────────────────────────────────────────────

pub async fn cmd_account(rpc: &RpcClient, account: &str) -> Result<()> {
    let account_id = resolve_account_id(account)?;
    let info = rpc.get_account(&account_id).await?;

    println!("Account");
    println!("────────────────────────────────────────");
    println!("  ID:        {}", info.id);
    println!("  Balance:   {}", info.balance);
    println!("  Nonce:     {}", info.nonce);
    println!("  Code hash: {}", if info.code_hash.chars().all(|c| c == '0') { "(none)".to_string() } else { format!("{}...", &info.code_hash[..16]) });

    Ok(())
}

// ── Block ───────────────────────────────────────────────────────

pub async fn cmd_block(rpc: &RpcClient, height: Option<u64>) -> Result<()> {
    let block = match height {
        Some(h) => rpc.get_block(h).await?,
        None => rpc.get_latest_block().await?,
    };

    println!("Block #{}", block.height);
    println!("────────────────────────────────────────");
    println!("  Epoch:      {}", block.epoch);
    println!("  Proposer:   {}...", &block.proposer[..16]);
    println!("  State root: {}...", &block.state_root[..16]);
    println!("  Txs:        {}", block.tx_count);
    println!("  Gas used:   {}", block.gas_used);
    println!("  Time:       {}", format_timestamp(block.timestamp_ms));

    Ok(())
}

// ── Transfer ────────────────────────────────────────────────────

pub async fn cmd_transfer(
    rpc: &RpcClient,
    from: &str,
    to: &str,
    amount: u128,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let to_id = resolve_account_id(to)?;
    let to_bytes = hex_decode(&to_id)?;
    let mut to_arr = [0u8; 32];
    to_arr.copy_from_slice(&to_bytes);

    // Get current nonce.
    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Transfer {
            to: to_arr,
            amount,
        }],
        max_fee: 100_000,
        signature: vec![],
    };

    sign_op(&mut op, &kp, chain_id);

    // Simulate first.
    let op_json = serde_json::to_value(&op)?;
    let sim = rpc.simulate_operation(op_json.clone()).await?;
    if !sim.success {
        println!("Simulation failed: {}", sim.error.unwrap_or_default());
        return Ok(());
    }

    println!("Simulated OK (gas: {}). Submitting...", sim.gas_used);

    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Transaction submitted successfully.");
        println!("  From:   {} ({})", from, &sender_hex[..12]);
        println!("  To:     {} ({}...)", to, &to_id[..12]);
        println!("  Amount: {}", amount);
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

// ── Deploy ──────────────────────────────────────────────────────

pub async fn cmd_deploy(rpc: &RpcClient, from: &str, wasm_path: &str, chain_id: u64) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let code = std::fs::read(wasm_path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {}", wasm_path, e))?;

    let code_hash = blake3_hash(&code);
    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Generate a deterministic salt from sender + nonce.
    let mut salt_preimage = Vec::new();
    salt_preimage.extend_from_slice(&sender_id);
    salt_preimage.extend_from_slice(&info.nonce.to_le_bytes());
    let salt = blake3_hash(&salt_preimage);

    // Predict the contract address.
    let mut addr_preimage = Vec::new();
    addr_preimage.extend_from_slice(&sender_id);
    addr_preimage.extend_from_slice(&salt);
    addr_preimage.extend_from_slice(&code_hash);
    let contract_id = blake3_hash(&addr_preimage);

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Deploy {
            code,
            salt,
        }],
        max_fee: 1_000_000,
        signature: vec![],
    };

    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let sim = rpc.simulate_operation(op_json.clone()).await?;
    if !sim.success {
        println!("Simulation failed: {}", sim.error.unwrap_or_default());
        return Ok(());
    }

    println!("Simulated OK (gas: {}). Deploying...", sim.gas_used);

    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Contract deployed successfully.");
        println!("  Contract ID: {}", hex_encode(&contract_id));
        println!("  Code hash:   {}...", &hex_encode(&code_hash)[..16]);
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

// ── Call ─────────────────────────────────────────────────────────

pub async fn cmd_call(
    rpc: &RpcClient,
    from: &str,
    target: &str,
    method: &str,
    args_hex: Option<&str>,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let target_id = resolve_account_id(target)?;
    let target_bytes = hex_decode(&target_id)?;
    let mut target_arr = [0u8; 32];
    target_arr.copy_from_slice(&target_bytes);

    let args = match args_hex {
        Some(hex) => hex_decode(hex)?,
        None => vec![],
    };

    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::Call {
            target: target_arr,
            method: method.to_string(),
            args,
        }],
        max_fee: 1_000_000,
        signature: vec![],
    };

    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let sim = rpc.simulate_operation(op_json.clone()).await?;
    if !sim.success {
        println!("Simulation failed: {}", sim.error.unwrap_or_default());
        return Ok(());
    }

    println!("Simulated OK (gas: {}). Submitting...", sim.gas_used);

    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Call submitted successfully.");
        println!("  Target: {}...", &target_id[..16]);
        println!("  Method: {}", method);
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

// ── Key Management ──────────────────────────────────────────────

pub fn cmd_key_generate(name: &str) -> Result<()> {
    let mut ks = wallet::load_keystore()?;

    if ks.keys.contains_key(name) {
        println!("Key '{}' already exists.", name);
        return Ok(());
    }

    let key = wallet::generate_key(name)?;
    println!("Generated key '{}'", name);
    println!("  Account ID:  {}", key.account_id_hex);
    println!("  Public key:  {}", key.public_key_hex);
    println!("  Seed:        {} (SAVE THIS!)", key.seed_hex);

    ks.keys.insert(name.to_string(), key);
    wallet::save_keystore(&ks)?;
    println!("\nSaved to ~/.solen/keys.json");

    Ok(())
}

pub fn cmd_key_import(name: &str, seed_hex: &str) -> Result<()> {
    let mut ks = wallet::load_keystore()?;

    let key = wallet::import_key(name, seed_hex)?;
    println!("Imported key '{}'", name);
    println!("  Account ID: {}", key.account_id_hex);
    println!("  Public key: {}", key.public_key_hex);

    ks.keys.insert(name.to_string(), key);
    wallet::save_keystore(&ks)?;

    Ok(())
}

pub fn cmd_key_list() -> Result<()> {
    let ks = wallet::load_keystore()?;

    if ks.keys.is_empty() {
        println!("No keys found. Generate one with: solen key generate <name>");
        return Ok(());
    }

    println!("{:<12} {:<20} {}", "NAME", "ACCOUNT ID", "PUBLIC KEY");
    println!("{}", "─".repeat(70));

    let mut keys: Vec<_> = ks.keys.values().collect();
    keys.sort_by(|a, b| a.name.cmp(&b.name));

    for key in keys {
        println!(
            "{:<12} {}...  {}...",
            key.name,
            &key.account_id_hex[..16],
            &key.public_key_hex[..16],
        );
    }

    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────

/// Resolve an account identifier — either a key name or a hex ID.
fn resolve_account_id(input: &str) -> Result<String> {
    // If it looks like hex (64 chars), use as-is.
    let clean = input.strip_prefix("0x").unwrap_or(input);
    if clean.len() == 64 && clean.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(clean.to_string());
    }

    // Try loading from keystore.
    let ks = wallet::load_keystore()?;
    if let Some(key) = ks.keys.get(input) {
        return Ok(key.account_id_hex.clone());
    }

    // Treat as a name and convert to account ID.
    let id = wallet::name_to_account_id(input);
    Ok(hex_encode(&id))
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

// ── Multi-sig ──────────────────────────────────────────────────

pub async fn cmd_multisig(
    rpc: &RpcClient,
    from: &str,
    threshold: u16,
    signer_hexes: &[String],
    chain_id: u64,
) -> Result<()> {
    use solen_types::account::AuthMethod;

    if signer_hexes.is_empty() {
        anyhow::bail!("at least one signer is required");
    }
    if threshold == 0 || threshold as usize > signer_hexes.len() {
        anyhow::bail!(
            "threshold must be between 1 and {} (number of signers)",
            signer_hexes.len()
        );
    }

    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = hex_encode(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Parse signer public keys.
    let mut signers = Vec::new();
    for hex_str in signer_hexes {
        let bytes = hex_decode(hex_str)?;
        if bytes.len() != 32 {
            anyhow::bail!("signer key must be 32 bytes (64 hex chars), got {}", bytes.len());
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        signers.push(key);
    }

    let auth_methods = vec![AuthMethod::Threshold {
        signers: signers.clone(),
        threshold,
    }];

    let mut op = UserOperation {
        sender: sender_id,
        nonce: info.nonce,
        actions: vec![Action::SetAuth { auth_methods }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Account converted to {}-of-{} multi-sig.", threshold, signers.len());
        println!("Signers:");
        for (i, s) in signers.iter().enumerate() {
            println!("  {}: {}", i + 1, hex_encode(s));
        }
        println!("\nAll future operations require {} signature(s).", threshold);
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

/// Format base units as human-readable SOLEN amount.
fn format_solen(base_units: u128) -> String {
    let whole = base_units / 100_000_000;
    let frac = base_units % 100_000_000;
    if frac == 0 {
        whole.to_string()
    } else {
        let frac_str = format!("{:08}", frac).trim_end_matches('0').to_string();
        format!("{}.{}", whole, frac_str)
    }
}

fn format_timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ago = now.saturating_sub(secs);
    if ago < 60 {
        format!("{}s ago", ago)
    } else if ago < 3600 {
        format!("{}m ago", ago / 60)
    } else {
        format!("{}h ago", ago / 3600)
    }
}
