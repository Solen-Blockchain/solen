//! Checkpoint sync: periodic state snapshots for fast node startup.
//!
//! Instead of replaying all blocks from genesis, a new node can load
//! the latest checkpoint and only replay blocks after that point.

use serde::{Deserialize, Serialize};
use solen_types::{BlockHeight, Epoch, Hash};

/// Interval between checkpoints (in blocks).
pub const CHECKPOINT_INTERVAL: u64 = 100;

/// A state checkpoint containing metadata needed to resume from a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Block height at which this checkpoint was taken.
    pub height: BlockHeight,
    /// Epoch at this height.
    pub epoch: Epoch,
    /// State root hash at this height.
    pub state_root: Hash,
    /// Hash of the block at this height.
    pub block_hash: Hash,
    /// Serialized validator set at this height.
    pub validator_set_hash: Hash,
    /// Timestamp when checkpoint was created.
    pub timestamp_ms: u64,
}

impl Checkpoint {
    /// Check if a given height should produce a checkpoint.
    pub fn should_checkpoint(height: BlockHeight) -> bool {
        height > 0 && height % CHECKPOINT_INTERVAL == 0
    }
}

/// Manages a list of checkpoints.
#[derive(Debug, Clone, Default)]
pub struct CheckpointStore {
    checkpoints: Vec<Checkpoint>,
    max_checkpoints: usize,
}

impl CheckpointStore {
    pub fn new(max_checkpoints: usize) -> Self {
        Self {
            checkpoints: Vec::new(),
            max_checkpoints,
        }
    }

    /// Add a new checkpoint, evicting the oldest if at capacity.
    pub fn add(&mut self, checkpoint: Checkpoint) {
        if self.checkpoints.len() >= self.max_checkpoints {
            self.checkpoints.remove(0);
        }
        self.checkpoints.push(checkpoint);
    }

    /// Get the latest checkpoint.
    pub fn latest(&self) -> Option<&Checkpoint> {
        self.checkpoints.last()
    }

    /// Get a checkpoint at or before a given height.
    pub fn at_or_before(&self, height: BlockHeight) -> Option<&Checkpoint> {
        self.checkpoints
            .iter()
            .rev()
            .find(|c| c.height <= height)
    }

    /// Get all checkpoints.
    pub fn all(&self) -> &[Checkpoint] {
        &self.checkpoints
    }

    /// Serialize checkpoints to JSON for persistence.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(&self.checkpoints).unwrap_or_default()
    }

    /// Load checkpoints from JSON.
    pub fn from_json(data: &[u8], max_checkpoints: usize) -> Self {
        let checkpoints: Vec<Checkpoint> =
            serde_json::from_slice(data).unwrap_or_default();
        Self {
            checkpoints,
            max_checkpoints,
        }
    }
}

use serde_json;

/// Hardcoded trusted checkpoints — verified block hashes at known heights.
/// Nodes reject any chain that doesn't pass through these checkpoints.
/// This prevents long-range attacks where an attacker builds an alternate
/// chain from genesis using old validator keys.
///
/// Add new entries periodically as the chain progresses.
pub struct TrustedCheckpoints {
    entries: Vec<(u64, Hash)>, // (height, block_hash)
}

impl TrustedCheckpoints {
    /// Testnet trusted checkpoints.
    pub fn testnet() -> Self {
        Self {
            entries: vec![
                // Add checkpoints as chain progresses:
                // (10_000, hex_to_hash("abcd...")),
            ],
        }
    }

    /// Mainnet trusted checkpoints.
    pub fn mainnet() -> Self {
        Self {
            entries: vec![
                // Populated before mainnet launch.
            ],
        }
    }

    /// Devnet has no checkpoints (resets frequently).
    pub fn devnet() -> Self {
        Self { entries: vec![] }
    }

    /// Check if a block at a given height violates a trusted checkpoint.
    /// Returns `Some(expected_hash)` if the height matches a checkpoint
    /// but the hash doesn't match (indicating an invalid chain).
    pub fn validate(&self, height: u64, block_hash: &Hash) -> Option<Hash> {
        for (cp_height, cp_hash) in &self.entries {
            if height == *cp_height && block_hash != cp_hash {
                return Some(*cp_hash);
            }
        }
        None
    }

    /// Get the highest checkpoint height.
    pub fn highest(&self) -> Option<u64> {
        self.entries.last().map(|(h, _)| *h)
    }

    /// Check if we have any checkpoints.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_checkpoint(height: u64) -> Checkpoint {
        Checkpoint {
            height,
            epoch: height / 100,
            state_root: [height as u8; 32],
            block_hash: [0; 32],
            validator_set_hash: [0; 32],
            timestamp_ms: height * 1000,
        }
    }

    #[test]
    fn should_checkpoint() {
        assert!(!Checkpoint::should_checkpoint(0));
        assert!(!Checkpoint::should_checkpoint(1));
        assert!(!Checkpoint::should_checkpoint(99));
        assert!(Checkpoint::should_checkpoint(100));
        assert!(Checkpoint::should_checkpoint(200));
    }

    #[test]
    fn store_add_and_query() {
        let mut store = CheckpointStore::new(5);
        store.add(make_checkpoint(100));
        store.add(make_checkpoint(200));
        store.add(make_checkpoint(300));

        assert_eq!(store.latest().unwrap().height, 300);
        assert_eq!(store.at_or_before(250).unwrap().height, 200);
        assert_eq!(store.all().len(), 3);
    }

    #[test]
    fn evicts_oldest() {
        let mut store = CheckpointStore::new(2);
        store.add(make_checkpoint(100));
        store.add(make_checkpoint(200));
        store.add(make_checkpoint(300));

        assert_eq!(store.all().len(), 2);
        assert_eq!(store.all()[0].height, 200);
        assert_eq!(store.all()[1].height, 300);
    }

    #[test]
    fn serialize_roundtrip() {
        let mut store = CheckpointStore::new(10);
        store.add(make_checkpoint(100));
        store.add(make_checkpoint(200));

        let json = store.to_json();
        let loaded = CheckpointStore::from_json(&json, 10);

        assert_eq!(loaded.all().len(), 2);
        assert_eq!(loaded.latest().unwrap().height, 200);
    }
}
