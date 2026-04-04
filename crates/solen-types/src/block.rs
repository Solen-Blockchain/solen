//! Block and header types.

use serde::{Deserialize, Serialize};

use crate::{BlockHeight, Epoch, Hash, ValidatorId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub height: BlockHeight,
    pub epoch: Epoch,
    pub parent_hash: Hash,
    pub state_root: Hash,
    pub transactions_root: Hash,
    pub receipts_root: Hash,
    pub proposer: ValidatorId,
    pub timestamp_ms: u64,
    /// Ed25519 signature over the header fields (excluding this field).
    /// Proves the proposer actually authored this block. Optional for
    /// backward compatibility during the rollout period.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposer_signature: Vec<u8>,
}
