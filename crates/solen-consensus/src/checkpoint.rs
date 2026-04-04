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

use solen_types::ValidatorId;

// ── Finalized Checkpoints (dynamic, consensus-agreed) ────────

/// A finalized checkpoint — agreed upon by 2/3+ of validators.
/// Once finalized, all blocks at or before this height are irreversible.
/// This prevents long-range attacks where an attacker forks from deep history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizedCheckpoint {
    pub height: BlockHeight,
    pub epoch: Epoch,
    pub block_hash: Hash,
    pub state_root: Hash,
    /// Validator signatures attesting to this checkpoint.
    pub attestations: Vec<(ValidatorId, Vec<u8>)>,
}

/// Manages pending and finalized checkpoints.
#[derive(Debug, Clone, Default)]
pub struct FinalizedCheckpointStore {
    /// The latest finalized checkpoint (has 2/3+ quorum).
    pub latest: Option<FinalizedCheckpoint>,
    /// Pending checkpoint being collected (not yet quorum).
    pub pending: Option<PendingCheckpoint>,
    /// History of finalized checkpoint heights for validation.
    pub finalized_heights: Vec<u64>,
}

/// A checkpoint in the process of collecting attestations.
#[derive(Debug, Clone)]
pub struct PendingCheckpoint {
    pub height: BlockHeight,
    pub epoch: Epoch,
    pub block_hash: Hash,
    pub state_root: Hash,
    pub attestations: Vec<(ValidatorId, Vec<u8>)>,
    pub created_at: u64, // timestamp_ms
}

impl FinalizedCheckpointStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start collecting attestations for a new checkpoint at an epoch boundary.
    pub fn propose_checkpoint(
        &mut self,
        height: BlockHeight,
        epoch: Epoch,
        block_hash: Hash,
        state_root: Hash,
    ) {
        self.pending = Some(PendingCheckpoint {
            height,
            epoch,
            block_hash,
            state_root,
            attestations: Vec::new(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        });
    }

    /// Add a validator's attestation to the pending checkpoint.
    /// Returns true if this attestation caused the checkpoint to be finalized.
    pub fn add_attestation(
        &mut self,
        validator_id: ValidatorId,
        signature: Vec<u8>,
        validator_set: &crate::validator::ValidatorSet,
    ) -> bool {
        let pending = match &mut self.pending {
            Some(p) => p,
            None => return false,
        };

        // Don't accept duplicate attestations.
        if pending.attestations.iter().any(|(v, _)| *v == validator_id) {
            return false;
        }

        pending.attestations.push((validator_id, signature));

        // Check if we have quorum.
        let attester_ids: Vec<ValidatorId> = pending
            .attestations
            .iter()
            .map(|(v, _)| *v)
            .collect();

        if validator_set.has_quorum(&attester_ids) {
            // Finalize the checkpoint.
            let finalized = FinalizedCheckpoint {
                height: pending.height,
                epoch: pending.epoch,
                block_hash: pending.block_hash,
                state_root: pending.state_root,
                attestations: pending.attestations.clone(),
            };
            self.finalized_heights.push(finalized.height);
            // Keep only last 100 finalized heights.
            if self.finalized_heights.len() > 100 {
                self.finalized_heights.remove(0);
            }
            self.latest = Some(finalized);
            self.pending = None;
            return true;
        }

        false
    }

    /// Check if a block conflicts with a finalized checkpoint.
    /// Returns the expected hash if there's a conflict.
    pub fn validate_block(&self, height: u64, block_hash: &Hash) -> Option<Hash> {
        if let Some(ref cp) = self.latest {
            // Any block at or before the finalized height must be on the same chain.
            if height == cp.height && *block_hash != cp.block_hash {
                return Some(cp.block_hash);
            }
        }
        None
    }

    /// Check if a height has been finalized.
    pub fn is_finalized(&self, height: u64) -> bool {
        if let Some(ref cp) = self.latest {
            height <= cp.height
        } else {
            false
        }
    }

    /// Persist to store.
    pub fn save(&self, store: &mut dyn solen_storage::StateStore) {
        if let Ok(data) = serde_json::to_vec(&self.latest) {
            let _ = store.put(b"__finalized_checkpoint__", &data);
        }
    }

    /// Load from store.
    pub fn load(store: &dyn solen_storage::StateStore) -> Self {
        let latest = store.get(b"__finalized_checkpoint__")
            .ok()
            .flatten()
            .and_then(|data| serde_json::from_slice(&data).ok());
        let finalized_heights = latest.as_ref()
            .map(|cp: &FinalizedCheckpoint| vec![cp.height])
            .unwrap_or_default();
        Self {
            latest,
            pending: None,
            finalized_heights,
        }
    }

    /// The checkpoint signing message: blake3(height || block_hash || state_root).
    pub fn signing_message(height: u64, block_hash: &Hash, state_root: &Hash) -> Vec<u8> {
        let mut msg = Vec::with_capacity(72);
        msg.extend_from_slice(&height.to_le_bytes());
        msg.extend_from_slice(block_hash);
        msg.extend_from_slice(state_root);
        solen_crypto::blake3_hash(&msg).to_vec()
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

    // ── Finalized checkpoint tests ───────────────────────────

    fn vid(n: u8) -> ValidatorId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn finalized_checkpoint_requires_quorum() {
        use crate::validator::{ValidatorInfo, ValidatorSet};

        let vs = ValidatorSet::new(vec![
            ValidatorInfo::new(vid(1), 100),
            ValidatorInfo::new(vid(2), 100),
            ValidatorInfo::new(vid(3), 100),
        ]);

        let mut store = FinalizedCheckpointStore::new();
        store.propose_checkpoint(100, 1, [0xAA; 32], [0xBB; 32]);

        // 1 of 3 — not quorum.
        let finalized = store.add_attestation(vid(1), vec![0; 64], &vs);
        assert!(!finalized);
        assert!(store.latest.is_none());

        // 2 of 3 — still not quorum (need > 2/3).
        let finalized = store.add_attestation(vid(2), vec![0; 64], &vs);
        assert!(!finalized);

        // 3 of 3 — quorum.
        let finalized = store.add_attestation(vid(3), vec![0; 64], &vs);
        assert!(finalized);
        assert!(store.latest.is_some());
        assert_eq!(store.latest.as_ref().unwrap().height, 100);
    }

    #[test]
    fn finalized_checkpoint_rejects_duplicate_attestation() {
        use crate::validator::{ValidatorInfo, ValidatorSet};

        let vs = ValidatorSet::new(vec![
            ValidatorInfo::new(vid(1), 100),
            ValidatorInfo::new(vid(2), 100),
            ValidatorInfo::new(vid(3), 100),
        ]);

        let mut store = FinalizedCheckpointStore::new();
        store.propose_checkpoint(100, 1, [0xAA; 32], [0xBB; 32]);

        store.add_attestation(vid(1), vec![0; 64], &vs);
        // Duplicate from vid(1) — should be ignored.
        let finalized = store.add_attestation(vid(1), vec![0; 64], &vs);
        assert!(!finalized);
    }

    #[test]
    fn finalized_checkpoint_blocks_conflicting_blocks() {
        use crate::validator::{ValidatorInfo, ValidatorSet};

        let vs = ValidatorSet::new(vec![
            ValidatorInfo::new(vid(1), 100),
            ValidatorInfo::new(vid(2), 100),
            ValidatorInfo::new(vid(3), 100),
        ]);

        let mut store = FinalizedCheckpointStore::new();
        store.propose_checkpoint(100, 1, [0xAA; 32], [0xBB; 32]);

        // Finalize.
        store.add_attestation(vid(1), vec![0; 64], &vs);
        store.add_attestation(vid(2), vec![0; 64], &vs);
        store.add_attestation(vid(3), vec![0; 64], &vs);

        // Block at height 100 with matching hash — OK.
        assert!(store.validate_block(100, &[0xAA; 32]).is_none());

        // Block at height 100 with different hash — conflict.
        assert!(store.validate_block(100, &[0xFF; 32]).is_some());

        // Block at height 50 — before checkpoint, finalized.
        assert!(store.is_finalized(50));
        assert!(store.is_finalized(100));
        assert!(!store.is_finalized(101));
    }

    #[test]
    fn signing_message_is_deterministic() {
        let msg1 = FinalizedCheckpointStore::signing_message(100, &[0xAA; 32], &[0xBB; 32]);
        let msg2 = FinalizedCheckpointStore::signing_message(100, &[0xAA; 32], &[0xBB; 32]);
        assert_eq!(msg1, msg2);

        // Different inputs produce different messages.
        let msg3 = FinalizedCheckpointStore::signing_message(101, &[0xAA; 32], &[0xBB; 32]);
        assert_ne!(msg1, msg3);
    }
}
