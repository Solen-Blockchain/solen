//! Smart account types.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

use crate::{AccountId, Hash};

/// Authentication method for a smart account.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum AuthMethod {
    Passkey { credential_id: Vec<u8> },
    Ed25519 { public_key: [u8; 32] },
    Threshold { signers: Vec<[u8; 32]>, threshold: u16 },
    Guardian { guardian_id: AccountId },
}

/// A smart account (no EOAs in Solen).
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Account {
    pub id: AccountId,
    pub code_hash: Hash,
    pub auth_methods: Vec<AuthMethod>,
    pub nonce: u64,
    pub balance: u128,
}
