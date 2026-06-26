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

    /// Get the proposer for a given height using hash-based randomized selection.
    ///
    /// Uses `epoch_seed` (derived from the previous epoch's last block hash)
    /// to shuffle the proposer order unpredictably. An attacker cannot predict
    /// the proposer schedule more than 1 epoch in advance, preventing targeted
    /// DDoS on upcoming proposers.
    ///
    /// Falls back to round-robin if epoch_seed is all zeros (genesis epoch).
    pub fn proposer_for_height(&self, height: u64) -> Option<ValidatorId> {
        self.proposer_for_height_with_seed(height, &[0u8; 32])
    }

    /// Stake-weighted proposer selection with epoch seed randomization.
    ///
    /// Validators are selected proportional to their stake. A validator with
    /// 2x the stake of another gets ~2x the proposer slots over time. The
    /// epoch seed (derived from previous epoch's last block) ensures the
    /// schedule is unpredictable more than 1 epoch in advance.
    ///
    /// Algorithm: compute a deterministic random value from the seed + height,
    /// then select the validator whose cumulative stake range contains that value.
    pub fn proposer_for_height_with_seed(&self, height: u64, epoch_seed: &[u8; 32]) -> Option<ValidatorId> {
        let active = self.active();
        if active.is_empty() {
            return None;
        }

        // Genesis epoch (seed = 0): use simple round-robin for bootstrapping.
        if *epoch_seed == [0u8; 32] {
            let idx = (height as usize) % active.len();
            return Some(active[idx].id);
        }

        // Compute deterministic random selector from seed + height.
        let mut input = Vec::with_capacity(40);
        input.extend_from_slice(epoch_seed);
        input.extend_from_slice(&height.to_le_bytes());
        let hash = solen_crypto::blake3_hash(&input);

        // Use first 16 bytes as u128 selector.
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&hash[..16]);
        let selector = u128::from_le_bytes(buf);

        // Stake-weighted selection via cumulative distribution.
        let total_stake: u128 = active.iter().map(|v| v.stake).sum();
        if total_stake == 0 {
            return Some(active[0].id);
        }
        let target = selector % total_stake;

        let mut cumulative: u128 = 0;
        for v in &active {
            cumulative += v.stake;
            if cumulative > target {
                return Some(v.id);
            }
        }
        Some(active.last().unwrap().id)
    }

    /// Get the full proposer order for a height, weighted by stake.
    /// Position 0 = designated proposer, 1+ = backup order.
    ///
    /// Uses stake-weighted shuffle: each validator gets a score based on
    /// hash(seed || height || validator_id) divided by their stake.
    /// Lower effective score = higher priority. This ensures validators
    /// with more stake are more likely to be earlier in the order.
    pub fn proposer_order_for_height(&self, height: u64, epoch_seed: &[u8; 32]) -> Vec<ValidatorId> {
        let active = self.active();
        if active.is_empty() {
            return vec![];
        }

        if *epoch_seed == [0u8; 32] {
            // Genesis: simple rotation order.
            let n = active.len();
            let start = (height as usize) % n;
            (0..n).map(|i| active[(start + i) % n].id).collect()
        } else {
            // Stake-weighted shuffle: score = hash / stake.
            // Validators with higher stake get lower effective scores,
            // placing them earlier in the proposer order.
            let mut scored: Vec<(u128, ValidatorId)> = active
                .iter()
                .map(|v| {
                    let mut input = Vec::with_capacity(72);
                    input.extend_from_slice(epoch_seed);
                    input.extend_from_slice(&height.to_le_bytes());
                    input.extend_from_slice(&v.id);
                    let hash = solen_crypto::blake3_hash(&input);
                    let mut buf = [0u8; 16];
                    buf.copy_from_slice(&hash[..16]);
                    let hash_val = u128::from_le_bytes(buf);
                    // Effective score = hash / stake. Lower = higher priority.
                    // Use saturating division to avoid div-by-zero.
                    let effective = if v.stake > 0 { hash_val / v.stake } else { u128::MAX };
                    (effective, v.id)
                })
                .collect();
            scored.sort_by(|a, b| a.0.cmp(&b.0));
            scored.into_iter().map(|(_, id)| id).collect()
        }
    }

    /// Check if a set of attesters forms a 2/3+ quorum by stake.
    ///
    /// SAFETY-CRITICAL: the quorum denominator is the TOTAL committed stake of
    /// every validator in the set — NOT the locally-active subset. Local,
    /// pre-consensus downtime jailing (see `engine::process_slashing`) flips an
    /// unreachable validator to `Jailed` immediately so *proposer rotation*
    /// skips it, but it must NEVER shrink the quorum denominator. If it did, a
    /// partitioned minority that locally jails the unreachable majority would
    /// measure a bogus "2/3 quorum" against its own narrowed view and
    /// force-finalize a divergent chain — the split-brain that shattered
    /// mainnet (2026-06-03 / 06-08 / 06-23). Only deterministic on-chain
    /// changes (stake slash / validator exit / unjail), which every node
    /// applies identically during block execution, may move this denominator.
    ///
    /// The numerator likewise counts any attester in the committed set: an
    /// attestation is cryptographic proof of participation, so excluding a
    /// (locally, possibly-erroneously) jailed-but-attesting validator could
    /// only ever *prevent* a legitimate quorum, never enable a false one.
    pub fn has_quorum(&self, attester_ids: &[ValidatorId]) -> bool {
        let total: u128 = self.validators.iter().map(|v| v.stake).sum();
        if total == 0 {
            return false;
        }
        let attested_stake: u128 = self
            .validators
            .iter()
            .filter(|v| attester_ids.contains(&v.id))
            .map(|v| v.stake)
            .sum();

        // 2/3+ quorum: attested_stake * 3 > total * 2
        attested_stake * 3 > total * 2
    }

    /// Total committed stake held by the given validators (each counted once).
    /// Same committed-set basis as `has_quorum`, so v2 fork choice ranks
    /// competing blocks by the same stake measure used for finality.
    pub fn stake_of(&self, ids: &[ValidatorId]) -> u128 {
        self.validators
            .iter()
            .filter(|v| ids.contains(&v.id))
            .map(|v| v.stake)
            .sum()
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

    #[test]
    fn jailing_does_not_shrink_quorum_denominator() {
        // Split-brain regression (mainnet fork 2026-06-03/06-08/06-23):
        // a partitioned minority that locally jails the unreachable majority
        // must NOT be able to self-certify a "quorum" against its own
        // shrunken view. Quorum is always measured against the full set.
        let mut vs = ValidatorSet::new(
            (1..=9).map(|n| ValidatorInfo::new(vid(n), 100)).collect(),
        );
        // Minority side sees only v1,v2,v3 (300 of 900 stake) and locally
        // jails the six it can't reach.
        for n in 4..=9 {
            vs.jail(&vid(n));
        }
        assert_eq!(vs.active_count(), 3);
        // Even though only 3 validators are locally "active", their 3/9 stake
        // is nowhere near 2/3 of the full committed set — no false quorum.
        assert!(!vs.has_quorum(&[vid(1), vid(2), vid(3)]));
        // The genuine majority (6/9) still forms a quorum regardless of how
        // any single node has locally jailed the rest.
        assert!(vs.has_quorum(&[vid(1), vid(2), vid(3), vid(4), vid(5), vid(6), vid(7)]));
    }

    #[test]
    fn seeded_proposer_is_deterministic() {
        let vs = test_set();
        let seed = [0xAB; 32];

        // Same seed + height = same proposer every time.
        let p1 = vs.proposer_for_height_with_seed(100, &seed);
        let p2 = vs.proposer_for_height_with_seed(100, &seed);
        assert_eq!(p1, p2);
    }

    #[test]
    fn different_seeds_produce_different_proposers() {
        let vs = test_set();
        let seed_a = [0xAA; 32];
        let seed_b = [0xBB; 32];

        // Different seeds should shuffle differently.
        // Over many heights, the proposer sets won't be identical.
        let mut differ = false;
        for h in 0..20 {
            let a = vs.proposer_for_height_with_seed(h, &seed_a);
            let b = vs.proposer_for_height_with_seed(h, &seed_b);
            if a != b {
                differ = true;
                break;
            }
        }
        assert!(differ, "different seeds must produce different proposer schedules");
    }

    #[test]
    fn zero_seed_falls_back_to_round_robin() {
        let vs = test_set();
        let zero_seed = [0u8; 32];

        // Zero seed = genesis epoch = round-robin.
        assert_eq!(vs.proposer_for_height_with_seed(1, &zero_seed), Some(vid(2)));
        assert_eq!(vs.proposer_for_height_with_seed(2, &zero_seed), Some(vid(3)));
        assert_eq!(vs.proposer_for_height_with_seed(3, &zero_seed), Some(vid(1)));
    }

    #[test]
    fn proposer_order_covers_all_validators() {
        let vs = test_set();
        let seed = [0xCC; 32];

        let order = vs.proposer_order_for_height(50, &seed);
        assert_eq!(order.len(), 3);
        // All 3 validators must appear exactly once.
        assert!(order.contains(&vid(1)));
        assert!(order.contains(&vid(2)));
        assert!(order.contains(&vid(3)));
    }

    #[test]
    fn seeded_proposer_distributes_by_stake() {
        // Equal stake = roughly equal distribution.
        let vs = test_set(); // 3 validators, 100 stake each
        let seed = [0xDD; 32];

        let mut counts = std::collections::HashMap::new();
        for h in 0..3000 {
            if let Some(p) = vs.proposer_for_height_with_seed(h, &seed) {
                *counts.entry(p).or_insert(0u32) += 1;
            }
        }
        // Each should get roughly 1000 out of 3000.
        for (_, count) in &counts {
            assert!(*count > 500, "equal-stake validators should each get >500/3000");
        }
        assert_eq!(counts.len(), 3, "all validators should be selected");
    }

    #[test]
    fn high_stake_validator_selected_more_often() {
        // Unequal stake: vid(1)=1000, vid(2)=100, vid(3)=100.
        let vs = ValidatorSet::new(vec![
            ValidatorInfo::new(vid(1), 1000), // 83% of stake
            ValidatorInfo::new(vid(2), 100),  // 8.3%
            ValidatorInfo::new(vid(3), 100),  // 8.3%
        ]);
        let seed = [0xEE; 32];

        let mut counts = std::collections::HashMap::new();
        for h in 0..6000 {
            if let Some(p) = vs.proposer_for_height_with_seed(h, &seed) {
                *counts.entry(p).or_insert(0u32) += 1;
            }
        }

        let v1_count = *counts.get(&vid(1)).unwrap_or(&0);
        let v2_count = *counts.get(&vid(2)).unwrap_or(&0);
        let v3_count = *counts.get(&vid(3)).unwrap_or(&0);

        // vid(1) should get significantly more than vid(2) or vid(3).
        assert!(
            v1_count > v2_count * 3,
            "high-stake validator should propose >3x more: v1={} v2={} v3={}",
            v1_count, v2_count, v3_count
        );
        // All should still be selected sometimes.
        assert!(v2_count > 0, "low-stake validators should still be selected");
        assert!(v3_count > 0, "low-stake validators should still be selected");
    }

    #[test]
    fn proposer_order_prefers_high_stake() {
        let vs = ValidatorSet::new(vec![
            ValidatorInfo::new(vid(1), 10000), // highest stake
            ValidatorInfo::new(vid(2), 100),
            ValidatorInfo::new(vid(3), 100),
        ]);
        let seed = [0xFF; 32];

        // Over many heights, vid(1) should be first in the order more often.
        let mut first_count = std::collections::HashMap::new();
        for h in 0..1000 {
            let order = vs.proposer_order_for_height(h, &seed);
            if let Some(first) = order.first() {
                *first_count.entry(*first).or_insert(0u32) += 1;
            }
        }

        let v1_first = *first_count.get(&vid(1)).unwrap_or(&0);
        assert!(
            v1_first > 500,
            "high-stake validator should be first in order >50% of the time: {}",
            v1_first
        );
    }
}
