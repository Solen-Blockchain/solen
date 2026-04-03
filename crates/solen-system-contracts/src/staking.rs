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
/// 500,000 SOLEN in base units (8 decimals).
pub const MIN_VALIDATOR_STAKE: u128 = 500_000 * 100_000_000;

/// Minimum number of active validators. The network will reject
/// deregistrations that would drop below this count.
pub const MIN_VALIDATOR_COUNT: usize = 20;

/// Genesis validator lock period in epochs.
/// At 100 blocks/epoch and 2s block time, 1 epoch ≈ 3.3 minutes.
/// 157,680 epochs ≈ 1 year.
pub const GENESIS_LOCK_EPOCHS: u64 = 157_680;

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
    #[error("genesis validator locked until epoch {unlock_epoch} (current: {current_epoch})")]
    GenesisLocked { unlock_epoch: u64, current_epoch: u64 },
    #[error("cannot deregister: would drop below minimum validator count ({min})")]
    BelowMinValidators { min: usize },
    #[error("key rotation already pending")]
    RotationAlreadyPending,
    #[error("new key already in use by another validator")]
    KeyAlreadyInUse,
}

/// A delegation from an account to a validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegation {
    pub delegator: AccountId,
    pub validator: ValidatorId,
    pub amount: u128,
    pub reward_debt: u128,
    /// Epoch from which this delegation is eligible for rewards.
    /// Set to current_epoch + 1 when delegating (must stake for full epoch).
    #[serde(default)]
    pub eligible_from_epoch: u64,
}

/// A pending undelegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Undelegation {
    pub delegator: AccountId,
    pub validator: ValidatorId,
    pub amount: u128,
    pub unlock_epoch: u64,
}

/// Default validator commission rate (10%).
pub const DEFAULT_COMMISSION_BPS: u64 = 1000;

/// A registered validator with staking info.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingValidator {
    pub id: ValidatorId,
    pub self_stake: u128,
    pub total_delegated: u128,
    pub accumulated_reward_per_token: u128,
    pub is_active: bool,
    /// Whether this validator was in the genesis set.
    pub is_genesis: bool,
    /// Epoch after which a genesis validator can unstake (0 = no lock).
    pub genesis_lock_until: u64,
    /// Commission rate in basis points (e.g., 1000 = 10%).
    /// Validator keeps this % of delegator rewards.
    #[serde(default = "default_commission")]
    pub commission_rate_bps: u64,
    /// Epoch from which this validator is eligible for rewards.
    /// Genesis validators: 0 (always eligible). Others: epoch they joined + 1.
    #[serde(default)]
    pub eligible_from_epoch: u64,
    /// Pending key rotation: new validator ID to switch to at next epoch.
    #[serde(default)]
    pub pending_new_key: Option<ValidatorId>,
    /// Epoch at which the key rotation takes effect.
    #[serde(default)]
    pub key_rotation_epoch: u64,
}

fn default_commission() -> u64 {
    DEFAULT_COMMISSION_BPS
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
            is_genesis: false,
            genesis_lock_until: 0,
            commission_rate_bps: DEFAULT_COMMISSION_BPS,
            eligible_from_epoch: u64::MAX, // not eligible until epoch is set
            pending_new_key: None,
            key_rotation_epoch: 0,
        });
        Ok(())
    }

    /// Register a new validator with epoch tracking for reward eligibility.
    pub fn register_validator_at_epoch(
        &mut self,
        id: ValidatorId,
        self_stake: u128,
        current_epoch: u64,
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
            is_genesis: false,
            genesis_lock_until: 0,
            commission_rate_bps: DEFAULT_COMMISSION_BPS,
            eligible_from_epoch: current_epoch + 1, // eligible starting next epoch
            pending_new_key: None,
            key_rotation_epoch: 0,
        });
        Ok(())
    }

    /// Register a genesis validator. Their stake is locked for GENESIS_LOCK_EPOCHS.
    /// Genesis validators are eligible for rewards from epoch 0.
    pub fn register_genesis_validator(
        &mut self,
        id: ValidatorId,
        self_stake: u128,
    ) -> Result<(), StakingError> {
        if self.validators.iter().any(|v| v.id == id) {
            return Err(StakingError::AlreadyRegistered);
        }
        self.validators.push(StakingValidator {
            id,
            self_stake,
            total_delegated: 0,
            accumulated_reward_per_token: 0,
            is_active: true,
            is_genesis: true,
            genesis_lock_until: GENESIS_LOCK_EPOCHS,
            commission_rate_bps: DEFAULT_COMMISSION_BPS,
            eligible_from_epoch: 0, // genesis validators always eligible
            pending_new_key: None,
            key_rotation_epoch: 0,
        });
        Ok(())
    }

    /// Deregister a validator and return their self-stake.
    /// Fails if the validator is genesis-locked or if it would drop
    /// below the minimum validator count.
    pub fn deregister_validator(
        &mut self,
        id: &ValidatorId,
        current_epoch: u64,
    ) -> Result<u128, StakingError> {
        let val = self
            .validators
            .iter()
            .find(|v| v.id == *id)
            .ok_or(StakingError::ValidatorNotFound)?;

        // Check genesis lock.
        if val.is_genesis && current_epoch < val.genesis_lock_until {
            return Err(StakingError::GenesisLocked {
                unlock_epoch: val.genesis_lock_until,
                current_epoch,
            });
        }

        // Check minimum validator count.
        let active_count = self.validators.iter().filter(|v| v.is_active).count();
        if active_count <= MIN_VALIDATOR_COUNT {
            return Err(StakingError::BelowMinValidators {
                min: MIN_VALIDATOR_COUNT,
            });
        }

        let stake = val.self_stake;

        // Deactivate.
        if let Some(v) = self.validators.iter_mut().find(|v| v.id == *id) {
            v.is_active = false;
            v.self_stake = 0;
        }

        Ok(stake)
    }

    /// Delegate tokens to a validator.
    pub fn delegate(
        &mut self,
        delegator: AccountId,
        validator: ValidatorId,
        amount: u128,
    ) -> Result<(), StakingError> {
        self.delegate_at_epoch(delegator, validator, amount, 0)
    }

    /// Delegate with epoch tracking for reward eligibility.
    pub fn delegate_at_epoch(
        &mut self,
        delegator: AccountId,
        validator: ValidatorId,
        amount: u128,
        current_epoch: u64,
    ) -> Result<(), StakingError> {
        let val = self
            .validators
            .iter_mut()
            .find(|v| v.id == validator)
            .ok_or(StakingError::ValidatorNotFound)?;

        val.total_delegated = val.total_delegated.saturating_add(amount);
        let reward_debt = val.accumulated_reward_per_token.saturating_mul(amount) / 1_000_000;

        // Check if delegation already exists.
        if let Some(d) = self
            .delegations
            .iter_mut()
            .find(|d| d.delegator == delegator && d.validator == validator)
        {
            d.amount = d.amount.saturating_add(amount);
            d.reward_debt = d.reward_debt.saturating_add(reward_debt);
            // Don't reset eligible_from_epoch on additional delegation
        } else {
            self.delegations.push(Delegation {
                delegator,
                validator,
                amount,
                reward_debt,
                eligible_from_epoch: current_epoch + 1, // eligible starting next epoch
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

        // Remove zero-amount delegations to prevent state bloat.
        if delegation.amount == 0 {
            self.delegations.retain(|d| !(d.delegator == delegator && d.validator == validator && d.amount == 0));
        }

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

    /// Set a validator's commission rate.
    pub fn set_commission(
        &mut self,
        validator_id: &ValidatorId,
        commission_bps: u64,
    ) -> Result<(), StakingError> {
        let val = self
            .validators
            .iter_mut()
            .find(|v| v.id == *validator_id)
            .ok_or(StakingError::ValidatorNotFound)?;
        val.commission_rate_bps = commission_bps.min(10_000); // cap at 100%
        Ok(())
    }

    /// Reactivate a jailed/slashed validator. The validator must still meet the
    /// minimum stake requirement. Caller must be the validator itself.
    pub fn unjail(&mut self, validator_id: &ValidatorId) -> Result<(), StakingError> {
        let val = self
            .validators
            .iter_mut()
            .find(|v| v.id == *validator_id)
            .ok_or(StakingError::ValidatorNotFound)?;

        if val.is_active {
            return Ok(()); // Already active, nothing to do.
        }

        if val.self_stake < MIN_VALIDATOR_STAKE {
            return Err(StakingError::InsufficientStake {
                need: MIN_VALIDATOR_STAKE,
                have: val.self_stake,
            });
        }

        val.is_active = true;
        Ok(())
    }

    /// Request key rotation for a validator. The new key takes effect at next epoch.
    /// Must be called by the current validator key (verified at the system call level).
    pub fn rotate_key(
        &mut self,
        validator_id: &ValidatorId,
        new_key: ValidatorId,
        current_epoch: u64,
    ) -> Result<(), StakingError> {
        // Check new key isn't already in use.
        if self.validators.iter().any(|v| v.id == new_key) {
            return Err(StakingError::KeyAlreadyInUse);
        }

        let val = self
            .validators
            .iter_mut()
            .find(|v| v.id == *validator_id)
            .ok_or(StakingError::ValidatorNotFound)?;

        if val.pending_new_key.is_some() {
            return Err(StakingError::RotationAlreadyPending);
        }

        val.pending_new_key = Some(new_key);
        val.key_rotation_epoch = current_epoch + 1;
        Ok(())
    }

    /// Apply all pending key rotations that have reached their effective epoch.
    /// Call this at each epoch boundary.
    pub fn apply_pending_rotations(&mut self, current_epoch: u64) -> Vec<(ValidatorId, ValidatorId)> {
        let mut rotated = Vec::new();

        for val in &mut self.validators {
            if let Some(new_key) = val.pending_new_key.take() {
                if current_epoch >= val.key_rotation_epoch {
                    let old_key = val.id;
                    val.id = new_key;
                    val.key_rotation_epoch = 0;
                    rotated.push((old_key, new_key));
                } else {
                    // Not yet — put it back.
                    val.pending_new_key = Some(new_key);
                }
            }
        }

        // Update delegation records to point to the new key.
        for (old_key, new_key) in &rotated {
            for d in &mut self.delegations {
                if d.validator == *old_key {
                    d.validator = *new_key;
                }
            }
            for u in &mut self.undelegations {
                if u.validator == *old_key {
                    u.validator = *new_key;
                }
            }
        }

        rotated
    }

    /// Get all delegations for a specific validator.
    pub fn delegations_for_validator(&self, validator_id: &ValidatorId) -> Vec<&Delegation> {
        self.delegations
            .iter()
            .filter(|d| d.validator == *validator_id)
            .collect()
    }

    /// Get delegations eligible for rewards at the given epoch.
    pub fn eligible_delegations_for_validator(
        &self,
        validator_id: &ValidatorId,
        epoch: u64,
    ) -> Vec<&Delegation> {
        self.delegations
            .iter()
            .filter(|d| d.validator == *validator_id && epoch >= d.eligible_from_epoch)
            .collect()
    }

    /// Get validators eligible for rewards at the given epoch.
    pub fn eligible_validators(&self, epoch: u64) -> Vec<&StakingValidator> {
        self.validators
            .iter()
            .filter(|v| v.is_active && epoch >= v.eligible_from_epoch)
            .collect()
    }

    /// Number of active validators.
    pub fn active_validator_count(&self) -> usize {
        self.validators.iter().filter(|v| v.is_active).count()
    }

    /// Total stake across all active validators.
    pub fn total_active_stake(&self) -> u128 {
        self.validators
            .iter()
            .filter(|v| v.is_active)
            .map(|v| v.total_stake())
            .sum()
    }

    /// Get all active validators.
    pub fn active_validators(&self) -> Vec<&StakingValidator> {
        self.validators.iter().filter(|v| v.is_active).collect()
    }

    // ── Persistence ─────────────────────────────────────────────

    const STORAGE_KEY: &'static [u8] = b"__staking_state__";

    /// Load staking state from the store.
    pub fn load(store: &dyn solen_storage::StateStore) -> Self {
        match store.get(Self::STORAGE_KEY) {
            Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    /// Save staking state to the store.
    pub fn save(&self, store: &mut dyn solen_storage::StateStore) {
        if let Ok(data) = serde_json::to_vec(self) {
            let _ = store.put(Self::STORAGE_KEY, &data);
        }
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

        sc.register_validator(vid(1), MIN_VALIDATOR_STAKE).unwrap();
        sc.delegate(aid(10), vid(1), 20_000).unwrap();

        let val = sc.get_validator(&vid(1)).unwrap();
        assert_eq!(val.total_stake(), MIN_VALIDATOR_STAKE + 20_000);
        assert_eq!(sc.delegator_total_stake(&aid(10)), 20_000);
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
        sc.register_validator(vid(1), MIN_VALIDATOR_STAKE).unwrap();
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
        sc.register_validator(vid(1), MIN_VALIDATOR_STAKE).unwrap();
        sc.delegate(aid(10), vid(1), MIN_VALIDATOR_STAKE).unwrap();

        // Total stake = 2 * MIN_VALIDATOR_STAKE. Distribute 10,000 reward.
        let total_stake = MIN_VALIDATOR_STAKE * 2;
        sc.distribute_rewards(vid(1), 10_000).unwrap();

        let val = sc.get_validator(&vid(1)).unwrap();
        assert_eq!(val.accumulated_reward_per_token, 10_000 * 1_000_000 / total_stake);
    }

    #[test]
    fn duplicate_registration_fails() {
        let mut sc = StakingContract::new();
        sc.register_validator(vid(1), MIN_VALIDATOR_STAKE).unwrap();
        let err = sc.register_validator(vid(1), MIN_VALIDATOR_STAKE).unwrap_err();
        assert!(matches!(err, StakingError::AlreadyRegistered));
    }

    #[test]
    fn genesis_validator_locked() {
        let mut sc = StakingContract::new();
        sc.register_genesis_validator(vid(1), 100_000).unwrap();

        let val = sc.get_validator(&vid(1)).unwrap();
        assert!(val.is_genesis);
        assert_eq!(val.genesis_lock_until, GENESIS_LOCK_EPOCHS);

        // Can't deregister during lock period.
        let err = sc.deregister_validator(&vid(1), 1000).unwrap_err();
        assert!(matches!(err, StakingError::GenesisLocked { .. }));

        // Can deregister after lock period (if enough validators).
        // But we need MIN_VALIDATOR_COUNT active validators first.
        for i in 2..=25 {
            sc.register_validator(vid(i), MIN_VALIDATOR_STAKE).unwrap();
        }

        let stake = sc.deregister_validator(&vid(1), GENESIS_LOCK_EPOCHS + 1).unwrap();
        assert_eq!(stake, 100_000);
        assert!(!sc.get_validator(&vid(1)).unwrap().is_active);
    }

    #[test]
    fn minimum_validator_count_enforced() {
        let mut sc = StakingContract::new();

        // Register exactly MIN_VALIDATOR_COUNT validators.
        for i in 1..=(MIN_VALIDATOR_COUNT as u8) {
            sc.register_validator(vid(i), MIN_VALIDATOR_STAKE).unwrap();
        }

        // Can't deregister — would drop below minimum.
        let err = sc.deregister_validator(&vid(1), 999_999).unwrap_err();
        assert!(matches!(err, StakingError::BelowMinValidators { .. }));

        // Add one more, then we can remove one.
        sc.register_validator(vid(99), MIN_VALIDATOR_STAKE).unwrap();
        let stake = sc.deregister_validator(&vid(1), 999_999).unwrap();
        assert_eq!(stake, MIN_VALIDATOR_STAKE);
    }

    #[test]
    fn non_genesis_can_deregister_freely() {
        let mut sc = StakingContract::new();

        // Register enough validators.
        for i in 1..=25 {
            sc.register_validator(vid(i), MIN_VALIDATOR_STAKE).unwrap();
        }

        // Non-genesis validator can deregister at any epoch.
        let val = sc.get_validator(&vid(5)).unwrap();
        assert!(!val.is_genesis);

        let stake = sc.deregister_validator(&vid(5), 0).unwrap();
        assert_eq!(stake, MIN_VALIDATOR_STAKE);
    }
}
