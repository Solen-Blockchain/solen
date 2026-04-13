//! Node events emitted by the consensus engine for external consumers
//! (WebSocket subscriptions, indexers, etc.).

use serde::{Deserialize, Serialize};

/// Events broadcast by the consensus engine when state changes occur.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeEvent {
    /// A new block was finalized (single-validator or quorum).
    BlockFinalized {
        height: u64,
        epoch: u64,
        block_hash: [u8; 32],
        state_root: [u8; 32],
        proposer: [u8; 32],
        timestamp_ms: u64,
        tx_count: usize,
        gas_used: u64,
    },
    /// A transaction was included in a finalized block.
    TxIncluded {
        block_height: u64,
        tx_hash: [u8; 32],
        sender: [u8; 32],
        nonce: u64,
        success: bool,
        gas_used: u64,
    },
    /// The active validator set changed at an epoch boundary.
    ValidatorSetChanged {
        epoch: u64,
        active_count: usize,
    },
}
