//! Fuzz target: deserialize random bytes as UserOperation.
//!
//! Ensures serde deserialization doesn't panic on malformed input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use solen_types::transaction::UserOperation;

fuzz_target!(|data: &[u8]| {
    // Attempt JSON deserialization — must not panic.
    let _ = serde_json::from_slice::<UserOperation>(data);

    // Also try deserializing as individual action types.
    let _ = serde_json::from_slice::<solen_types::transaction::Action>(data);
    let _ = serde_json::from_slice::<solen_types::transaction::Intent>(data);
});
