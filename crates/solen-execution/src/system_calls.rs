//! System contract call routing.
//!
//! When Action::Call targets a well-known system address, the call is
//! routed here instead of the WASM VM. Arguments are encoded as:
//! method_name + \0 + arg_bytes (same as WASM contracts).

use solen_storage::StateStore;
use solen_types::system::*;
use solen_types::AccountId;

use crate::receipt::Event;
use crate::state::StateManager;

/// Result of a system call.
pub struct SystemCallResult {
    pub gas_used: u64,
    pub events: Vec<Event>,
    pub error: Option<String>,
}

const SYSTEM_CALL_GAS: u64 = 200;

/// Execute a system contract call.
/// Minimum balance to retain after system call deductions (for gas fees).
const MIN_FEE_RESERVE: u128 = 10_000;

pub fn execute_system_call(
    store: &mut dyn StateStore,
    sender: &AccountId,
    target: &AccountId,
    method: &str,
    args: &[u8],
) -> SystemCallResult {
    if *target == STAKING_ADDRESS {
        execute_staking_call(store, sender, method, args)
    } else if *target == GOVERNANCE_ADDRESS {
        execute_governance_call(store, sender, method, args)
    } else if *target == BRIDGE_ADDRESS {
        execute_bridge_call(store, sender, method, args)
    } else if *target == TREASURY_ADDRESS {
        execute_treasury_call(store, sender, method)
    } else if *target == solen_types::system::VESTING_ADDRESS {
        execute_vesting_call(store, sender, method)
    } else if *target == INTENT_ADDRESS {
        execute_intent_call(store, sender, method, args)
    } else if *target == PAYMASTER_REGISTRY_ADDRESS {
        execute_paymaster_call(store, sender, method, args)
    } else if *target == GUARDIAN_ADDRESS {
        execute_guardian_call(store, sender, method, args)
    } else {
        SystemCallResult {
            gas_used: 0,
            events: vec![],
            error: Some("unknown system contract".into()),
        }
    }
}

fn read_account_id(args: &[u8], offset: usize) -> Option<AccountId> {
    if args.len() < offset + 32 {
        return None;
    }
    let mut id = [0u8; 32];
    id.copy_from_slice(&args[offset..offset + 32]);
    Some(id)
}

fn read_u128(args: &[u8], offset: usize) -> Option<u128> {
    if args.len() < offset + 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&args[offset..offset + 16]);
    Some(u128::from_le_bytes(buf))
}

fn read_u64(args: &[u8], offset: usize) -> Option<u64> {
    if args.len() < offset + 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&args[offset..offset + 8]);
    Some(u64::from_le_bytes(buf))
}

// ── Staking ─────────────────────────────────────────────────────

fn execute_staking_call(
    store: &mut dyn StateStore,
    sender: &AccountId,
    method: &str,
    args: &[u8],
) -> SystemCallResult {
    use solen_system_contracts::staking::StakingContract;

    let mut staking = StakingContract::load(store);
    let mut events = Vec::new();

    let result = match method {
        "register" => {
            // Register as a new validator with self-stake.
            // args: amount[16] (stake amount, must be >= MIN_VALIDATOR_STAKE)
            // The sender becomes the validator (validator ID = sender).
            let amount = match read_u128(args, 0) {
                Some(a) => a,
                None => return err("invalid args: need amount[16]"),
            };

            // Check governance-configured min stake (falls back to default).
            let min_stake = read_config_u128(store, b"__config_min_validator_stake__")
                .unwrap_or(solen_system_contracts::staking::MIN_VALIDATOR_STAKE);
            if amount < min_stake {
                return err(&format!(
                    "insufficient stake: need {} but got {}",
                    min_stake, amount
                ));
            }

            // Deduct from sender balance.
            let mut state = StateManager::new(store);
            match state.require_account(sender) {
                Ok(mut acct) => {
                    if acct.balance < amount + MIN_FEE_RESERVE {
                        return err("insufficient balance for registration (need fee reserve)");
                    }
                    acct.balance -= amount;
                    if let Err(e) = state.save_account(&acct) {
                        return err(&format!("state save failed: {e}"));
                    }
                }
                Err(e) => return err(&e.to_string()),
            }
            drop(state);

            staking = StakingContract::load(store);
            let current_epoch = read_current_epoch(store);
            match staking.register_validator_at_epoch(*sender, amount, current_epoch) {
                Ok(()) => {
                    let mut data = Vec::with_capacity(48);
                    data.extend_from_slice(sender);
                    data.extend_from_slice(&amount.to_le_bytes());
                    events.push(Event {
                        emitter: STAKING_ADDRESS,
                        topic: b"validator_registered".to_vec(),
                        data,
                    });
                    Ok(())
                }
                Err(e) => {
                    // Refund on failure.
                    let mut state = StateManager::new(store);
                    if let Ok(mut acct) = state.require_account(sender) {
                        acct.balance = acct.balance.saturating_add(amount);
                        let _ = state.save_account(&acct);
                    }
                    Err(e.to_string())
                }
            }
        }
        "delegate" => {
            // args: validator_id[32] + amount[16]
            let validator = match read_account_id(args, 0) {
                Some(v) => v,
                None => return err("invalid args: need validator[32] + amount[16]"),
            };
            let amount = match read_u128(args, 32) {
                Some(a) => a,
                None => return err("invalid args: need amount[16]"),
            };

            // Deduct from sender balance.
            let mut state = StateManager::new(store);
            match state.require_account(sender) {
                Ok(mut acct) => {
                    if acct.balance < amount + MIN_FEE_RESERVE {
                        return err("insufficient balance for delegation (need fee reserve)");
                    }
                    acct.balance -= amount;
                    if let Err(e) = state.save_account(&acct) {
                        return err(&format!("state save failed: {e}"));
                    }
                }
                Err(e) => return err(&e.to_string()),
            }
            drop(state);

            // Reload staking after state manager dropped.
            staking = StakingContract::load(store);

            // Read current epoch from chain metadata.
            let current_epoch = read_current_epoch(store);
            match staking.delegate_at_epoch(*sender, validator, amount, current_epoch) {
                Ok(()) => {
                    let mut data = Vec::with_capacity(48);
                    data.extend_from_slice(&validator);
                    data.extend_from_slice(&amount.to_le_bytes());
                    events.push(Event {
                        emitter: STAKING_ADDRESS,
                        topic: b"delegate".to_vec(),
                        data,
                    });
                    Ok(())
                }
                Err(e) => {
                    // Refund on delegation failure.
                    let mut state = StateManager::new(store);
                    if let Ok(mut acct) = state.require_account(sender) {
                        acct.balance = acct.balance.saturating_add(amount);
                        let _ = state.save_account(&acct);
                    }
                    Err(e.to_string())
                }
            }
        }
        "undelegate" => {
            // args: validator_id[32] + amount[16] (epoch read from chain meta)
            let validator = match read_account_id(args, 0) {
                Some(v) => v,
                None => return err("invalid args"),
            };
            let amount = match read_u128(args, 32) {
                Some(a) => a,
                None => return err("invalid args"),
            };
            let current_epoch = read_current_epoch(store);

            // Auto-withdraw any previously matured undelegations first.
            let auto_withdrawn = staking.withdraw_undelegated(*sender, current_epoch);
            if auto_withdrawn > 0 {
                // Save staking state first (undelegations removed).
                staking.save(store);

                let mut state = StateManager::new(store);
                if let Ok(mut acct) = state.require_account(sender) {
                    acct.balance = acct.balance.saturating_add(auto_withdrawn);
                    let _ = state.save_account(&acct);
                }
                drop(state);

                // Reload after state changes.
                staking = StakingContract::load(store);

                events.push(Event {
                    emitter: STAKING_ADDRESS,
                    topic: b"withdraw".to_vec(),
                    data: auto_withdrawn.to_le_bytes().to_vec(),
                });
            }

            match staking.undelegate(*sender, validator, amount, current_epoch) {
                Ok(()) => {
                    let mut data = Vec::with_capacity(48);
                    data.extend_from_slice(&validator);
                    data.extend_from_slice(&amount.to_le_bytes());
                    events.push(Event {
                        emitter: STAKING_ADDRESS,
                        topic: b"undelegate".to_vec(),
                        data,
                    });
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "withdraw" => {
            let current_epoch = read_current_epoch(store);
            let withdrawn = staking.withdraw_undelegated(*sender, current_epoch);

            if withdrawn > 0 {
                // Save staking state FIRST (with undelegations removed),
                // then credit sender balance.
                staking.save(store);

                let mut state = StateManager::new(store);
                if let Ok(mut acct) = state.require_account(sender) {
                    acct.balance = acct.balance.saturating_add(withdrawn);
                    if let Err(e) = state.save_account(&acct) {
                        return err(&format!("state save failed: {e}"));
                    }
                }
                drop(state);

                // Reload after state changes.
                staking = StakingContract::load(store);

                events.push(Event {
                    emitter: STAKING_ADDRESS,
                    topic: b"withdraw".to_vec(),
                    data: withdrawn.to_le_bytes().to_vec(),
                });
            }
            Ok(())
        }
        "slash" => {
            // System-authorized slashing. Args: offender[32] + penalty_bps[8]
            // Only accepted from [0xFF]-signed system operations (block proposer).
            let offender = match read_account_id(args, 0) {
                Some(id) => id,
                None => return err("invalid args: need offender[32] + penalty_bps[8]"),
            };
            let penalty_bps = read_u64(args, 32).unwrap_or(100); // default 1%

            if let Some(val) = staking.validators.iter_mut().find(|v| v.id == offender) {
                let penalty = val.self_stake.saturating_mul(penalty_bps as u128) / 10_000;
                val.self_stake = val.self_stake.saturating_sub(penalty);
                val.is_active = false;

                let mut data = Vec::with_capacity(48);
                data.extend_from_slice(&offender);
                data.extend_from_slice(&penalty.to_le_bytes());
                events.push(Event {
                    emitter: STAKING_ADDRESS,
                    topic: b"slashed".to_vec(),
                    data,
                });

                Ok(())
            } else {
                Err("validator not found".to_string())
            }
        }
        "unjail" => {
            // No args — sender is the validator requesting reactivation.
            match staking.unjail(sender) {
                Ok(()) => {
                    events.push(Event {
                        emitter: STAKING_ADDRESS,
                        topic: b"unjailed".to_vec(),
                        data: sender.to_vec(),
                    });
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "rotate_key" => {
            // args: new_key[32]
            let new_key = match read_account_id(args, 0) {
                Some(k) => k,
                None => return err("invalid args: need new_key[32]"),
            };
            let current_epoch = read_current_epoch(store);

            match staking.rotate_key(sender, new_key, current_epoch) {
                Ok(()) => {
                    let mut data = Vec::with_capacity(64);
                    data.extend_from_slice(sender);
                    data.extend_from_slice(&new_key);
                    events.push(Event {
                        emitter: STAKING_ADDRESS,
                        topic: b"key_rotation_requested".to_vec(),
                        data,
                    });
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        _ => Err(format!("unknown staking method: {method}")),
    };

    staking.save(store);

    match result {
        Ok(()) => SystemCallResult {
            gas_used: SYSTEM_CALL_GAS,
            events,
            error: None,
        },
        Err(e) => SystemCallResult {
            gas_used: SYSTEM_CALL_GAS,
            events,
            error: Some(e),
        },
    }
}

// ── Governance ──────────────────────────────────────────────────

fn execute_governance_call(
    store: &mut dyn StateStore,
    sender: &AccountId,
    method: &str,
    args: &[u8],
) -> SystemCallResult {
    use solen_system_contracts::governance::{GovernanceContract, ProposalAction, PROPOSAL_DEPOSIT};

    let mut gov = GovernanceContract::load(store);
    let mut events = Vec::new();

    // Deduct proposal deposit for proposal methods.
    let is_proposal = method.starts_with("propose_");
    if is_proposal {
        let mut state = StateManager::new(store);
        match state.require_account(sender) {
            Ok(mut acct) => {
                if acct.balance < PROPOSAL_DEPOSIT + MIN_FEE_RESERVE {
                    return err(&format!(
                        "insufficient balance for proposal deposit: need {} (1,000 SOLEN + fee reserve)",
                        PROPOSAL_DEPOSIT + MIN_FEE_RESERVE
                    ));
                }
                acct.balance -= PROPOSAL_DEPOSIT;
                if let Err(e) = state.save_account(&acct) {
                    return err(&format!("state save failed: {e}"));
                }
            }
            Err(e) => return err(&e.to_string()),
        }
        drop(state);
        gov = GovernanceContract::load(store);
    }

    let result = match method {
        "propose_set_base_fee" => {
            let new_fee = match read_u128(args, 0) {
                Some(f) => f,
                None => return err("invalid args: need new_fee[16]"),
            };
            let desc = String::from_utf8_lossy(&args[16..]).to_string();
            let epoch = read_current_epoch(store);
            let id = gov.create_proposal(
                *sender,
                ProposalAction::SetBaseFee { new_fee },
                desc,
                epoch,
            );
            events.push(Event {
                emitter: GOVERNANCE_ADDRESS,
                topic: b"proposal_created".to_vec(),
                data: id.to_le_bytes().to_vec(),
            });
            Ok(())
        }
        "propose_set_block_time" => {
            let new_block_time = match read_u64(args, 0) {
                Some(t) => t,
                None => return err("invalid args: need new_block_time_ms[8]"),
            };
            let desc = String::from_utf8_lossy(&args[8..]).to_string();
            let epoch = read_current_epoch(store);
            let id = gov.create_proposal(
                *sender,
                ProposalAction::SetBlockTime { new_block_time_ms: new_block_time },
                desc,
                epoch,
            );
            events.push(Event {
                emitter: GOVERNANCE_ADDRESS,
                topic: b"proposal_created".to_vec(),
                data: id.to_le_bytes().to_vec(),
            });
            Ok(())
        }
        "propose_set_burn_rate" => {
            let new_burn_rate_bps = match read_u64(args, 0) {
                Some(r) => r,
                None => return err("invalid args: need new_burn_rate_bps[8]"),
            };
            if new_burn_rate_bps > 10_000 {
                return err("burn rate cannot exceed 10000 bps (100%)");
            }
            let desc = String::from_utf8_lossy(&args[8..]).to_string();
            let epoch = read_current_epoch(store);
            let id = gov.create_proposal(
                *sender,
                ProposalAction::SetBurnRate { new_burn_rate_bps },
                desc,
                epoch,
            );
            events.push(Event {
                emitter: GOVERNANCE_ADDRESS,
                topic: b"proposal_created".to_vec(),
                data: id.to_le_bytes().to_vec(),
            });
            Ok(())
        }
        "propose_set_epoch_reward" => {
            let new_reward = match read_u128(args, 0) {
                Some(r) => r,
                None => return err("invalid args: need new_reward[16]"),
            };
            let desc = String::from_utf8_lossy(&args[16..]).to_string();
            let epoch = read_current_epoch(store);
            let id = gov.create_proposal(
                *sender,
                ProposalAction::SetEpochReward { new_reward },
                desc,
                epoch,
            );
            events.push(Event {
                emitter: GOVERNANCE_ADDRESS,
                topic: b"proposal_created".to_vec(),
                data: id.to_le_bytes().to_vec(),
            });
            Ok(())
        }
        "propose_set_min_validator_stake" => {
            let new_min_stake = match read_u128(args, 0) {
                Some(s) => s,
                None => return err("invalid args: need new_min_stake[16]"),
            };
            let desc = String::from_utf8_lossy(&args[16..]).to_string();
            let epoch = read_current_epoch(store);
            let id = gov.create_proposal(
                *sender,
                ProposalAction::SetMinValidatorStake { new_min_stake },
                desc,
                epoch,
            );
            events.push(Event {
                emitter: GOVERNANCE_ADDRESS,
                topic: b"proposal_created".to_vec(),
                data: id.to_le_bytes().to_vec(),
            });
            Ok(())
        }
        "vote" => {
            // args: proposal_id[8] + support[1] + stake_weight[16]
            let proposal_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return err("invalid args"),
            };
            let support = args.get(8).map(|&b| b != 0).unwrap_or(false);
            let weight = read_u128(args, 9).unwrap_or(1);
            let epoch = read_current_epoch(store);

            match gov.vote(proposal_id, *sender, support, weight, epoch) {
                Ok(()) => {
                    events.push(Event {
                        emitter: GOVERNANCE_ADDRESS,
                        topic: b"voted".to_vec(),
                        data: proposal_id.to_le_bytes().to_vec(),
                    });
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "finalize" => {
            let proposal_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return err("invalid args: need proposal_id[8]"),
            };
            let epoch = read_current_epoch(store);

            // Read total stake from staking contract for quorum calculation.
            let staking = solen_system_contracts::staking::StakingContract::load(store);
            let total_stake: u128 = staking.validators.iter()
                .filter(|v| v.is_active)
                .map(|v| v.total_stake())
                .sum();

            // Get proposer and deposit before finalizing.
            let (proposer, deposit) = gov.get_proposal(proposal_id)
                .map(|p| (p.proposer, p.deposit))
                .unwrap_or(([0u8; 32], 0));

            match gov.finalize(proposal_id, total_stake, epoch) {
                Ok(status) => {
                    let status_str = format!("{:?}", status);

                    // Save finalized status BEFORE deposit handling
                    // (deposit handling needs StateManager which conflicts with gov).
                    gov.save(store);

                    // Handle deposit: return to proposer if passed, send to treasury if rejected.
                    if deposit > 0 {
                        use solen_system_contracts::governance::ProposalStatus;
                        let recipient = match status {
                            ProposalStatus::Passed => proposer,
                            _ => solen_types::system::TREASURY_ADDRESS,
                        };
                        let mut state = StateManager::new(store);
                        if let Ok(mut acct) = state.require_account(&recipient) {
                            acct.balance = acct.balance.saturating_add(deposit);
                            let _ = state.save_account(&acct);
                        }
                    }
                    // Reload after deposit handling.
                    gov = GovernanceContract::load(store);

                    events.push(Event {
                        emitter: GOVERNANCE_ADDRESS,
                        topic: b"proposal_finalized".to_vec(),
                        data: {
                            let mut d = proposal_id.to_le_bytes().to_vec();
                            d.extend_from_slice(status_str.as_bytes());
                            d
                        },
                    });
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "execute" => {
            let proposal_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return err("invalid args: need proposal_id[8]"),
            };
            let epoch = read_current_epoch(store);

            match gov.execute(proposal_id, epoch) {
                Ok(action) => {
                    // Apply the action to chain state.
                    let action_desc = match &action {
                        ProposalAction::SetBaseFee { new_fee } => {
                            // Store new base fee in chain config.
                            let _ = store.put(b"__config_base_fee__", &new_fee.to_le_bytes());
                            format!("base_fee={}", new_fee)
                        }
                        ProposalAction::SetBlockTime { new_block_time_ms } => {
                            let _ = store.put(b"__config_block_time__", &new_block_time_ms.to_le_bytes());
                            format!("block_time={}ms", new_block_time_ms)
                        }
                        ProposalAction::SetBurnRate { new_burn_rate_bps } => {
                            let _ = store.put(b"__config_burn_rate__", &new_burn_rate_bps.to_le_bytes());
                            format!("burn_rate={}bps", new_burn_rate_bps)
                        }
                        ProposalAction::SetEpochReward { new_reward } => {
                            let _ = store.put(b"__config_epoch_reward__", &new_reward.to_le_bytes());
                            format!("epoch_reward={}", new_reward)
                        }
                        ProposalAction::SetMinValidatorStake { new_min_stake } => {
                            let _ = store.put(b"__config_min_validator_stake__", &new_min_stake.to_le_bytes());
                            format!("min_validator_stake={}", new_min_stake)
                        }
                        ProposalAction::EmergencyPause => "paused".to_string(),
                        ProposalAction::EmergencyResume => "resumed".to_string(),
                        _ => format!("{:?}", action),
                    };
                    events.push(Event {
                        emitter: GOVERNANCE_ADDRESS,
                        topic: b"proposal_executed".to_vec(),
                        data: {
                            let mut d = proposal_id.to_le_bytes().to_vec();
                            d.extend_from_slice(action_desc.as_bytes());
                            d
                        },
                    });
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        _ => Err(format!("unknown governance method: {method}")),
    };

    gov.save(store);

    match result {
        Ok(()) => SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None },
        Err(e) => {
            // Refund proposal deposit on failure.
            if is_proposal {
                let mut state = StateManager::new(store);
                if let Ok(mut acct) = state.require_account(sender) {
                    acct.balance = acct.balance.saturating_add(PROPOSAL_DEPOSIT);
                    let _ = state.save_account(&acct);
                }
            }
            SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: Some(e) }
        }
    }
}

// ── Bridge ──────────────────────────────────────────────────────

fn execute_bridge_call(
    store: &mut dyn StateStore,
    sender: &AccountId,
    method: &str,
    args: &[u8],
) -> SystemCallResult {
    use solen_system_contracts::bridge::BridgeContract;

    let mut bridge = BridgeContract::load(store);
    let mut events = Vec::new();

    let result = match method {
        "deposit" => {
            // args: rollup_id[8] + amount[16]
            let rollup_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return err("invalid args: need rollup_id[8] + amount[16]"),
            };
            let amount = match read_u128(args, 8) {
                Some(a) => a,
                None => return err("invalid args"),
            };

            // Deduct from sender.
            let mut state = StateManager::new(store);
            if let Ok(mut acct) = state.require_account(sender) {
                if acct.balance < amount + MIN_FEE_RESERVE {
                    return err("insufficient balance for deposit (need fee reserve)");
                }
                acct.balance -= amount;
                let _ = state.save_account(&acct);
            }
            drop(state);

            bridge = BridgeContract::load(store);

            match bridge.deposit(rollup_id, amount) {
                Ok(()) => {
                    events.push(Event {
                        emitter: BRIDGE_ADDRESS,
                        topic: b"deposit".to_vec(),
                        data: amount.to_le_bytes().to_vec(),
                    });
                    Ok(())
                }
                Err(e) => {
                    // Refund on deposit failure.
                    let mut state = StateManager::new(store);
                    if let Ok(mut acct) = state.require_account(sender) {
                        acct.balance = acct.balance.saturating_add(amount);
                        let _ = state.save_account(&acct);
                    }
                    Err(e.to_string())
                }
            }
        }
        "register_vault" => {
            let rollup_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return err("invalid args"),
            };
            match bridge.register_vault(rollup_id) {
                Ok(()) => {
                    events.push(Event {
                        emitter: BRIDGE_ADDRESS,
                        topic: b"vault_registered".to_vec(),
                        data: rollup_id.to_le_bytes().to_vec(),
                    });
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "register_rollup" => {
            // args: rollup_id[8] + name_len[4] + name[...] + proof_type_len[4] + proof_type[...] + sequencer[32] + genesis_state_root[32]
            let rollup_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return err("invalid args: need rollup_id[8]"),
            };
            // Parse name (length-prefixed)
            if args.len() < 12 {
                return err("invalid args: too short");
            }
            let name_len = u32::from_le_bytes([args[8], args[9], args[10], args[11]]) as usize;
            let name_end = 12 + name_len;
            if args.len() < name_end + 4 {
                return err("invalid args: name too long");
            }
            let name = String::from_utf8_lossy(&args[12..name_end]).to_string();

            // Parse proof_type (length-prefixed)
            let pt_len = u32::from_le_bytes([args[name_end], args[name_end+1], args[name_end+2], args[name_end+3]]) as usize;
            let pt_end = name_end + 4 + pt_len;
            if args.len() < pt_end + 64 {
                return err("invalid args: need sequencer[32] + genesis_state_root[32]");
            }
            let proof_type = String::from_utf8_lossy(&args[name_end+4..pt_end]).to_string();

            // Block mock proofs on mainnet — only allow on devnet/testnet.
            if proof_type == "mock" {
                let chain_id = match store.get(b"__chain_id__") {
                    Ok(Some(data)) if data.len() >= 8 => {
                        u64::from_le_bytes(data[..8].try_into().unwrap_or([0; 8]))
                    }
                    _ => 0,
                };
                // Mainnet chain_id = 1.
                if chain_id == 1 {
                    return err("mock proof type is not allowed on mainnet");
                }
            }

            // Parse sequencer and genesis_state_root
            let sequencer = match read_account_id(args, pt_end) {
                Some(id) => id,
                None => return err("invalid args: bad sequencer"),
            };
            let mut genesis_state_root = [0u8; 32];
            genesis_state_root.copy_from_slice(&args[pt_end+32..pt_end+64]);

            // Require a registration deposit (10,000 SOLEN).
            let deposit: u128 = 10_000 * 100_000_000;
            let mut state = StateManager::new(store);
            if let Ok(mut acct) = state.require_account(sender) {
                if acct.balance < deposit + MIN_FEE_RESERVE {
                    return err("insufficient balance for rollup registration deposit");
                }
                acct.balance -= deposit;
                let _ = state.save_account(&acct);

                // Credit deposit to bridge address.
                if let Ok(mut bridge_acct) = state.require_account(&BRIDGE_ADDRESS) {
                    bridge_acct.balance += deposit;
                    let _ = state.save_account(&bridge_acct);
                }
            } else {
                return err("sender account not found");
            }
            drop(state);

            // Reload bridge after state changes.
            bridge = solen_system_contracts::bridge::BridgeContract::load(store);

            // Read current height from chain meta.
            let height = match store.get(b"__chain_meta__") {
                Ok(Some(data)) if data.len() >= 8 => {
                    let mut h = [0u8; 8];
                    h.copy_from_slice(&data[..8]);
                    u64::from_le_bytes(h)
                }
                _ => 0,
            };

            match bridge.register_rollup(rollup_id, name, proof_type, sequencer, genesis_state_root, height) {
                Ok(()) => {
                    // Store rollup registration info in a well-known state key
                    // so the RPC can find it without the bridge contract.
                    let reg_key = format!("__rollup_{}__", rollup_id);
                    let reg_data = serde_json::json!({
                        "rollup_id": rollup_id,
                        "proof_type": bridge.get_rollup(rollup_id).map(|r| r.proof_type.clone()).unwrap_or_default(),
                        "genesis_state_root": hex_encode(&genesis_state_root),
                        "sequencer": hex_encode(&sequencer),
                    });
                    if let Ok(data) = serde_json::to_vec(&reg_data) {
                        let _ = store.put(reg_key.as_bytes(), &data);
                    }

                    events.push(Event {
                        emitter: BRIDGE_ADDRESS,
                        topic: b"rollup_registered".to_vec(),
                        data: rollup_id.to_le_bytes().to_vec(),
                    });
                    Ok(())
                }
                Err(e) => {
                    // Refund deposit on registration failure.
                    let mut state = StateManager::new(store);
                    if let Ok(mut acct) = state.require_account(sender) {
                        acct.balance = acct.balance.saturating_add(deposit);
                        let _ = state.save_account(&acct);
                    }
                    if let Ok(mut bridge_acct) = state.require_account(&BRIDGE_ADDRESS) {
                        bridge_acct.balance = bridge_acct.balance.saturating_sub(deposit);
                        let _ = state.save_account(&bridge_acct);
                    }
                    Err(e.to_string())
                }
            }
        }
        _ => Err(format!("unknown bridge method: {method}")),
    };

    bridge.save(store);

    match result {
        Ok(()) => SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None },
        Err(e) => SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: Some(e) },
    }
}

// ── Treasury ────────────────────────────────────────────────────

fn execute_treasury_call(
    store: &mut dyn StateStore,
    _sender: &AccountId,
    method: &str,
) -> SystemCallResult {
    use solen_system_contracts::treasury::TreasuryContract;

    let treasury = TreasuryContract::load(store);

    match method {
        "status" => {
            let events = vec![Event {
                emitter: TREASURY_ADDRESS,
                topic: b"treasury_status".to_vec(),
                data: format!(
                    "balance={},collected={},burned={}",
                    treasury.balance, treasury.total_fees_collected, treasury.total_burned
                )
                .into_bytes(),
            }];
            SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None }
        }
        _ => err(&format!("unknown treasury method: {method}")),
    }
}

// ── Intents ─────────────────────────────────────────────────────

fn execute_intent_call(
    store: &mut dyn StateStore,
    sender: &AccountId,
    method: &str,
    args: &[u8],
) -> SystemCallResult {
    let mut events = Vec::new();

    match method {
        "fulfill" => {
            // args: intent_id[8] + solver[32] + claimed_tip[16] + num_transfers[4] + (to[32]+amount[16])*N
            if args.len() < 60 {
                return err("invalid args: too short");
            }

            let intent_id = read_u64(args, 0).unwrap_or(0);
            let solver = match read_account_id(args, 8) {
                Some(s) => s,
                None => return err("invalid args: bad solver"),
            };
            let claimed_tip = read_u128(args, 40).unwrap_or(0);
            let num_transfers = u32::from_le_bytes([args[56], args[57], args[58], args[59]]) as usize;

            let mut offset = 60;
            let mut state = StateManager::new(store);

            // Execute each transfer.
            for _ in 0..num_transfers {
                if offset + 48 > args.len() {
                    return err("invalid args: transfer data truncated");
                }
                let to = match read_account_id(args, offset) {
                    Some(t) => t,
                    None => return err("invalid args: bad transfer recipient"),
                };
                let amount = read_u128(args, offset + 32).unwrap_or(0);
                offset += 48;

                // Debit sender.
                match state.require_account(sender) {
                    Ok(mut sender_acct) => {
                        if sender_acct.balance < amount {
                            return SystemCallResult {
                                gas_used: SYSTEM_CALL_GAS,
                                events,
                                error: Some("insufficient balance for intent transfer".to_string()),
                            };
                        }
                        sender_acct.balance -= amount;
                        let _ = state.save_account(&sender_acct);
                    }
                    Err(e) => return SystemCallResult {
                        gas_used: SYSTEM_CALL_GAS,
                        events,
                        error: Some(e.to_string()),
                    },
                }

                // Credit recipient (create if needed).
                let mut recipient = state.get_account(&to)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| solen_types::account::Account {
                        id: to,
                        code_hash: [0u8; 32],
                        auth_methods: vec![],
                        nonce: 0,
                        balance: 0,
                    });
                recipient.balance += amount;
                let _ = state.save_account(&recipient);

                // Emit transfer event.
                let mut data = Vec::with_capacity(48);
                data.extend_from_slice(&to);
                data.extend_from_slice(&amount.to_le_bytes());
                events.push(Event {
                    emitter: *sender,
                    topic: b"transfer".to_vec(),
                    data,
                });
            }

            // Pay solver tip.
            if claimed_tip > 0 {
                match state.require_account(sender) {
                    Ok(mut sender_acct) => {
                        if sender_acct.balance >= claimed_tip {
                            sender_acct.balance -= claimed_tip;
                            let _ = state.save_account(&sender_acct);

                            let mut solver_acct = state.get_account(&solver)
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| solen_types::account::Account {
                                    id: solver,
                                    code_hash: [0u8; 32],
                                    auth_methods: vec![],
                                    nonce: 0,
                                    balance: 0,
                                });
                            solver_acct.balance += claimed_tip;
                            let _ = state.save_account(&solver_acct);

                            let mut tip_data = Vec::with_capacity(48);
                            tip_data.extend_from_slice(&solver);
                            tip_data.extend_from_slice(&claimed_tip.to_le_bytes());
                            events.push(Event {
                                emitter: *sender,
                                topic: b"solver_tip".to_vec(),
                                data: tip_data,
                            });
                        }
                    }
                    Err(_) => {} // sender depleted, skip tip
                }
            }

            events.push(Event {
                emitter: *sender,
                topic: b"intent_fulfilled".to_vec(),
                data: intent_id.to_le_bytes().to_vec(),
            });

            SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None }
        }
        _ => err(&format!("unknown intent method: {method}")),
    }
}

// ── Paymaster Registry ────────────────────────────────────────

fn execute_paymaster_call(
    store: &mut dyn StateStore,
    sender: &AccountId,
    method: &str,
    _args: &[u8],
) -> SystemCallResult {
    let mut events = Vec::new();

    match method {
        "register" => {
            // Register the sender's contract as a paymaster.
            // The contract must implement a `willSponsor` view method.
            // args: (none — sender registers themselves)
            //
            // Verify sender has contract code deployed.
            let state = StateManager::new(store);
            match state.get_account(sender) {
                Ok(Some(acct)) if acct.code_hash != [0u8; 32] => {}
                _ => return err("only contracts can register as paymasters"),
            }
            drop(state);

            // Load existing paymasters list.
            let paymasters_key = b"__paymasters__";
            let mut paymasters: Vec<AccountId> = match store.get(paymasters_key) {
                Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or_default(),
                _ => vec![],
            };

            // Check not already registered.
            if paymasters.contains(sender) {
                return err("already registered as paymaster");
            }

            paymasters.push(*sender);

            if let Ok(data) = serde_json::to_vec(&paymasters) {
                let _ = store.put(paymasters_key, &data);
            }

            events.push(Event {
                emitter: PAYMASTER_REGISTRY_ADDRESS,
                topic: b"paymaster_registered".to_vec(),
                data: sender.to_vec(),
            });

            SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None }
        }
        "unregister" => {
            let paymasters_key = b"__paymasters__";
            let mut paymasters: Vec<AccountId> = match store.get(paymasters_key) {
                Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or_default(),
                _ => vec![],
            };

            paymasters.retain(|p| p != sender);

            if let Ok(data) = serde_json::to_vec(&paymasters) {
                let _ = store.put(paymasters_key, &data);
            }

            events.push(Event {
                emitter: PAYMASTER_REGISTRY_ADDRESS,
                topic: b"paymaster_unregistered".to_vec(),
                data: sender.to_vec(),
            });

            SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None }
        }
        "list" => {
            let paymasters_key = b"__paymasters__";
            let paymasters: Vec<AccountId> = match store.get(paymasters_key) {
                Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or_default(),
                _ => vec![],
            };

            events.push(Event {
                emitter: PAYMASTER_REGISTRY_ADDRESS,
                topic: b"paymaster_list".to_vec(),
                data: format!("{}", paymasters.len()).into_bytes(),
            });

            SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None }
        }
        _ => err(&format!("unknown paymaster method: {method}")),
    }
}

// ── Guardian Recovery ─────────────────────────────────────────

fn execute_guardian_call(
    store: &mut dyn StateStore,
    sender: &AccountId,
    method: &str,
    args: &[u8],
) -> SystemCallResult {
    use solen_system_contracts::guardian::GuardianContract;
    use solen_types::account::AuthMethod;

    let mut guardian = GuardianContract::load(store);
    let mut events = Vec::new();

    // Read current height from chain meta.
    let current_height = match store.get(b"__chain_meta__") {
        Ok(Some(data)) if data.len() >= 8 => {
            let mut h = [0u8; 8];
            h.copy_from_slice(&data[..8]);
            u64::from_le_bytes(h)
        }
        _ => 0,
    };

    let result = match method {
        "initiate_recovery" => {
            // args: target_account[32] + new_auth_methods_json[...]
            let target = match read_account_id(args, 0) {
                Some(id) => id,
                None => return err("invalid args: need target_account[32] + new_auth_methods_json"),
            };

            // Parse new auth methods from JSON.
            let json_bytes = &args[32..];
            let new_auth: Vec<AuthMethod> = match serde_json::from_slice(json_bytes) {
                Ok(a) => a,
                Err(e) => return err(&format!("invalid new_auth_methods JSON: {e}")),
            };

            // Load target account to get its guardian list.
            let state = StateManager::new(store);
            let target_acct = match state.get_account(&target) {
                Ok(Some(a)) => a,
                _ => return err("target account not found"),
            };
            drop(state);

            let guardian_ids: Vec<AccountId> = target_acct.auth_methods.iter()
                .filter_map(|m| {
                    if let AuthMethod::Guardian { guardian_id } = m {
                        Some(*guardian_id)
                    } else {
                        None
                    }
                })
                .collect();

            if guardian_ids.is_empty() {
                return err("target account has no guardians configured");
            }

            match guardian.initiate_recovery(target, *sender, new_auth, &guardian_ids, current_height) {
                Ok(id) => {
                    events.push(Event {
                        emitter: GUARDIAN_ADDRESS,
                        topic: b"recovery_initiated".to_vec(),
                        data: {
                            let mut d = target.to_vec();
                            d.extend_from_slice(&id.to_le_bytes());
                            d
                        },
                    });
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        "confirm_recovery" => {
            // args: recovery_id[8]
            let recovery_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return err("invalid args: need recovery_id[8]"),
            };

            // Guardian IDs are stored in the recovery request (captured at
            // initiation time). No need to re-read from the account.
            match guardian.confirm_recovery(recovery_id, *sender) {
                Ok(()) => {
                    events.push(Event {
                        emitter: GUARDIAN_ADDRESS,
                        topic: b"recovery_confirmed".to_vec(),
                        data: {
                            let mut d = sender.to_vec();
                            d.extend_from_slice(&recovery_id.to_le_bytes());
                            d
                        },
                    });
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        "cancel_recovery" => {
            // args: recovery_id[8]
            let recovery_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return err("invalid args: need recovery_id[8]"),
            };

            match guardian.cancel_recovery(recovery_id, sender) {
                Ok(()) => {
                    events.push(Event {
                        emitter: GUARDIAN_ADDRESS,
                        topic: b"recovery_cancelled".to_vec(),
                        data: recovery_id.to_le_bytes().to_vec(),
                    });
                    Ok(())
                }
                Err(e) => Err(e),
            }
        }
        "execute_recovery" => {
            // args: recovery_id[8]
            let recovery_id = match read_u64(args, 0) {
                Some(id) => id,
                None => return err("invalid args: need recovery_id[8]"),
            };

            // Check timelock and confirmations.
            if let Err(e) = guardian.can_execute(recovery_id, current_height) {
                return err(&e);
            }

            // Execute: update the target account's auth methods.
            let req = match guardian.mark_executed(recovery_id) {
                Ok(r) => r,
                Err(e) => return err(&e),
            };

            // Save guardian state BEFORE modifying accounts (same pattern as governance).
            guardian.save(store);

            let mut state = StateManager::new(store);
            match state.get_account(&req.target_account) {
                Ok(Some(mut acct)) => {
                    acct.auth_methods = req.new_auth_methods.clone();
                    if let Err(e) = state.save_account(&acct) {
                        return err(&format!("failed to update account: {e}"));
                    }
                }
                _ => return err("target account not found"),
            }

            events.push(Event {
                emitter: GUARDIAN_ADDRESS,
                topic: b"recovery_executed".to_vec(),
                data: {
                    let mut d = req.target_account.to_vec();
                    d.extend_from_slice(&recovery_id.to_le_bytes());
                    d
                },
            });

            // Already saved above, return early.
            return SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None };
        }
        _ => Err(format!("unknown guardian method: {method}")),
    };

    guardian.save(store);

    match result {
        Ok(()) => SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None },
        Err(e) => SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: Some(e) },
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ── Vesting ─────────────────────────────────────────────────────

fn execute_vesting_call(
    store: &mut dyn StateStore,
    sender: &AccountId,
    method: &str,
) -> SystemCallResult {
    use solen_system_contracts::vesting::VestingContract;

    let mut vesting = VestingContract::load(store);
    let mut events = Vec::new();

    let result = match method {
        "claim" => {
            let current_epoch = read_current_epoch(store);
            match vesting.claim(sender, current_epoch) {
                Ok(amount) => {
                    // Credit sender's account with claimed tokens.
                    let mut state = StateManager::new(store);
                    if let Ok(mut acct) = state.require_account(sender) {
                        acct.balance = acct.balance.saturating_add(amount);
                        if let Err(e) = state.save_account(&acct) {
                        return err(&format!("state save failed: {e}"));
                    }
                    }
                    drop(state);

                    // Reload vesting after state manager dropped.
                    vesting = VestingContract::load(store);

                    let mut data = Vec::with_capacity(48);
                    data.extend_from_slice(sender);
                    data.extend_from_slice(&amount.to_le_bytes());
                    events.push(Event {
                        emitter: solen_types::system::VESTING_ADDRESS,
                        topic: b"vesting_claim".to_vec(),
                        data,
                    });
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        }
        "status" => {
            match vesting.get_schedule(sender) {
                Some(schedule) => {
                    let current_epoch = read_current_epoch(store);
                    let vested = schedule.vested_at(current_epoch);
                    let claimable = schedule.claimable_at(current_epoch);
                    let data = format!(
                        "total={},vested={},claimed={},claimable={},type={:?}",
                        schedule.total_amount,
                        vested,
                        schedule.claimed,
                        claimable,
                        schedule.vesting_type,
                    );
                    events.push(Event {
                        emitter: solen_types::system::VESTING_ADDRESS,
                        topic: b"vesting_status".to_vec(),
                        data: data.into_bytes(),
                    });
                    Ok(())
                }
                None => Err("no vesting schedule for this account".into()),
            }
        }
        _ => Err(format!("unknown vesting method: {method}")),
    };

    vesting.save(store);

    match result {
        Ok(()) => SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: None },
        Err(e) => SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: Some(e) },
    }
}

fn err(msg: &str) -> SystemCallResult {
    SystemCallResult {
        gas_used: SYSTEM_CALL_GAS,
        events: vec![],
        error: Some(msg.to_string()),
    }
}

/// Read the current epoch from chain metadata.
fn read_current_epoch(store: &dyn StateStore) -> u64 {
    match store.get(b"__chain_meta__") {
        Ok(Some(data)) if data.len() >= 16 => {
            let mut h = [0u8; 8];
            h.copy_from_slice(&data[..8]);
            let height = u64::from_le_bytes(h);
            height / 100 // epoch length = 100 blocks
        }
        _ => 0,
    }
}

fn read_config_u128(store: &dyn StateStore, key: &[u8]) -> Option<u128> {
    match store.get(key) {
        Ok(Some(data)) if data.len() >= 16 => {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&data[..16]);
            Some(u128::from_le_bytes(buf))
        }
        _ => None,
    }
}
