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

            // Deduct from sender balance.
            let mut state = StateManager::new(store);
            match state.require_account(sender) {
                Ok(mut acct) => {
                    if acct.balance < amount {
                        return err("insufficient balance for registration");
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
                    if acct.balance < amount {
                        return err("insufficient balance for delegation");
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
                    // Data: validator[32] + amount[16 LE]
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
                Err(e) => Err(e.to_string()),
            }
        }
        "undelegate" => {
            // args: validator_id[32] + amount[16] + current_epoch[8]
            let validator = match read_account_id(args, 0) {
                Some(v) => v,
                None => return err("invalid args"),
            };
            let amount = match read_u128(args, 32) {
                Some(a) => a,
                None => return err("invalid args"),
            };
            let epoch = read_u64(args, 48).unwrap_or(0);

            match staking.undelegate(*sender, validator, amount, epoch) {
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
            let epoch = read_u64(args, 0).unwrap_or(0);
            let withdrawn = staking.withdraw_undelegated(*sender, epoch);

            if withdrawn > 0 {
                // Credit sender balance.
                let mut state = StateManager::new(store);
                if let Ok(mut acct) = state.require_account(sender) {
                    acct.balance = acct.balance.saturating_add(withdrawn);
                    if let Err(e) = state.save_account(&acct) {
                        return err(&format!("state save failed: {e}"));
                    }
                }
                drop(state);

                staking = StakingContract::load(store);

                events.push(Event {
                    emitter: STAKING_ADDRESS,
                    topic: b"withdraw".to_vec(),
                    data: withdrawn.to_le_bytes().to_vec(),
                });
            }
            Ok(())
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
    use solen_system_contracts::governance::{GovernanceContract, ProposalAction};

    let mut gov = GovernanceContract::load(store);
    let mut events = Vec::new();

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

            match gov.finalize(proposal_id, total_stake, epoch) {
                Ok(status) => {
                    let status_str = format!("{:?}", status);
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
        Err(e) => SystemCallResult { gas_used: SYSTEM_CALL_GAS, events, error: Some(e) },
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
                if acct.balance < amount {
                    return err("insufficient balance for deposit");
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
                Err(e) => Err(e.to_string()),
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
    _store: &mut dyn StateStore,
    _sender: &AccountId,
    method: &str,
    _args: &[u8],
) -> SystemCallResult {
    // Intent pool runs in-memory, not in state. Expose via RPC instead.
    err(&format!("intent operations use RPC, not system calls: {method}"))
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
