//! Fuzz target: P2P network message decoding.
//!
//! Security properties tested:
//! - No panic on any input (raw or compressed)
//! - Decompression bomb protection (16 MB limit)
//! - Malformed deflate streams handled gracefully
//! - Invalid JSON after decompression handled gracefully
//!
//! Likely failure modes:
//! - Partial decompression producing invalid JSON
//! - Decompression of crafted data hitting edge cases in flate2
//! - Oversized decompressed output

#![no_main]

use libfuzzer_sys::fuzz_target;
use solen_p2p::messages::NetworkMessage;

fuzz_target!(|data: &[u8]| {
    // Limit input to 1 MB to keep fuzzing fast.
    if data.len() > 1_000_000 {
        return;
    }

    // Raw decode — must never panic.
    let _ = NetworkMessage::decode(data);

    // Also try with compression prefix.
    if !data.is_empty() {
        let mut compressed = vec![0x01]; // compression marker
        compressed.extend_from_slice(data);
        let _ = NetworkMessage::decode(&compressed);
    }

    // Try with just the compression marker and no data.
    let _ = NetworkMessage::decode(&[0x01]);
    let _ = NetworkMessage::decode(&[0x01, 0x00]);
    let _ = NetworkMessage::decode(&[]);
});
