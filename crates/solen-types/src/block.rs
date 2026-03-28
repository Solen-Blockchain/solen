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
}
