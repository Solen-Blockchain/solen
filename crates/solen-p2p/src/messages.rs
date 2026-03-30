//! Network message types for gossip topics.

use serde::{Deserialize, Serialize};
use solen_types::block::BlockHeader;
use solen_types::transaction::UserOperation;
use solen_types::{Hash, ValidatorId};

/// Gossip topic names.
pub const TOPIC_BLOCKS: &str = "solen/blocks/1";
pub const TOPIC_TRANSACTIONS: &str = "solen/transactions/1";
pub const TOPIC_ATTESTATIONS: &str = "solen/attestations/1";
pub const TOPIC_SYNC: &str = "solen/sync/1";

/// Messages that can be sent over the gossip network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// A proposed block with its transactions for validators to verify.
    NewBlock {
        header: BlockHeader,
        operations: Vec<UserOperation>,
        tx_count: usize,
        gas_used: u64,
    },
    /// A new user operation for the mempool.
    NewTransaction(UserOperation),
    /// A validator's attestation of a block.
    Attestation {
        validator_id: ValidatorId,
        block_height: u64,
        block_hash: Hash,
        signature: Vec<u8>,
    },
    /// Request blocks for sync. Peer should respond with SyncBlocks.
    SyncRequest {
        from_height: u64,
        to_height: u64,
    },
    /// Response with historical blocks for sync.
    SyncBlocks {
        blocks: Vec<SyncBlock>,
    },
    /// Announce current height (for peers to know if they need to sync).
    StatusAnnounce {
        height: u64,
        state_root: Hash,
    },
}

/// A block sent during sync (header + receipts for indexing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncBlock {
    pub header: BlockHeader,
    pub operations: Vec<UserOperation>,
    /// Receipts from block execution, included so syncing nodes can index transactions.
    #[serde(default)]
    pub receipts: Vec<solen_execution::receipt::ExecutionReceipt>,
}

impl NetworkMessage {
    /// Returns the gossip topic for this message type.
    pub fn topic(&self) -> &'static str {
        match self {
            NetworkMessage::NewBlock { .. } => TOPIC_BLOCKS,
            NetworkMessage::NewTransaction(_) => TOPIC_TRANSACTIONS,
            NetworkMessage::Attestation { .. } => TOPIC_ATTESTATIONS,
            NetworkMessage::SyncRequest { .. } => TOPIC_SYNC,
            NetworkMessage::SyncBlocks { .. } => TOPIC_SYNC,
            NetworkMessage::StatusAnnounce { .. } => TOPIC_SYNC,
        }
    }

    /// Serialize to JSON bytes for gossip.
    pub fn encode(&self) -> Option<Vec<u8>> {
        serde_json::to_vec(self).ok()
    }

    /// Deserialize from JSON bytes.
    pub fn decode(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}
