//! Transaction and intent types.

use serde::{Deserialize, Serialize};

use crate::AccountId;

/// A user operation submitted to the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserOperation {
    pub sender: AccountId,
    pub nonce: u64,
    pub actions: Vec<Action>,
    pub max_fee: u128,
    pub signature: Vec<u8>,
}

/// A single action within a user operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Action {
    Transfer { to: AccountId, amount: u128 },
    Call { target: AccountId, method: String, args: Vec<u8> },
    Deploy { code: Vec<u8>, salt: [u8; 32] },
}

/// An intent expressing desired outcome rather than exact execution steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intent {
    pub sender: AccountId,
    pub constraints: Vec<u8>,
    pub max_fee: u128,
    pub expiry_height: u64,
    pub signature: Vec<u8>,
}
