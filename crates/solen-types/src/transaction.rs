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

impl UserOperation {
    /// Bytes that get signed for this operation. Single source of truth — every
    /// signer and verifier must call this. Format is consensus-critical; any
    /// change is a hard fork.
    ///
    /// Layout: chain_id[8 LE] ‖ sender[32] ‖ nonce[8 LE] ‖ max_fee[16 LE] ‖
    ///         blake3(serde_json(actions))[32]  = 96 bytes
    pub fn signing_message(&self, chain_id: u64) -> Vec<u8> {
        let mut msg = Vec::with_capacity(96);
        msg.extend_from_slice(&chain_id.to_le_bytes());
        msg.extend_from_slice(&self.sender);
        msg.extend_from_slice(&self.nonce.to_le_bytes());
        msg.extend_from_slice(&self.max_fee.to_le_bytes());
        let actions_bytes = serde_json::to_vec(&self.actions).unwrap_or_default();
        msg.extend_from_slice(blake3::hash(&actions_bytes).as_bytes());
        msg
    }
}

/// A single action within a user operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Action {
    Transfer { to: AccountId, amount: u128 },
    Call { target: AccountId, method: String, args: Vec<u8> },
    Deploy { code: Vec<u8>, salt: [u8; 32] },
    /// Replace the account's auth methods. Must be signed by a current auth method.
    SetAuth { auth_methods: Vec<crate::account::AuthMethod> },
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
