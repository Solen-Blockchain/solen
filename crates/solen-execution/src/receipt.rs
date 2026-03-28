//! Transaction execution receipts.

use serde::{Deserialize, Serialize};
use solen_types::AccountId;

/// Outcome of executing a single user operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub sender: AccountId,
    pub nonce: u64,
    pub success: bool,
    pub gas_used: u64,
    pub error: Option<String>,
    pub events: Vec<Event>,
}

/// An event emitted during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub emitter: AccountId,
    pub topic: Vec<u8>,
    pub data: Vec<u8>,
}

/// Result of executing a full block.
#[derive(Debug, Clone)]
pub struct BlockResult {
    pub state_root: [u8; 32],
    pub receipts: Vec<ExecutionReceipt>,
    pub gas_used: u64,
}
