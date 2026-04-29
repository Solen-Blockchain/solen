//! Cryptographic primitives: hashing, signing, verification.

pub mod hash;
pub mod signing;

pub use hash::{blake3_hash, receipt_tx_hash};
pub use signing::{verify, Keypair, SigningError};

/// Fill a buffer with OS-provided cryptographically secure random bytes.
pub fn random_bytes(buf: &mut [u8]) {
    use rand::RngCore;
    rand::thread_rng().fill_bytes(buf);
}
