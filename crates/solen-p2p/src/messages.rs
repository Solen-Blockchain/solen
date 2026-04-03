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

    /// Serialize to bytes for gossip.
    /// SyncBlocks are compressed (large payloads). Everything else is raw JSON.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let json = serde_json::to_vec(self).ok()?;
        if matches!(self, NetworkMessage::SyncBlocks { .. }) && json.len() > 1024 {
            // Compress large sync messages. Prefix with 0x01.
            let mut compressed = vec![0x01];
            let mut encoder = flate2::write::DeflateEncoder::new(
                Vec::new(),
                flate2::Compression::fast(),
            );
            std::io::Write::write_all(&mut encoder, &json).ok()?;
            compressed.extend(encoder.finish().ok()?);
            Some(compressed)
        } else {
            Some(json)
        }
    }

    /// Maximum decompressed message size (16 MB). Prevents decompression bombs.
    const MAX_DECOMPRESSED_SIZE: usize = 16 * 1024 * 1024;

    /// Deserialize from gossip bytes (supports both compressed and raw JSON).
    pub fn decode(data: &[u8]) -> Result<Self, serde_json::Error> {
        if data.first() == Some(&0x01) {
            // Compressed format — decompress with size limit.
            let mut decoder = flate2::read::DeflateDecoder::new(&data[1..]);
            let mut json = Vec::new();
            // Read in chunks to enforce size limit.
            let mut buf = [0u8; 8192];
            loop {
                match std::io::Read::read(&mut decoder, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        json.extend_from_slice(&buf[..n]);
                        if json.len() > Self::MAX_DECOMPRESSED_SIZE {
                            return Err(serde_json::from_str::<()>("").unwrap_err()); // size exceeded
                        }
                    }
                    Err(_) => break,
                }
            }
            if !json.is_empty() {
                return serde_json::from_slice(&json);
            }
        }
        // Raw JSON (default for blocks, attestations, etc.).
        serde_json::from_slice(data)
    }
}
