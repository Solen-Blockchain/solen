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
    /// Which auth method was used to sign this operation (e.g. "ed25519", "passkey", "session", "threshold").
    #[serde(default = "default_auth_method")]
    pub auth_method: String,
}

fn default_auth_method() -> String {
    "ed25519".to_string()
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
