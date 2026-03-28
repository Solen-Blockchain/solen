//! Cryptographic primitives: hashing, signing, verification.

pub mod hash;
pub mod signing;

pub use hash::blake3_hash;
pub use signing::{verify, Keypair, SigningError};
