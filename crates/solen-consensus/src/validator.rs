//! Validator set management.

use serde::{Deserialize, Serialize};
use solen_types::ValidatorId;

/// Status of a validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidatorStatus {
    Active,
    Jailed,
    Exiting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorInfo {
    pub id: ValidatorId,
    pub stake: u128,
    pub status: ValidatorStatus,
    pub missed_blocks: u64,
}

impl ValidatorInfo {
    pub fn new(id: ValidatorId, stake: u128) -> Self {
        Self {
            id,
            stake,
            status: ValidatorStatus::Active,
            missed_blocks: 0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.status == ValidatorStatus::Active
    }
}

/// Tracks the current validator set with quorum calculations.
#[derive(Debug, Clone)]
pub struct ValidatorSet {
    validators: Vec<ValidatorInfo>,
}

impl ValidatorSet {
    pub fn new(validators: Vec<ValidatorInfo>) -> Self {
        Self { validators }
    }

    /// Get all validators.
    pub fn all(&self) -> &[ValidatorInfo] {
        &self.validators
    }

    /// Get only active validators, sorted by ID for deterministic ordering.
    /// All nodes must agree on proposer selection, so ordering must be identical.
    pub fn active(&self) -> Vec<&ValidatorInfo> {
        let mut active: Vec<_> = self.validators.iter().filter(|v| v.is_active()).collect();
        active.sort_by_key(|v| v.id);
        active
    }

    /// Total active stake.
    pub fn total_active_stake(&self) -> u128 {
        self.active().iter().map(|v| v.stake).sum()
    }

    /// Number of active validators.
    pub fn active_count(&self) -> usize {
        self.active().len()
    }

    /// Get the proposer for a given height using round-robin.
    pub fn proposer_for_height(&self, height: u64) -> Option<ValidatorId> {
        let active = self.active();
        if active.is_empty() {
            return None;
        }
        let idx = (height as usize) % active.len();
        Some(active[idx].id)
    }

    /// Check if a set of attesters forms a 2/3+ quorum by stake.
    pub fn has_quorum(&self, attester_ids: &[ValidatorId]) -> bool {
        let total = self.total_active_stake();
        if total == 0 {
            return false;
        }
        let attested_stake: u128 = self
            .validators
            .iter()
            .filter(|v| v.is_active() && attester_ids.contains(&v.id))
            .map(|v| v.stake)
            .sum();

        // 2/3+ quorum: attested_stake * 3 > total * 2
        attested_stake * 3 > total * 2
    }

    /// Get a mutable reference to a validator by ID.
    pub fn get_mut(&mut self, id: &ValidatorId) -> Option<&mut ValidatorInfo> {
        self.validators.iter_mut().find(|v| v.id == *id)
    }

    /// Add a new validator to the set.
    pub fn add(&mut self, info: ValidatorInfo) {
        self.validators.push(info);
    }

    /// Remove a validator from the set entirely (e.g., exited from staking).
    pub fn remove(&mut self, id: &ValidatorId) {
        self.validators.retain(|v| v.id != *id);
    }

    /// Jail a validator (remove from active set).
    pub fn jail(&mut self, id: &ValidatorId) -> bool {
        if let Some(v) = self.get_mut(id) {
            v.status = ValidatorStatus::Jailed;
            true
        } else {
            false
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

    fn test_set() -> ValidatorSet {
        ValidatorSet::new(vec![
            ValidatorInfo::new(vid(1), 100),
            ValidatorInfo::new(vid(2), 100),
            ValidatorInfo::new(vid(3), 100),
        ])
    }

    #[test]
    fn round_robin_proposer() {
        let vs = test_set();
        assert_eq!(vs.proposer_for_height(1), Some(vid(2)));
        assert_eq!(vs.proposer_for_height(2), Some(vid(3)));
        assert_eq!(vs.proposer_for_height(3), Some(vid(1)));
    }

    #[test]
    fn quorum_requires_two_thirds() {
        let vs = test_set();
        // 1 of 3 = 33% — no quorum
        assert!(!vs.has_quorum(&[vid(1)]));
        // 2 of 3 = 67% — just barely quorum (200 * 3 = 600 > 300 * 2 = 600? No, need >)
        assert!(!vs.has_quorum(&[vid(1), vid(2)]));
        // 3 of 3 = 100% — quorum
        assert!(vs.has_quorum(&[vid(1), vid(2), vid(3)]));
    }

    #[test]
    fn quorum_with_unequal_stake() {
        let vs = ValidatorSet::new(vec![
            ValidatorInfo::new(vid(1), 200), // big validator
            ValidatorInfo::new(vid(2), 50),
            ValidatorInfo::new(vid(3), 50),
        ]);
        // vid(1) alone: 200/300 = 67%, 200*3=600 > 300*2=600? No, not strictly >
        assert!(!vs.has_quorum(&[vid(1)]));
        // vid(1) + vid(2): 250/300 = 83% — quorum
        assert!(vs.has_quorum(&[vid(1), vid(2)]));
    }

    #[test]
    fn jailing_removes_from_active() {
        let mut vs = test_set();
        assert_eq!(vs.active_count(), 3);

        vs.jail(&vid(2));
        assert_eq!(vs.active_count(), 2);
        // Proposer rotation skips jailed validator
        assert_eq!(vs.proposer_for_height(0), Some(vid(1)));
        assert_eq!(vs.proposer_for_height(1), Some(vid(3)));
    }
}
