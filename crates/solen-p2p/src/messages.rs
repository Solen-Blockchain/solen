//! Network message types for gossip topics.

use serde::{Deserialize, Serialize};
use solen_types::block::BlockHeader;
use solen_types::transaction::UserOperation;
use solen_types::{Hash, ValidatorId};

/// Build network-specific gossip topic names.
/// This ensures testnet, devnet, and mainnet nodes don't interfere.
pub fn topic_blocks(chain_id: u64) -> String { format!("solen/{}/blocks/1", chain_id) }
pub fn topic_transactions(chain_id: u64) -> String { format!("solen/{}/transactions/1", chain_id) }
pub fn topic_attestations(chain_id: u64) -> String { format!("solen/{}/attestations/1", chain_id) }
pub fn topic_sync(chain_id: u64) -> String { format!("solen/{}/sync/1", chain_id) }

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
    /// Returns the network-specific gossip topic for this message type.
    pub fn topic_for_chain(&self, chain_id: u64) -> String {
        match self {
            NetworkMessage::NewBlock { .. } => topic_blocks(chain_id),
            NetworkMessage::NewTransaction(_) => topic_transactions(chain_id),
            NetworkMessage::Attestation { .. } => topic_attestations(chain_id),
            NetworkMessage::SyncRequest { .. } => topic_sync(chain_id),
            NetworkMessage::SyncBlocks { .. } => topic_sync(chain_id),
            NetworkMessage::StatusAnnounce { .. } => topic_sync(chain_id),
        }
    }

    /// Serialize to compressed JSON bytes for gossip.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let json = serde_json::to_vec(self).ok()?;
        // Prefix with 0x01 to indicate compressed format.
        let mut compressed = vec![0x01];
        let mut encoder = flate2::write::DeflateEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        );
        std::io::Write::write_all(&mut encoder, &json).ok()?;
        compressed.extend(encoder.finish().ok()?);
        Some(compressed)
    }

    /// Deserialize from gossip bytes (supports both compressed and raw JSON).
    pub fn decode(data: &[u8]) -> Result<Self, serde_json::Error> {
        if data.first() == Some(&0x01) {
            // Compressed format.
            let mut decoder = flate2::read::DeflateDecoder::new(&data[1..]);
            let mut json = Vec::new();
            if std::io::Read::read_to_end(&mut decoder, &mut json).is_ok() {
                return serde_json::from_slice(&json);
            }
        }
        // Fall back to raw JSON (backwards compatible with old nodes).
        serde_json::from_slice(data)
    }
}
