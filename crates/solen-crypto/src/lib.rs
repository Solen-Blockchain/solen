//! Cryptographic primitives: hashing, signing, verification.

pub mod hash;
pub mod pq;
pub mod signing;

pub use hash::{blake3_hash, receipt_tx_hash};
pub use pq::{verify_ml_dsa, MlDsaKeypair, ML_DSA_PK_LEN, ML_DSA_SIG_LEN};
pub use signing::{verify, Keypair, SigningError};

/// Fill a buffer with OS-provided cryptographically secure random bytes.
pub fn random_bytes(buf: &mut [u8]) {
    use rand::RngCore;
    rand::thread_rng().fill_bytes(buf);
}
