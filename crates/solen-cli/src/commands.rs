//! CLI command implementations.

use anyhow::Result;
use solen_crypto::{blake3_hash, Keypair};
use solen_types::transaction::{Action, UserOperation};

use solen_types::encoding::{account_to_base58, hex_encode, parse_address};

use crate::rpc::RpcClient;
use crate::wallet::{self, hex_decode};

// ── Status ──────────────────────────────────────────────────────

pub async fn cmd_status(rpc: &RpcClient) -> Result<()> {
    let status = rpc.chain_status().await?;
    let block = rpc.get_latest_block().await?;

    println!("Solen Network Status");
    println!("────────────────────────────────────────");
    println!("  Height:      {}", status.height);
    println!("  State root:  {}...{}", &status.state_root[..12], &status.state_root[status.state_root.len()-8..]);
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

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Vesting system contract address.
    let vesting_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x06;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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

pub async fn cmd_propose_set_bridge_relayer(
    rpc: &RpcClient,
    from: &str,
    relayer: &str,
    description: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let relayer_id = resolve_account_id(relayer)?;
    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    // args: relayer[32] + desc[...]
    let mut args = Vec::new();
    args.extend_from_slice(&relayer_id);
    args.extend_from_slice(description.as_bytes());

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: gov_addr,
            method: "propose_set_bridge_relayer".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Proposal submitted: set bridge relayer to {}", account_to_base58(&relayer_id));
        println!("  Description: {}", description);
        println!("\nVoting period: 14 epochs. Use `solen vote`, then finalize-proposal and execute-proposal.");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_propose_set_vesting_admin(
    rpc: &RpcClient,
    from: &str,
    admin: &str,
    description: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let admin_id = resolve_account_id(admin)?;
    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    // args: admin[32] + desc[...]
    let mut args = Vec::new();
    args.extend_from_slice(&admin_id);
    args.extend_from_slice(description.as_bytes());

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: gov_addr,
            method: "propose_set_vesting_admin".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Proposal submitted: set vesting admin to {}", account_to_base58(&admin_id));
        println!("  Description: {}", description);
        println!("\nVoting period: 14 epochs. Use `solen vote`, then finalize-proposal and execute-proposal.");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_propose_set_voting_period(
    rpc: &RpcClient,
    from: &str,
    epochs: u64,
    description: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    // args: epochs[8] + desc[...]
    let mut args = Vec::new();
    args.extend_from_slice(&epochs.to_le_bytes());
    args.extend_from_slice(description.as_bytes());

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: gov_addr,
            method: "propose_set_voting_period".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Proposal submitted: set governance voting period to {} epochs", epochs);
        println!("  Description: {}", description);
        println!("\nUse `solen vote`, then finalize-proposal and execute-proposal after the timelock.");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_propose_migrate_team_pool_to_vesting(
    rpc: &RpcClient,
    from: &str,
    description: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    // args: description bytes only (the handler reads the whole arg as desc).
    let args = description.as_bytes().to_vec();

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: gov_addr,
            method: "propose_migrate_team_pool_to_vesting".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Proposal submitted: migrate team pool -> vesting vault");
        println!("  Description: {}", description);
        println!("\nVoting period: 14 epochs. Use `solen vote` to vote, then");
        println!("`solen finalize-proposal` and `solen execute-proposal` after the timelock.");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_propose_block_time(
    rpc: &RpcClient,
    from: &str,
    new_block_time_ms: u64,
    description: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    let mut args = Vec::new();
    args.extend_from_slice(&new_block_time_ms.to_le_bytes());
    args.extend_from_slice(description.as_bytes());

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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

pub async fn cmd_propose_min_stake(
    rpc: &RpcClient,
    from: &str,
    new_min_stake: u128,
    description: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    let mut args = Vec::new();
    args.extend_from_slice(&new_min_stake.to_le_bytes());
    args.extend_from_slice(description.as_bytes());

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: gov_addr,
            method: "propose_set_min_validator_stake".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Proposal submitted: change minimum validator stake to {} base units", new_min_stake);
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
    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    let mut args = Vec::new();
    args.extend_from_slice(&proposal_id.to_le_bytes());
    args.push(if support { 1 } else { 0 });
    args.extend_from_slice(&weight.to_le_bytes());

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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
    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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
    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let gov_addr = { let mut t = [0xFFu8; 32]; t[31] = 0x02; t };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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

    let sender_hex = account_to_base58(&sender_id);
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
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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
    let validator_bytes = validator_id.to_vec();

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Build args: validator[32] + amount[16]
    let mut args = Vec::new();
    args.extend_from_slice(&validator_bytes);
    args.extend_from_slice(&amount.to_le_bytes());


    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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
        println!("  Delegated {} SOLEN to {}", format_solen(amount), account_to_base58(&validator_id));
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
    let validator_bytes = validator_id.to_vec();

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Build args: validator[32] + amount[16] + epoch[8]
    let mut args = Vec::new();
    args.extend_from_slice(&validator_bytes);
    args.extend_from_slice(&amount.to_le_bytes());
    args.extend_from_slice(&0u64.to_le_bytes()); // epoch placeholder

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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
        println!("  Undelegating {} SOLEN from {}", format_solen(amount), account_to_base58(&validator_id));
        println!("  Funds available after unbonding period (7 epochs).");
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_withdraw_stake(rpc: &RpcClient, from: &str, chain_id: u64) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let staking_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x01;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: staking_addr,
            method: "withdraw".to_string(),
            args: vec![],
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Withdraw submitted. Matured unstaked tokens will be credited to your balance.");
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_set_vesting_admin(rpc: &RpcClient, from: &str, new_admin: &str, chain_id: u64) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let admin_id = resolve_account_id(new_admin)?;

    let sender_hex = account_to_base58(&sender_id);

    let vesting_addr = solen_types::system::VESTING_ADDRESS;

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(0),
        actions: vec![Action::Call {
            target: vesting_addr,
            method: "set_vesting_admin".to_string(),
            args: admin_id.to_vec(),
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Vesting admin set to {}", account_to_base58(&admin_id));
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_add_vesting(
    rpc: &RpcClient,
    from: &str,
    recipient: &str,
    amount: u128,
    vesting_type: &str,
    cliff_months: Option<u64>,
    vest_months: Option<u64>,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;
    let recipient_id = resolve_account_id(recipient)?;

    let sender_hex = account_to_base58(&sender_id);

    // Epochs per month, calibrated to mainnet timing (100 blocks/epoch × 6s
    // = 600s/epoch → 52,596 epochs/year). Must match
    // solen_system_contracts::vesting::EPOCHS_PER_MONTH so CLI-created custom
    // schedules convert months → epochs the same way the contract interprets them.
    const EPOCHS_PER_MONTH: u64 = 52_596 / 12;

    // Build args: recipient[32] + amount[16] + type[1] + (optional custom: cliff[8] + total[8])
    let mut args = Vec::with_capacity(65);
    args.extend_from_slice(&recipient_id);
    args.extend_from_slice(&amount.to_le_bytes());

    let type_name = match vesting_type {
        "team" => { args.push(0); "Team (1yr cliff, 3yr vest)" }
        "investor" => { args.push(1); "Investor (6mo cliff, 2yr vest)" }
        "validator" => { args.push(2); "Validator (3mo cliff, 1yr vest)" }
        "custom" => {
            let cliff = cliff_months.unwrap_or(3);
            let vest = vest_months.unwrap_or(12);
            let cliff_ep = cliff * EPOCHS_PER_MONTH;
            let total_ep = cliff_ep + vest * EPOCHS_PER_MONTH;
            args.push(3);
            args.extend_from_slice(&cliff_ep.to_le_bytes());
            args.extend_from_slice(&total_ep.to_le_bytes());
            "Custom"
        }
        _ => anyhow::bail!("invalid vesting type: {vesting_type} (use team, investor, validator, or custom)"),
    };

    let vesting_addr = solen_types::system::VESTING_ADDRESS;

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(0),
        actions: vec![Action::Call {
            target: vesting_addr,
            method: "add_vesting".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let solen_amount = amount as f64 / 1e8;
    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Vesting schedule added:");
        println!("  Recipient: {}", account_to_base58(&recipient_id));
        println!("  Amount:    {:.2} SOLEN", solen_amount);
        println!("  Type:      {}", type_name);
        println!("  Starts:    current epoch (vesting clock begins now)");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }
    Ok(())
}

pub async fn cmd_unjail(rpc: &RpcClient, from: &str, chain_id: u64) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let staking_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x01;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: staking_addr,
            method: "unjail".to_string(),
            args: vec![],
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Unjail submitted. Validator will be reactivated at the next epoch.");
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

// ── Bridge ─────────────────────────────────────────────────────

pub async fn cmd_bridge_to_base(
    rpc: &RpcClient,
    from: &str,
    base_address: &str,
    amount_str: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    // Parse Base address (20 bytes, 0x-prefixed).
    let base_hex = base_address.strip_prefix("0x").unwrap_or(base_address);
    if base_hex.len() != 40 {
        anyhow::bail!("invalid Base address: expected 40 hex chars (20 bytes), got {}", base_hex.len());
    }
    let base_bytes = hex_decode(base_hex)?;

    // Parse amount (SOLEN -> base units).
    let amount = parse_solen_amount(amount_str)?;

    // Build args: base_recipient[20] + amount[16]
    let mut args = Vec::with_capacity(36);
    args.extend_from_slice(&base_bytes);
    args.extend_from_slice(&amount.to_le_bytes());

    let bridge_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x03; // Bridge system contract
        t
    };

    let sender_hex = account_to_base58(&sender_id);
    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(0),
        actions: vec![Action::Call {
            target: bridge_addr,
            method: "bridge_to_base".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let sim = rpc.simulate_operation(op_json.clone()).await?;
    if !sim.success {
        println!("Simulation failed: {}", sim.error.unwrap_or_default());
        return Ok(());
    }

    println!("Simulated OK. Bridging {} SOLEN to Base address {}...", amount_str, base_address);

    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Bridge deposit submitted.");
        println!("  From:         {} ({})", from, sender_hex);
        println!("  To (Base):    {}", base_address);
        println!("  Amount:       {} SOLEN", amount_str);
        println!("\nThe relayer will mint wSOLEN on Base once this transaction is finalized.");
    } else {
        println!("Rejected: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

fn parse_solen_amount(s: &str) -> Result<u128> {
    const DECIMALS: u128 = 100_000_000; // 1 SOLEN = 1e8 base units
    if let Some(dot_pos) = s.find('.') {
        let whole: u128 = s[..dot_pos].parse()?;
        let frac_str = &s[dot_pos + 1..];
        let frac_len = frac_str.len().min(8);
        let frac: u128 = frac_str[..frac_len].parse()?;
        let multiplier = 10u128.pow(8 - frac_len as u32);
        Ok(whole * DECIMALS + frac * multiplier)
    } else {
        let whole: u128 = s.parse()?;
        Ok(whole * DECIMALS)
    }
}

// ── Balance ─────────────────────────────────────────────────────

pub async fn cmd_balance(rpc: &RpcClient, account: &str) -> Result<()> {
    let account_id = resolve_account_id(account)?;
    let balance = rpc.get_balance(&account_to_base58(&account_id)).await?;
    println!("{}", balance);
    Ok(())
}

// ── Account ─────────────────────────────────────────────────────

pub async fn cmd_account(rpc: &RpcClient, account: &str) -> Result<()> {
    let account_id = resolve_account_id(account)?;
    let info = rpc.get_account(&account_to_base58(&account_id)).await?;

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
    let to_arr = resolve_account_id(to)?;

    // Get next nonce (accounts for pending mempool transactions).
    let sender_hex = account_to_base58(&sender_id);
    let nonce = rpc.get_next_nonce(&sender_hex).await?;

    let mut op = UserOperation {
        sender: sender_id,
        nonce,
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
        println!("  From:   {} ({})", from, sender_hex);
        println!("  To:     {} ({})", to, account_to_base58(&to_arr));
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
    let sender_hex = account_to_base58(&sender_id);
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
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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
        println!("  Contract ID: {}", account_to_base58(&contract_id));
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
    let target_bytes = target_id.to_vec();
    let mut target_arr = [0u8; 32];
    target_arr.copy_from_slice(&target_bytes);

    let args = match args_hex {
        Some(hex) => hex_decode(hex)?,
        None => vec![],
    };

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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
        println!("  Target: {}", account_to_base58(&target_id));
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

    let mut key = wallet::generate_key(name)?;
    println!("Generated key '{}'", name);
    println!("  Account ID:  {}", key.account_id_hex);
    println!("  Public key:  {}", key.public_key_hex);

    if wallet::is_locked(&ks) {
        // Wallet is locked — encrypt the new key's seed before storing.
        let password = wallet::prompt_password("Wallet is locked. Enter password to add key: ")?;
        wallet::encrypt_new_key(&ks, &mut key, &password)?;
        println!("  Seed encrypted (wallet is locked)");
    } else {
        // Never print seeds to stdout — they persist in shell history and logs.
        // Seeds are stored in the keystore file (~/.solen/keys.json).
        println!("  Seed:        stored in keystore (use 'solen key lock' to encrypt)");
    }

    ks.keys.insert(name.to_string(), key);
    wallet::save_keystore(&ks)?;
    println!("\nSaved to ~/.solen/keys.json");

    Ok(())
}

pub fn cmd_key_import(name: &str, seed_hex: &str) -> Result<()> {
    let mut ks = wallet::load_keystore()?;

    let mut key = wallet::import_key(name, seed_hex)?;
    println!("Imported key '{}'", name);
    println!("  Account ID: {}", key.account_id_hex);
    println!("  Public key: {}", key.public_key_hex);

    if wallet::is_locked(&ks) {
        let password = wallet::prompt_password("Wallet is locked. Enter password to add key: ")?;
        wallet::encrypt_new_key(&ks, &mut key, &password)?;
        println!("  Seed encrypted (wallet is locked)");
    }

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

    if wallet::is_locked(&ks) {
        println!("Wallet status: LOCKED");
    } else {
        println!("Wallet status: unlocked");
    }
    println!();

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

// ── Wallet Lock / Unlock ───────────────────────────────────────

pub fn cmd_key_lock() -> Result<()> {
    let mut ks = wallet::load_keystore()?;

    if wallet::is_locked(&ks) {
        println!("Wallet is already locked.");
        return Ok(());
    }

    if ks.keys.is_empty() {
        println!("No keys to lock. Generate one first with: solen key generate <name>");
        return Ok(());
    }

    let password = wallet::prompt_new_password()?;
    wallet::lock_keystore(&mut ks, &password)?;
    wallet::save_keystore(&ks)?;

    println!("Wallet locked. {} key(s) encrypted.", ks.keys.len());
    println!("You will be prompted for your password when signing transactions.");

    Ok(())
}

pub fn cmd_key_unlock() -> Result<()> {
    let mut ks = wallet::load_keystore()?;

    if !wallet::is_locked(&ks) {
        println!("Wallet is not locked.");
        return Ok(());
    }

    let password = wallet::prompt_password("Enter wallet password: ")?;
    wallet::unlock_keystore(&mut ks, &password)?;
    wallet::save_keystore(&ks)?;

    println!("Wallet unlocked. Seeds are now stored in plaintext.");

    Ok(())
}

pub fn cmd_key_change_password() -> Result<()> {
    let mut ks = wallet::load_keystore()?;

    if !wallet::is_locked(&ks) {
        println!("Wallet is not locked. Use 'solen key lock' first.");
        return Ok(());
    }

    let old_password = wallet::prompt_password("Current password: ")?;
    let new_password = wallet::prompt_new_password()?;
    wallet::change_password(&mut ks, &old_password, &new_password)?;
    wallet::save_keystore(&ks)?;

    println!("Password changed successfully.");

    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────

/// Resolve an account identifier — a key name, hex ID, or Base58 address.
/// Resolve an input to a 32-byte account ID. Accepts:
/// - Base58 address
/// - Hex address (with optional 0x prefix)
/// - Key name from the local keystore
/// - Arbitrary name (hashed to deterministic ID)
fn resolve_account_id(input: &str) -> Result<[u8; 32]> {
    // Try parsing as an address (hex or Base58).
    if let Ok(id) = parse_address(input) {
        return Ok(id);
    }

    // Try loading from keystore.
    let ks = wallet::load_keystore()?;
    if let Some(key) = ks.keys.get(input) {
        return parse_address(&key.account_id_hex)
            .map_err(|e| anyhow::anyhow!("invalid stored account_id: {}", e));
    }

    // Treat as a name and convert to account ID.
    Ok(wallet::name_to_account_id(input))
}

fn sign_op(op: &mut UserOperation, kp: &Keypair, chain_id: u64) {
    op.signature = kp.sign(&op.signing_message(chain_id)).to_vec();
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

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Parse signer public keys (hex or Base58).
    let mut signers = Vec::new();
    for addr_str in signer_hexes {
        let key = parse_address(addr_str)
            .map_err(|e| anyhow::anyhow!("invalid signer address '{}': {}", addr_str, e))?;
        signers.push(key);
    }

    let auth_methods = vec![AuthMethod::Threshold {
        signers: signers.clone(),
        threshold,
    }];

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
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
            println!("  {}: {}", i + 1, account_to_base58(s));
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

pub async fn cmd_initiate_recovery(
    rpc: &RpcClient,
    from: &str,
    target: &str,
    new_public_key_hex: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let target_id = resolve_account_id(target)?;
    let target_bytes = target_id.to_vec();

    // Parse new public key.
    let new_pk_hex = new_public_key_hex.strip_prefix("0x").unwrap_or(new_public_key_hex);
    let new_pk_bytes = hex_decode(new_pk_hex)?;
    if new_pk_bytes.len() != 32 {
        anyhow::bail!("new public key must be 32 bytes (64 hex chars)");
    }
    let mut new_pk = [0u8; 32];
    new_pk.copy_from_slice(&new_pk_bytes);

    // Build new auth methods JSON: single Ed25519 key.
    let new_auth = vec![solen_types::account::AuthMethod::Ed25519 { public_key: new_pk }];
    let auth_json = serde_json::to_vec(&new_auth)?;

    // args: target[32] + auth_json[...]
    let mut args = Vec::new();
    args.extend_from_slice(&target_bytes);
    args.extend_from_slice(&auth_json);

    let guardian_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x08;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: guardian_addr,
            method: "initiate_recovery".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Recovery initiated successfully.");
        println!("  Target:     {}", account_to_base58(&target_id));
        println!("  New key:    {}", new_pk_hex);
        println!("  Timelock:   ~1 week (151,200 blocks)");
        println!("\nOther guardians must confirm with: confirm-recovery");
        println!("The account owner can cancel with: cancel-recovery");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_confirm_recovery(
    rpc: &RpcClient,
    from: &str,
    recovery_id: u64,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let guardian_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x08;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: guardian_addr,
            method: "confirm_recovery".to_string(),
            args: recovery_id.to_le_bytes().to_vec(),
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Recovery #{} confirmed.", recovery_id);
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_cancel_recovery(
    rpc: &RpcClient,
    from: &str,
    recovery_id: u64,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let guardian_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x08;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: guardian_addr,
            method: "cancel_recovery".to_string(),
            args: recovery_id.to_le_bytes().to_vec(),
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Recovery #{} cancelled.", recovery_id);
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_execute_recovery(
    rpc: &RpcClient,
    from: &str,
    recovery_id: u64,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let guardian_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x08;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: guardian_addr,
            method: "execute_recovery".to_string(),
            args: recovery_id.to_le_bytes().to_vec(),
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Recovery #{} executed! Account auth methods have been replaced.", recovery_id);
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_register_rollup(
    rpc: &RpcClient,
    from: &str,
    rollup_id: u64,
    name: &str,
    proof_type: &str,
    genesis_state_root_hex: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    // Build args: rollup_id[8] + name_len[4] + name + proof_type_len[4] + proof_type + sequencer[32] + genesis_state_root[32]
    let mut args = Vec::new();
    args.extend_from_slice(&rollup_id.to_le_bytes());

    let name_bytes = name.as_bytes();
    args.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
    args.extend_from_slice(name_bytes);

    let pt_bytes = proof_type.as_bytes();
    args.extend_from_slice(&(pt_bytes.len() as u32).to_le_bytes());
    args.extend_from_slice(pt_bytes);

    // Sequencer = sender
    args.extend_from_slice(&sender_id);

    // Genesis state root
    let root_hex = genesis_state_root_hex.strip_prefix("0x").unwrap_or(genesis_state_root_hex);
    let root_bytes = hex_decode(root_hex)?;
    if root_bytes.len() != 32 {
        anyhow::bail!("genesis state root must be 32 bytes (64 hex chars)");
    }
    args.extend_from_slice(&root_bytes);

    let bridge_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x03;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: bridge_addr,
            method: "register_rollup".to_string(),
            args,
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Rollup registered successfully.");
        println!("  Rollup ID:    {}", rollup_id);
        println!("  Name:         {}", name);
        println!("  Proof type:   {}", proof_type);
        println!("  Sequencer:    {}", sender_hex);
        println!("  Deposit:      10,000 SOLEN");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_register_paymaster(
    rpc: &RpcClient,
    from: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let paymaster_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x07;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: paymaster_addr,
            method: "register".to_string(),
            args: vec![],
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Paymaster registered successfully.");
        println!("  Contract: {}", sender_hex);
        println!("\nYour contract must implement a 'willSponsor' view method.");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }

    Ok(())
}

pub async fn cmd_unregister_paymaster(
    rpc: &RpcClient,
    from: &str,
    chain_id: u64,
) -> Result<()> {
    let ks = wallet::load_keystore()?;
    let (kp, sender_id) = wallet::load_keypair(&ks, from)?;

    let sender_hex = account_to_base58(&sender_id);
    let info = rpc.get_account(&sender_hex).await?;

    let paymaster_addr = {
        let mut t = [0xFFu8; 32];
        t[31] = 0x07;
        t
    };

    let mut op = UserOperation {
        sender: sender_id,
        nonce: rpc.get_next_nonce(&sender_hex).await.unwrap_or(info.nonce),
        actions: vec![Action::Call {
            target: paymaster_addr,
            method: "unregister".to_string(),
            args: vec![],
        }],
        max_fee: 100_000,
        signature: vec![],
    };
    sign_op(&mut op, &kp, chain_id);

    let op_json = serde_json::to_value(&op)?;
    let result = rpc.submit_operation(op_json).await?;
    if result.accepted {
        println!("Paymaster unregistered successfully.");
    } else {
        println!("Failed: {}", result.error.unwrap_or_default());
    }

    Ok(())
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
