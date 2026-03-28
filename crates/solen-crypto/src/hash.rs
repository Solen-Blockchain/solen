//! Hashing utilities.

use solen_types::Hash;

/// Compute a BLAKE3 hash of the input.
pub fn blake3_hash(data: &[u8]) -> Hash {
    *blake3::hash(data).as_bytes()
}
