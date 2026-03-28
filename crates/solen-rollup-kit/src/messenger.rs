//! Cross-domain messenger: sends and receives messages between domains.

use solen_types::{Hash, RollupId};

pub struct MessageReceipt {
    pub source: RollupId,
    pub destination: RollupId,
    pub nonce: u64,
    pub payload_hash: Hash,
}
