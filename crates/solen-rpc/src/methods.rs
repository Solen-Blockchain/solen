//! RPC method implementations using jsonrpsee proc macros.

use std::sync::Arc;

use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use serde::{Deserialize, Serialize};
use solen_consensus::engine::ConsensusEngine;
use solen_execution::executor::BlockExecutor;
use solen_execution::state::ReadonlyStateManager;
use solen_types::transaction::UserOperation;

/// Account info returned by the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub id: String,
    pub balance: String,
    pub nonce: u64,
    pub code_hash: String,
}

/// Block info returned by the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockInfo {
    pub height: u64,
    pub epoch: u64,
    pub parent_hash: String,
    pub state_root: String,
    pub transactions_root: String,
    pub receipts_root: String,
    pub proposer: String,
    pub timestamp_ms: u64,
    pub tx_count: usize,
    pub gas_used: u64,
}

/// Simulation result returned by the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub success: bool,
    pub gas_used: u64,
    pub error: Option<String>,
    pub events: Vec<EventInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventInfo {
    pub emitter: String,
    pub topic: String,
}

/// Submit result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitResult {
    pub accepted: bool,
    pub error: Option<String>,
}

/// Chain status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainStatus {
    pub height: u64,
    pub latest_state_root: String,
    pub pending_ops: usize,
    /// Total tokens allocated at genesis (base units).
    pub total_allocation: String,
    /// Tokens currently in circulation (not locked in system pools).
    pub total_circulation: String,
}

/// Validator info returned by the RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorInfo {
    pub address: String,
    pub self_stake: String,
    pub total_delegated: String,
    pub total_stake: String,
    pub is_active: bool,
    pub is_genesis: bool,
    pub commission_bps: u64,
}

/// Staking info for an account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingInfo {
    pub total_delegated: String,
    pub delegations: Vec<DelegationInfo>,
    pub pending_undelegations: usize,
}

/// A single delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationInfo {
    pub validator: String,
    pub amount: String,
}

/// Vesting schedule info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VestingInfo {
    pub has_schedule: bool,
    pub total_amount: String,
    pub vested: String,
    pub claimed: String,
    pub claimable: String,
    pub vesting_type: String,
}

#[rpc(server)]
pub trait SolenApi {
    #[method(name = "solen_getBalance")]
    fn get_balance(&self, account_id: String) -> RpcResult<String>;

    #[method(name = "solen_getAccount")]
    fn get_account(&self, account_id: String) -> RpcResult<AccountInfo>;

    #[method(name = "solen_getBlock")]
    fn get_block(&self, height: u64) -> RpcResult<BlockInfo>;

    #[method(name = "solen_getLatestBlock")]
    fn get_latest_block(&self) -> RpcResult<BlockInfo>;

    #[method(name = "solen_submitOperation")]
    fn submit_operation(&self, op: UserOperation) -> RpcResult<SubmitResult>;

    #[method(name = "solen_simulateOperation")]
    fn simulate_operation(&self, op: UserOperation) -> RpcResult<SimulationResult>;

    #[method(name = "solen_chainStatus")]
    fn chain_status(&self) -> RpcResult<ChainStatus>;

    #[method(name = "solen_getValidators")]
    fn get_validators(&self) -> RpcResult<Vec<ValidatorInfo>>;

    #[method(name = "solen_getStakingInfo")]
    fn get_staking_info(&self, account_id: String) -> RpcResult<StakingInfo>;

    #[method(name = "solen_getVestingInfo")]
    fn get_vesting_info(&self, account_id: String) -> RpcResult<VestingInfo>;
}

/// Implementation of the Solen RPC API.
pub struct SolenRpc {
    engine: Arc<ConsensusEngine>,
}

impl SolenRpc {
    pub fn new(engine: Arc<ConsensusEngine>) -> Self {
        Self { engine }
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, ErrorObjectOwned> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| {
                ErrorObjectOwned::owned(
                    -32602,
                    format!("invalid hex at position {i}"),
                    None::<()>,
                )
            })
        })
        .collect()
}

fn parse_account_id(s: &str) -> RpcResult<[u8; 32]> {
    let bytes = hex_decode(s)?;
    if bytes.len() != 32 {
        return Err(ErrorObjectOwned::owned(
            -32602,
            format!("account_id must be 32 bytes, got {}", bytes.len()),
            None::<()>,
        ));
    }
    let mut id = [0u8; 32];
    id.copy_from_slice(&bytes);
    Ok(id)
}

fn internal_error(msg: impl ToString) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32603, msg.to_string(), None::<()>)
}

impl SolenApiServer for SolenRpc {
    fn get_balance(&self, account_id: String) -> RpcResult<String> {
        let id = parse_account_id(&account_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let state = ReadonlyStateManager::new(store.as_ref());
        let balance = state.get_balance(&id).map_err(|e| internal_error(e))?;
        Ok(balance.to_string())
    }

    fn get_account(&self, account_id: String) -> RpcResult<AccountInfo> {
        let id = parse_account_id(&account_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let state = ReadonlyStateManager::new(store.as_ref());
        let account = state
            .get_account(&id)
            .map_err(|e| internal_error(e))?
            .ok_or_else(|| {
                ErrorObjectOwned::owned(-32001, "account not found", None::<()>)
            })?;

        Ok(AccountInfo {
            id: hex_encode(&account.id),
            balance: account.balance.to_string(),
            nonce: account.nonce,
            code_hash: hex_encode(&account.code_hash),
        })
    }

    fn get_block(&self, height: u64) -> RpcResult<BlockInfo> {
        let block = self.engine.get_block(height).ok_or_else(|| {
            ErrorObjectOwned::owned(-32001, "block not found", None::<()>)
        })?;

        Ok(block_to_info(&block))
    }

    fn get_latest_block(&self) -> RpcResult<BlockInfo> {
        let block = self.engine.latest_block().ok_or_else(|| {
            ErrorObjectOwned::owned(-32001, "no blocks yet", None::<()>)
        })?;

        Ok(block_to_info(&block))
    }

    fn submit_operation(&self, op: UserOperation) -> RpcResult<SubmitResult> {
        let accepted = self.engine.mempool().submit(op);
        Ok(SubmitResult {
            accepted,
            error: if accepted {
                None
            } else {
                Some("mempool full".to_string())
            },
        })
    }

    fn simulate_operation(&self, op: UserOperation) -> RpcResult<SimulationResult> {
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let executor = BlockExecutor::new();
        let receipt = executor.simulate(store.as_ref(), &op);

        Ok(SimulationResult {
            success: receipt.success,
            gas_used: receipt.gas_used,
            error: receipt.error,
            events: receipt
                .events
                .iter()
                .map(|e| EventInfo {
                    emitter: hex_encode(&e.emitter),
                    topic: String::from_utf8_lossy(&e.topic).to_string(),
                })
                .collect(),
        })
    }

    fn chain_status(&self) -> RpcResult<ChainStatus> {
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;

        let state = ReadonlyStateManager::new(store.as_ref());

        // Read total supply stored at genesis.
        let total_allocation = match store.get(b"__total_supply__") {
            Ok(Some(data)) if data.len() >= 16 => {
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&data[..16]);
                u128::from_le_bytes(buf)
            }
            _ => 0,
        };

        // Circulation = total supply minus all system/fund account balances and staked tokens.
        use solen_types::system::*;
        let non_circulating_addresses = [
            TREASURY_ADDRESS,
            STAKING_POOL_ADDRESS,
            ECOSYSTEM_FUND_ADDRESS,
            COMMUNITY_ADDRESS,
            LIQUIDITY_ADDRESS,
            TEAM_POOL_ADDRESS,
            INVESTOR_POOL_ADDRESS,
            STAKING_ADDRESS,
            GOVERNANCE_ADDRESS,
            BRIDGE_ADDRESS,
            INTENT_ADDRESS,
            VESTING_ADDRESS,
        ];
        let non_circulating: u128 = non_circulating_addresses
            .iter()
            .map(|addr| state.get_balance(addr).unwrap_or(0))
            .sum();

        // Staked tokens are also not circulating.
        let staking =
            solen_system_contracts::staking::StakingContract::load(store.as_ref());
        let total_staked: u128 = staking.validators.iter().map(|v| v.total_stake()).sum();

        let total_circulation = total_allocation.saturating_sub(non_circulating).saturating_sub(total_staked);

        Ok(ChainStatus {
            height: self.engine.height(),
            latest_state_root: hex_encode(&store.state_root()),
            pending_ops: self.engine.mempool().len(),
            total_allocation: total_allocation.to_string(),
            total_circulation: total_circulation.to_string(),
        })
    }

    fn get_validators(&self) -> RpcResult<Vec<ValidatorInfo>> {
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let staking =
            solen_system_contracts::staking::StakingContract::load(store.as_ref());

        let validators: Vec<ValidatorInfo> = staking
            .validators
            .iter()
            .map(|v| ValidatorInfo {
                address: hex_encode(&v.id),
                self_stake: v.self_stake.to_string(),
                total_delegated: v.total_delegated.to_string(),
                total_stake: v.total_stake().to_string(),
                is_active: v.is_active,
                is_genesis: v.is_genesis,
                commission_bps: v.commission_rate_bps,
            })
            .collect();

        Ok(validators)
    }

    fn get_staking_info(&self, account_id: String) -> RpcResult<StakingInfo> {
        let id = parse_account_id(&account_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let staking =
            solen_system_contracts::staking::StakingContract::load(store.as_ref());

        let delegations: Vec<DelegationInfo> = staking
            .delegations
            .iter()
            .filter(|d| d.delegator == id)
            .map(|d| DelegationInfo {
                validator: hex_encode(&d.validator),
                amount: d.amount.to_string(),
            })
            .collect();

        let total: u128 = delegations
            .iter()
            .map(|d| d.amount.parse::<u128>().unwrap_or(0))
            .sum();

        let pending = staking
            .undelegations
            .iter()
            .filter(|u| u.delegator == id)
            .count();

        Ok(StakingInfo {
            total_delegated: total.to_string(),
            delegations,
            pending_undelegations: pending,
        })
    }

    fn get_vesting_info(&self, account_id: String) -> RpcResult<VestingInfo> {
        let id = parse_account_id(&account_id)?;
        let store = self.engine.store();
        let store = store.read().map_err(|e| internal_error(e.to_string()))?;
        let vesting =
            solen_system_contracts::vesting::VestingContract::load(store.as_ref());

        match vesting.get_schedule(&id) {
            Some(schedule) => {
                // Read current epoch from chain meta.
                let current_epoch = match store.get(b"__chain_meta__") {
                    Ok(Some(data)) if data.len() >= 16 => {
                        let mut h = [0u8; 8];
                        h.copy_from_slice(&data[..8]);
                        u64::from_le_bytes(h) / 100
                    }
                    _ => 0,
                };

                Ok(VestingInfo {
                    has_schedule: true,
                    total_amount: schedule.total_amount.to_string(),
                    vested: schedule.vested_at(current_epoch).to_string(),
                    claimed: schedule.claimed.to_string(),
                    claimable: schedule.claimable_at(current_epoch).to_string(),
                    vesting_type: format!("{:?}", schedule.vesting_type),
                })
            }
            None => Ok(VestingInfo {
                has_schedule: false,
                total_amount: "0".into(),
                vested: "0".into(),
                claimed: "0".into(),
                claimable: "0".into(),
                vesting_type: "".into(),
            }),
        }
    }
}

fn block_to_info(block: &solen_consensus::engine::FinalizedBlock) -> BlockInfo {
    BlockInfo {
        height: block.header.height,
        epoch: block.header.epoch,
        parent_hash: hex_encode(&block.header.parent_hash),
        state_root: hex_encode(&block.header.state_root),
        transactions_root: hex_encode(&block.header.transactions_root),
        receipts_root: hex_encode(&block.header.receipts_root),
        proposer: hex_encode(&block.header.proposer),
        timestamp_ms: block.header.timestamp_ms,
        tx_count: block.result.receipts.len(),
        gas_used: block.result.gas_used,
    }
}
