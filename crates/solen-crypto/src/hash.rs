//! Hashing utilities.

use solen_types::Hash;

/// Compute a BLAKE3 hash of the input.
pub fn blake3_hash(data: &[u8]) -> Hash {
    *blake3::hash(data).as_bytes()
}

/// Canonical per-receipt transaction hash.
///
/// Layout: `block_height_le[8] ‖ tx_index_le[4] ‖ sender[32] ‖ nonce_le[8]`.
/// Includes block_height + tx_index so system-emitted receipts (where
/// (sender, nonce) is constant, e.g. epoch rewards) hash to distinct values.
pub fn receipt_tx_hash(block_height: u64, tx_index: u32, sender: &[u8; 32], nonce: u64) -> Hash {
    let mut buf = [0u8; 8 + 4 + 32 + 8];
    buf[..8].copy_from_slice(&block_height.to_le_bytes());
    buf[8..12].copy_from_slice(&tx_index.to_le_bytes());
    buf[12..44].copy_from_slice(sender);
    buf[44..52].copy_from_slice(&nonce.to_le_bytes());
    blake3_hash(&buf)
}
