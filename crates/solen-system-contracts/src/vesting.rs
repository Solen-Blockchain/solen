//! Vesting contract: time-locked token distribution for team, investors, etc.
//!
//! Supports cliff + linear vesting schedules. Tokens are held by the
//! vesting contract and released according to each recipient's schedule.
//!
//! Schedule types:
//!   - Team: 1-year cliff, 3-year linear vest (4 years total)
//!   - Investor: 6-month cliff, 2-year linear vest (2.5 years total)

use serde::{Deserialize, Serialize};
use solen_types::AccountId;
use thiserror::Error;

/// Epochs per year (~157,680 at 100 blocks/epoch, 2s block time).
pub const EPOCHS_PER_YEAR: u64 = 157_680;
pub const EPOCHS_PER_MONTH: u64 = EPOCHS_PER_YEAR / 12;

#[derive(Debug, Error)]
pub enum VestingError {
    #[error("no vesting schedule found for this account")]
    NotFound,
    #[error("nothing to claim yet (cliff not reached)")]
    CliffNotReached,
    #[error("nothing to claim (already fully claimed)")]
    FullyClaimed,
}

/// A vesting schedule category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VestingType {
    /// 1-year cliff, 3-year linear vest.
    Team,
    /// 6-month cliff, 2-year linear vest.
    Investor,
}

impl VestingType {
    /// Cliff duration in epochs.
    pub fn cliff_epochs(&self) -> u64 {
        match self {
            VestingType::Team => EPOCHS_PER_YEAR,           // 1 year
            VestingType::Investor => EPOCHS_PER_MONTH * 6,  // 6 months
        }
    }

    /// Total vesting duration in epochs (from genesis, including cliff).
    pub fn total_epochs(&self) -> u64 {
        match self {
            VestingType::Team => EPOCHS_PER_YEAR * 4,       // 4 years total
            VestingType::Investor => EPOCHS_PER_MONTH * 30, // 2.5 years total
        }
    }
}

/// A single recipient's vesting schedule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VestingSchedule {
    pub recipient: AccountId,
    pub total_amount: u128,
    pub claimed: u128,
    pub vesting_type: VestingType,
    /// Epoch at which vesting starts (usually 0 for genesis).
    pub start_epoch: u64,
}

impl VestingSchedule {
    /// Calculate how much is vested (unlocked) at the given epoch.
    pub fn vested_at(&self, current_epoch: u64) -> u128 {
        let elapsed = current_epoch.saturating_sub(self.start_epoch);
        let cliff = self.vesting_type.cliff_epochs();
        let total_duration = self.vesting_type.total_epochs();

        if elapsed < cliff {
            return 0; // still in cliff period
        }

        if elapsed >= total_duration {
            return self.total_amount; // fully vested
        }

        // Linear vesting from cliff to end.
        // At cliff: vested = 0. At end: vested = total_amount.
        // vested = total * (elapsed - cliff) / (total_duration - cliff)
        let vesting_period = total_duration - cliff;
        if vesting_period == 0 {
            return self.total_amount;
        }
        let elapsed_after_cliff = elapsed - cliff;
        self.total_amount.saturating_mul(elapsed_after_cliff as u128) / vesting_period as u128
    }

    /// How much can be claimed right now.
    pub fn claimable_at(&self, current_epoch: u64) -> u128 {
        let vested = self.vested_at(current_epoch);
        vested.saturating_sub(self.claimed)
    }
}

/// The vesting contract state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VestingContract {
    pub schedules: Vec<VestingSchedule>,
}

impl VestingContract {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a vesting schedule for a recipient.
    pub fn add_schedule(
        &mut self,
        recipient: AccountId,
        total_amount: u128,
        vesting_type: VestingType,
        start_epoch: u64,
    ) {
        self.schedules.push(VestingSchedule {
            recipient,
            total_amount,
            claimed: 0,
            vesting_type,
            start_epoch,
        });
    }

    /// Claim vested tokens for a recipient. Returns the amount claimed.
    pub fn claim(
        &mut self,
        recipient: &AccountId,
        current_epoch: u64,
    ) -> Result<u128, VestingError> {
        let schedule = self
            .schedules
            .iter_mut()
            .find(|s| s.recipient == *recipient)
            .ok_or(VestingError::NotFound)?;

        let claimable = schedule.claimable_at(current_epoch);
        if claimable == 0 {
            if schedule.claimed >= schedule.total_amount {
                return Err(VestingError::FullyClaimed);
            }
            return Err(VestingError::CliffNotReached);
        }

        schedule.claimed += claimable;
        Ok(claimable)
    }

    /// Get a recipient's vesting info.
    pub fn get_schedule(&self, recipient: &AccountId) -> Option<&VestingSchedule> {
        self.schedules.iter().find(|s| s.recipient == *recipient)
    }

    /// Total tokens still locked across all schedules.
    pub fn total_locked(&self) -> u128 {
        self.schedules
            .iter()
            .map(|s| s.total_amount.saturating_sub(s.claimed))
            .sum()
    }

    // ── Persistence ─────────────────────────────────────────

    const STORAGE_KEY: &'static [u8] = b"__vesting_state__";

    pub fn load(store: &dyn solen_storage::StateStore) -> Self {
        match store.get(Self::STORAGE_KEY) {
            Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    pub fn save(&self, store: &mut dyn solen_storage::StateStore) {
        if let Ok(data) = serde_json::to_vec(self) {
            let _ = store.put(Self::STORAGE_KEY, &data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aid(n: u8) -> AccountId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn team_vesting_cliff() {
        let mut vc = VestingContract::new();
        vc.add_schedule(aid(1), 1_000_000, VestingType::Team, 0);

        let schedule = vc.get_schedule(&aid(1)).unwrap();

        // Before cliff (1 year): nothing vested.
        assert_eq!(schedule.vested_at(0), 0);
        assert_eq!(schedule.vested_at(EPOCHS_PER_YEAR - 1), 0);

        // At cliff: nothing vested yet (cliff is the start of linear vesting).
        assert_eq!(schedule.vested_at(EPOCHS_PER_YEAR), 0);

        // Just after cliff: small amount vested.
        let after_cliff = schedule.vested_at(EPOCHS_PER_YEAR + 1);
        assert!(after_cliff > 0);

        // Halfway through vesting period (2.5 years into a 3-year linear vest).
        let midpoint = EPOCHS_PER_YEAR + (EPOCHS_PER_YEAR * 3 / 2);
        let mid_vested = schedule.vested_at(midpoint);
        assert!(mid_vested > 400_000 && mid_vested < 600_000);

        // Fully vested at 4 years.
        assert_eq!(schedule.vested_at(EPOCHS_PER_YEAR * 4), 1_000_000);
    }

    #[test]
    fn investor_vesting_cliff() {
        let mut vc = VestingContract::new();
        vc.add_schedule(aid(1), 1_000_000, VestingType::Investor, 0);

        let schedule = vc.get_schedule(&aid(1)).unwrap();

        // Before cliff (6 months): nothing.
        assert_eq!(schedule.vested_at(EPOCHS_PER_MONTH * 6 - 1), 0);

        // At cliff: nothing vested yet.
        assert_eq!(schedule.vested_at(EPOCHS_PER_MONTH * 6), 0);

        // Just after cliff: some vested.
        assert!(schedule.vested_at(EPOCHS_PER_MONTH * 6 + 1) > 0);

        // Fully vested at 2.5 years.
        assert_eq!(schedule.vested_at(EPOCHS_PER_MONTH * 30), 1_000_000);
    }

    #[test]
    fn claim_flow() {
        let mut vc = VestingContract::new();
        vc.add_schedule(aid(1), 1_000_000, VestingType::Team, 0);

        // Can't claim before cliff.
        assert!(vc.claim(&aid(1), 0).is_err());

        // Can't claim at exact cliff (0 vested at cliff boundary).
        assert!(vc.claim(&aid(1), EPOCHS_PER_YEAR).is_err());

        // Claim after cliff.
        let claimed = vc.claim(&aid(1), EPOCHS_PER_YEAR + EPOCHS_PER_MONTH).unwrap();
        assert!(claimed > 0);

        // Claim again later.
        let claimed2 = vc.claim(&aid(1), EPOCHS_PER_YEAR * 2).unwrap();
        assert!(claimed2 > 0);

        // Claim everything at end.
        let claimed3 = vc.claim(&aid(1), EPOCHS_PER_YEAR * 4).unwrap();
        assert!(claimed3 > 0);

        // Nothing left.
        assert!(vc.claim(&aid(1), EPOCHS_PER_YEAR * 5).is_err());
    }
}
