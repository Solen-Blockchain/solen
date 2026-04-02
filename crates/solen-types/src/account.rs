//! Smart account types.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};

use crate::{AccountId, Hash};

/// Authentication method for a smart account.
#[derive(Debug, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub enum AuthMethod {
    /// WebAuthn/Passkey: P-256 (secp256r1) ECDSA signature verification.
    /// The signature field carries: authenticatorData || clientDataJSON || r[32] || s[32].
    /// The challenge in clientDataJSON must be base64url(signing_message).
    Passkey {
        credential_id: Vec<u8>,
        /// P-256 public key x-coordinate.
        public_key_x: [u8; 32],
        /// P-256 public key y-coordinate.
        public_key_y: [u8; 32],
    },
    Ed25519 { public_key: [u8; 32] },
    Threshold { signers: Vec<[u8; 32]>, threshold: u16 },
    Guardian { guardian_id: AccountId },
    /// Temporary session key with restrictions.
    Session {
        /// Ed25519 session key.
        session_key: [u8; 32],
        /// Block height after which this session expires.
        expires_at: u64,
        /// Maximum total spend allowed (base units). 0 = unlimited.
        spending_limit: u128,
        /// Allowed contract targets (empty = all allowed).
        allowed_targets: Vec<AccountId>,
        /// Allowed methods (empty = all allowed).
        allowed_methods: Vec<String>,
    },
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
