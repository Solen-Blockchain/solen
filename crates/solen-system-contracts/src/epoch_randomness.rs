//! Epoch randomness accumulator (grind-resistant proposer seed, committed to state).
//!
//! The epoch proposer seed MUST be a deterministic function of AGREED on-chain
//! state — never of proposer/timestamp (that non-canonical dependence caused the
//! 2026-07-28 epoch-boundary mainnet halt: nodes agreed on `state_root` yet held
//! boundary blocks with different `block_hash`, so their in-memory seeds
//! diverged and they picked different proposers).
//!
//! This accumulator mixes an agreed per-block contribution (the parent
//! `state_root`) into a value held IN STATE, and snapshots it into `seed` at each
//! epoch boundary. Consensus reads `seed` for proposer selection. Because the
//! value lives in the state store it is:
//!   * identical on every honest node (it is part of `state_root`),
//!   * restart-safe (loaded from the store — no in-memory reset, no history
//!     lookback window that could differ across nodes),
//! which structurally removes both the original halt bug and the in-memory /
//! restart footguns of a plain "look back N blocks" seed.
//!
//! Grinding is diluted, not eliminated: a proposer still influences its own
//! block's contribution (via its txs -> state_root), but only ~1/EPOCH_LENGTH of
//! the epoch's randomness rather than all of it. Full unbiasability wants a VRF.
//!
//! Ships behind a dormant activation height (see `epoch_randomness_height`): it
//! only writes to state and is only read by consensus at/after the flag-day
//! height, so a deployed-but-dormant binary is byte-for-byte identical.

use serde::{Deserialize, Serialize};
use solen_crypto::blake3_hash;
use solen_storage::StateStore;
use solen_types::Hash;

/// Blocks per epoch. Mirrors the consensus `EPOCH_LENGTH`.
pub const EPOCH_LENGTH: u64 = 100;

/// Reserved state key holding the serialized [`EpochRandomness`].
const STORAGE_KEY: &[u8] = b"__sys_epoch_randomness__";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochRandomness {
    /// Running accumulator for the in-progress epoch.
    pub acc: Hash,
    /// Finalized seed for the current epoch — a snapshot of `acc` taken at the
    /// last epoch boundary. This is the value consensus reads for proposer
    /// selection.
    pub seed: Hash,
}

impl EpochRandomness {
    pub fn load(store: &dyn StateStore) -> Self {
        match store.get(STORAGE_KEY) {
            Ok(Some(data)) => serde_json::from_slice(&data).unwrap_or_default(),
            _ => Self::default(),
        }
    }

    pub fn save(&self, store: &mut dyn StateStore) {
        if let Ok(data) = serde_json::to_vec(self) {
            let _ = store.put(STORAGE_KEY, &data);
        }
    }

    /// Mix one block's AGREED `contribution` (e.g. the parent `state_root`) into
    /// the accumulator, and at an epoch boundary snapshot it into `seed`.
    ///
    /// `contribution` must be an agreed on-chain value identical on every node —
    /// never proposer/timestamp/block_hash.
    pub fn mix_block(&mut self, contribution: &Hash, height: u64) {
        let mut input = Vec::with_capacity(32 + 32 + 8);
        input.extend_from_slice(&self.acc);
        input.extend_from_slice(contribution);
        input.extend_from_slice(&height.to_le_bytes());
        self.acc = blake3_hash(&input);

        if height > 0 && height % EPOCH_LENGTH == 0 {
            // Snapshot this epoch's accumulated randomness as the seed the next
            // epoch's proposer selection will consume, then carry it forward so
            // the randomness chain stays continuous across epochs.
            self.seed = blake3_hash(&self.acc);
            self.acc = self.seed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solen_storage::MemoryStore;

    fn h(n: u8) -> Hash {
        let mut x = [0u8; 32];
        x[0] = n;
        x
    }

    #[test]
    fn mix_is_deterministic_for_same_inputs() {
        let mut a = EpochRandomness::default();
        let mut b = EpochRandomness::default();
        for height in 1..=250u64 {
            a.mix_block(&h((height % 7) as u8), height);
            b.mix_block(&h((height % 7) as u8), height);
        }
        assert_eq!(a, b, "same contributions must yield identical acc+seed");
    }

    #[test]
    fn boundary_snapshots_seed_and_carries_forward() {
        let mut er = EpochRandomness::default();
        // Below the first boundary, seed stays default; acc evolves.
        for height in 1..EPOCH_LENGTH {
            er.mix_block(&h(1), height);
        }
        assert_eq!(er.seed, [0u8; 32], "seed unset before the first boundary");
        let acc_before = er.acc;
        er.mix_block(&h(1), EPOCH_LENGTH); // boundary
        assert_ne!(er.seed, [0u8; 32], "boundary must set the seed");
        assert_ne!(er.acc, acc_before, "acc advances across the boundary");
        assert_eq!(er.acc, er.seed, "acc carries the seed forward");
    }

    #[test]
    fn seed_differs_when_contributions_differ() {
        let mut a = EpochRandomness::default();
        let mut b = EpochRandomness::default();
        for height in 1..=EPOCH_LENGTH {
            a.mix_block(&h(1), height);
            b.mix_block(&h(2), height); // one differing contribution stream
        }
        assert_ne!(a.seed, b.seed, "different histories -> different seed");
    }

    #[test]
    fn load_save_roundtrip() {
        let mut store = MemoryStore::new();
        let mut er = EpochRandomness::default();
        er.mix_block(&h(9), 100);
        er.save(&mut store);
        let loaded = EpochRandomness::load(&store);
        assert_eq!(er, loaded);
    }
}
