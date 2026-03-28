//! Cryptographic type aliases and marker types.

use serde::{Deserialize, Serialize};

/// A signature with its scheme identifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Signature {
    Ed25519(Vec<u8>),
    Passkey {
        authenticator_data: Vec<u8>,
        client_data: Vec<u8>,
        signature: Vec<u8>,
    },
}

/// A zero-knowledge proof blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZkProof {
    pub proof_type: String,
    pub data: Vec<u8>,
}
