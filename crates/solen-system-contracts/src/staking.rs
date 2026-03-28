//! Staking system contract: delegation, undelegation, reward claims.
//!
//! Validators register with a stake deposit. Delegators can delegate to
//! validators and earn a share of epoch rewards. Undelegation has a
//! cooldown period before funds can be withdrawn.

use serde::{Deserialize, Serialize};
use solen_types::{AccountId, ValidatorId};
use thiserror::Error;

/// Unbonding cooldown in epochs.
pub const UNBONDING_PERIOD: u64 = 7;

/// Minimum stake to register as a validator.
pub const MIN_VALIDATOR_STAKE: u128 = 1000;

#[derive(Debug, Error)]
pub enum StakingError {
    #[error("insufficient stake: need {need}, have {have}")]
    InsufficientStake { need: u128, have: u128 },
    #[error("validator not found")]
    ValidatorNotFound,
    #[error("delegation not found")]
    DelegationNotFound,
    #[error("unbonding not ready: {remaining} epochs remaining")]
    UnbondingNotReady { remaining: u64 },
    #[error("already registered")]
    AlreadyRegistered,
}

/// A delegation from an account to a validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegation {
    pub delegator: AccountId,
    pub validator: ValidatorId,
    pub amount: u128,
    pub reward_debt: u128,
}

/// A pending undelegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Undelegation {
    pub delegator: AccountId,
    pub validator: ValidatorId,
    pub amount: u128,
    pub unlock_epoch: u64,
}

/// A registered validator with staking info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingValidator {
    pub id: ValidatorId,
    pub self_stake: u128,
    pub total_delegated: u128,
    pub accumulated_reward_per_token: u128,
    pub is_active: bool,
}

impl StakingValidator {
    pub fn total_stake(&self) -> u128 {
        self.self_stake + self.total_delegated
    }
}

/// The staking contract state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StakingContract {
    pub validators: Vec<StakingValidator>,
    pub delegations: Vec<Delegation>,
    pub undelegations: Vec<Undelegation>,
}

impl StakingContract {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new validator with an initial self-stake.
    pub fn register_validator(
        &mut self,
        id: ValidatorId,
        self_stake: u128,
    ) -> Result<(), StakingError> {
        if self_stake < MIN_VALIDATOR_STAKE {
            return Err(StakingError::InsufficientStake {
                need: MIN_VALIDATOR_STAKE,
                have: self_stake,
            });
        }
        if self.validators.iter().any(|v| v.id == id) {
            return Err(StakingError::AlreadyRegistered);
        }
        self.validators.push(StakingValidator {
            id,
            self_stake,
            total_delegated: 0,
            accumulated_reward_per_token: 0,
            is_active: true,
        });
        Ok(())
    }

    /// Delegate tokens to a validator.
    pub fn delegate(
        &mut self,
        delegator: AccountId,
        validator: ValidatorId,
        amount: u128,
    ) -> Result<(), StakingError> {
        let val = self
            .validators
            .iter_mut()
            .find(|v| v.id == validator)
            .ok_or(StakingError::ValidatorNotFound)?;

        val.total_delegated += amount;
        let reward_debt = val.accumulated_reward_per_token * amount / 1_000_000;

        // Check if delegation already exists.
        if let Some(d) = self
            .delegations
            .iter_mut()
            .find(|d| d.delegator == delegator && d.validator == validator)
        {
            d.amount += amount;
            d.reward_debt += reward_debt;
        } else {
            self.delegations.push(Delegation {
                delegator,
                validator,
                amount,
                reward_debt,
            });
        }

        Ok(())
    }

    /// Begin undelegation. Funds are locked for UNBONDING_PERIOD epochs.
    pub fn undelegate(
        &mut self,
        delegator: AccountId,
        validator: ValidatorId,
        amount: u128,
        current_epoch: u64,
    ) -> Result<(), StakingError> {
        let delegation = self
            .delegations
            .iter_mut()
            .find(|d| d.delegator == delegator && d.validator == validator)
            .ok_or(StakingError::DelegationNotFound)?;

        if delegation.amount < amount {
            return Err(StakingError::InsufficientStake {
                need: amount,
                have: delegation.amount,
            });
        }

        delegation.amount -= amount;

        // Reduce validator's total.
        if let Some(val) = self.validators.iter_mut().find(|v| v.id == validator) {
            val.total_delegated = val.total_delegated.saturating_sub(amount);
        }

        self.undelegations.push(Undelegation {
            delegator,
            validator,
            amount,
            unlock_epoch: current_epoch + UNBONDING_PERIOD,
        });

        Ok(())
    }

    /// Withdraw unlocked undelegations. Returns the total amount withdrawn.
    pub fn withdraw_undelegated(
        &mut self,
        delegator: AccountId,
        current_epoch: u64,
    ) -> u128 {
        let mut total = 0u128;
        self.undelegations.retain(|u| {
            if u.delegator == delegator && u.unlock_epoch <= current_epoch {
                total += u.amount;
                false
            } else {
                true
            }
        });
        total
    }

    /// Distribute rewards to a validator. Increases the per-token accumulator.
    pub fn distribute_rewards(
        &mut self,
        validator: ValidatorId,
        reward: u128,
    ) -> Result<(), StakingError> {
        let val = self
            .validators
            .iter_mut()
            .find(|v| v.id == validator)
            .ok_or(StakingError::ValidatorNotFound)?;

        if val.total_stake() > 0 {
            val.accumulated_reward_per_token +=
                reward * 1_000_000 / val.total_stake();
        }

        Ok(())
    }

    /// Get a validator's info.
    pub fn get_validator(&self, id: &ValidatorId) -> Option<&StakingValidator> {
        self.validators.iter().find(|v| v.id == *id)
    }

    /// Get a delegator's total stake across all validators.
    pub fn delegator_total_stake(&self, delegator: &AccountId) -> u128 {
        self.delegations
            .iter()
            .filter(|d| d.delegator == *delegator)
            .map(|d| d.amount)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vid(n: u8) -> ValidatorId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn aid(n: u8) -> AccountId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn register_and_delegate() {
        let mut sc = StakingContract::new();

        sc.register_validator(vid(1), 5000).unwrap();
        sc.delegate(aid(10), vid(1), 2000).unwrap();

        let val = sc.get_validator(&vid(1)).unwrap();
        assert_eq!(val.total_stake(), 7000);
        assert_eq!(sc.delegator_total_stake(&aid(10)), 2000);
    }

    #[test]
    fn register_below_minimum() {
        let mut sc = StakingContract::new();
        let err = sc.register_validator(vid(1), 500).unwrap_err();
        assert!(matches!(err, StakingError::InsufficientStake { .. }));
    }

    #[test]
    fn undelegate_and_withdraw() {
        let mut sc = StakingContract::new();
        sc.register_validator(vid(1), 5000).unwrap();
        sc.delegate(aid(10), vid(1), 3000).unwrap();

        // Undelegate at epoch 5.
        sc.undelegate(aid(10), vid(1), 1000, 5).unwrap();
        assert_eq!(sc.delegator_total_stake(&aid(10)), 2000);

        // Try to withdraw too early.
        let withdrawn = sc.withdraw_undelegated(aid(10), 10);
        assert_eq!(withdrawn, 0);

        // Withdraw after unbonding period (5 + 7 = 12).
        let withdrawn = sc.withdraw_undelegated(aid(10), 12);
        assert_eq!(withdrawn, 1000);
    }

    #[test]
    fn reward_distribution() {
        let mut sc = StakingContract::new();
        sc.register_validator(vid(1), 5000).unwrap();
        sc.delegate(aid(10), vid(1), 5000).unwrap();

        // Total stake = 10000. Distribute 1000 reward.
        sc.distribute_rewards(vid(1), 1000).unwrap();

        let val = sc.get_validator(&vid(1)).unwrap();
        assert_eq!(val.accumulated_reward_per_token, 1000 * 1_000_000 / 10000);
    }

    #[test]
    fn duplicate_registration_fails() {
        let mut sc = StakingContract::new();
        sc.register_validator(vid(1), 5000).unwrap();
        let err = sc.register_validator(vid(1), 5000).unwrap_err();
        assert!(matches!(err, StakingError::AlreadyRegistered));
    }
}
