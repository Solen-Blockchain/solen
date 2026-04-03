//! Epoch transition logic: reward distribution, validator rotation.

use solen_types::Epoch;
use tracing::info;

use crate::validator::ValidatorSet;

/// Blocks per epoch.
pub const EPOCH_LENGTH: u64 = 100;

/// Manages epoch boundaries and transitions.
pub struct EpochManager {
    pub current_epoch: Epoch,
    /// Base reward per epoch per validator (in smallest units).
    pub base_reward: u128,
}

impl EpochManager {
    pub fn new() -> Self {
        Self {
            current_epoch: 0,
            base_reward: 100,
        }
    }

    /// Returns true if a given block height is an epoch boundary.
    pub fn is_epoch_boundary(&self, height: u64) -> bool {
        height > 0 && height % EPOCH_LENGTH == 0
    }

    /// Compute the epoch number for a given block height.
    pub fn epoch_for_height(&self, height: u64) -> Epoch {
        height / EPOCH_LENGTH
    }

    /// Process an epoch transition: distribute rewards to active validators,
    /// reset missed-block counters, and advance the epoch.
    pub fn process_epoch_transition(&mut self, validator_set: &mut ValidatorSet) {
        let new_epoch = self.current_epoch + 1;

        // Reward active validators proportionally to stake.
        let total_stake = validator_set.total_active_stake();
        if total_stake > 0 {
            let total_reward = self.base_reward * validator_set.active_count() as u128;
            for v in validator_set.all().to_vec() {
                if v.is_active() {
                    if let Some(vm) = validator_set.get_mut(&v.id) {
                        let reward = total_reward * vm.stake / total_stake;
                        vm.stake += reward;
                        // Only reset missed_blocks for validators that are actually
                        // producing. Don't reset for offline validators — their
                        // counter must accumulate across epochs to reach the
                        // downtime threshold (50 missed slots).
                        if vm.missed_blocks == 0 {
                            // Already at 0 — validator is producing normally.
                        }
                        // Do NOT reset missed_blocks here — it's now managed
                        // by the engine's finalization logic (reset on successful
                        // proposal, increment on miss, reset on slash).
                    }
                }
            }
        }

        info!(epoch = new_epoch, "epoch transition complete");
        self.current_epoch = new_epoch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validator::ValidatorInfo;

    fn vid(n: u8) -> [u8; 32] {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn epoch_boundaries() {
        let em = EpochManager::new();
        assert!(!em.is_epoch_boundary(0));
        assert!(!em.is_epoch_boundary(1));
        assert!(!em.is_epoch_boundary(99));
        assert!(em.is_epoch_boundary(100));
        assert!(em.is_epoch_boundary(200));
    }

    #[test]
    fn epoch_rewards_distributed() {
        let mut em = EpochManager::new();
        em.base_reward = 100;

        let mut vs = ValidatorSet::new(vec![
            ValidatorInfo::new(vid(1), 500),
            ValidatorInfo::new(vid(2), 500),
        ]);

        em.process_epoch_transition(&mut vs);

        // Each gets 100 reward (equal stake, 200 total reward / 2)
        assert_eq!(vs.get_mut(&vid(1)).unwrap().stake, 600);
        assert_eq!(vs.get_mut(&vid(2)).unwrap().stake, 600);
        assert_eq!(em.current_epoch, 1);
    }
}
