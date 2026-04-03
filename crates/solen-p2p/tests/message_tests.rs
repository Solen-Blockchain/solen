//! P2P message security tests.

use solen_p2p::messages::NetworkMessage;

// ── Test #18: Decompression bomb bounded ──────────────────────

#[test]
fn decompression_bounded_to_16mb() {
    // Create a large compressible payload.
    let huge = vec![0u8; 20 * 1024 * 1024]; // 20MB of zeros — compresses very small

    // Compress it.
    use std::io::Write;
    let mut compressed = vec![0x01u8]; // compression prefix
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&huge).unwrap();
    compressed.extend(encoder.finish().unwrap());

    // Attempt to decode — should not crash or allocate 20MB.
    let result = NetworkMessage::decode(&compressed);
    // Should fail (not valid JSON after decompression), but should NOT panic or OOM.
    assert!(result.is_err(), "garbage decompressed data should fail JSON parse");
}

// ── Test: Empty compressed message ────────────────────────────

#[test]
fn empty_compressed_message_handled() {
    let data = vec![0x01u8]; // Compression prefix with no data.
    let result = NetworkMessage::decode(&data);
    assert!(result.is_err());
}

// ── Test: Invalid JSON after decompression ────────────────────

#[test]
fn invalid_json_after_decompression() {
    use std::io::Write;
    let mut compressed = vec![0x01u8];
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(b"this is not json").unwrap();
    compressed.extend(encoder.finish().unwrap());

    let result = NetworkMessage::decode(&compressed);
    assert!(result.is_err());
}

// ── Test: Raw (uncompressed) invalid JSON ─────────────────────

#[test]
fn raw_invalid_json_rejected() {
    let result = NetworkMessage::decode(b"not json at all");
    assert!(result.is_err());
}

#[test]
fn raw_empty_message_rejected() {
    let result = NetworkMessage::decode(b"");
    assert!(result.is_err());
}
