//! Example counter contract for the Solen network.
//!
//! Maintains a counter in storage that can be incremented.
//! Emits an "incremented" event on each call with the new value.
//!
//! Build with:
//!   cargo build --target wasm32-unknown-unknown --release
//!
//! The compiled WASM will be at:
//!   target/wasm32-unknown-unknown/release/solen_example_counter.wasm

#![no_std]

use solen_contract_sdk::{events, sdk, storage};

#[no_mangle]
pub extern "C" fn call(_input_ptr: i32, _input_len: i32) -> i32 {
    // Read current counter value (defaults to 0).
    let count = storage::get_u64(b"count").unwrap_or(0);

    // Increment.
    let new_count = count + 1;
    storage::set_u64(b"count", new_count);

    // Emit event.
    events::emit(b"incremented", &new_count.to_le_bytes());

    // Return the new count.
    sdk::return_value(&new_count.to_le_bytes())
}
