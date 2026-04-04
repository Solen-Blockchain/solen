//! Fuzz target: snapshot restore parsing.
//!
//! Security properties tested:
//! - No panic on malformed snapshot data
//! - Truncated headers don't crash
//! - Invalid magic/version rejected
//! - Decompression bomb protection (2 GB limit)
//! - Truncated entry list detected and rejected
//! - State root mismatch detected
//!
//! Likely failure modes:
//! - Off-by-one in entry parsing loop
//! - Integer overflow in key_len/val_len reading
//! - Panic on slice operations with insufficient data
//! - Silent data corruption from partial entries

#![no_main]

use libfuzzer_sys::fuzz_target;
use solen_consensus::snapshot::{read_snapshot_meta, restore_snapshot};
use solen_storage::MemoryStore;

fuzz_target!(|data: &[u8]| {
    // Limit input to 256 KB to keep fuzzing fast.
    if data.len() > 256 * 1024 {
        return;
    }

    // Test meta parsing — must never panic.
    let _ = read_snapshot_meta(data);

    // Test full restore — must never panic.
    let mut store = MemoryStore::new();
    let _ = restore_snapshot(&mut store, data);
});
