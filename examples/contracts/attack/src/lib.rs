//! Adversarial contracts for stress testing the Solen VM.
//!
//! Each method name triggers a different attack pattern.
//! Deploy once, call with different method names to test each vector.

#![no_std]

use solen_contract_sdk::{events, sdk, storage};

#[no_mangle]
pub extern "C" fn call(input_ptr: i32, input_len: i32) -> i32 {
    let input = sdk::read_input(input_ptr, input_len);

    // Parse method name (everything before the first \0).
    let method_end = input.iter().position(|&b| b == 0).unwrap_or(input.len());
    let method = &input[..method_end];

    match method {
        // Attack 1: Infinite loop — should be stopped by fuel metering.
        b"infinite_loop" => {
            let mut i: u64 = 0;
            loop {
                i = i.wrapping_add(1);
                // Prevent the compiler from optimizing this away.
                if i == 0 { storage::set_u64(b"x", i); }
            }
        }

        // Attack 2: Storage bomb — write as many large values as possible.
        // Each write costs 2000 + (key_len + val_len) * 10 fuel.
        // With 1M fuel: ~2000 + (8 + 4096) * 10 = 43,040 per write → ~23 writes.
        b"storage_bomb" => {
            let data = [0xFFu8; 4096]; // 4KB per write
            let mut i: u64 = 0;
            loop {
                let mut key = [0u8; 12];
                key[..4].copy_from_slice(b"bomb");
                key[4..12].copy_from_slice(&i.to_le_bytes());
                storage::set(&key, &data);
                i += 1;
            }
        }

        // Attack 3: Storage bomb with tiny keys, maximum entries.
        // Try to create as many storage keys as possible.
        b"key_flood" => {
            let mut i: u64 = 0;
            loop {
                let mut key = [0u8; 10];
                key[..2].copy_from_slice(b"k/");
                key[2..10].copy_from_slice(&i.to_le_bytes());
                storage::set(&key, &[1u8]);
                i += 1;
            }
        }

        // Attack 4: Event flood — emit as many events as possible.
        // Each event costs 2000 + (topic_len + data_len) * 10.
        // With small payloads: 2000 + (4 + 8) * 10 = 2120 per event → ~471 events.
        b"event_flood" => {
            let mut i: u64 = 0;
            loop {
                events::emit(b"spam", &i.to_le_bytes());
                i += 1;
            }
        }

        // Attack 5: Event flood with large payloads.
        // 2000 + (4 + 1024) * 10 = 12,280 per event → ~81 events.
        b"event_bomb" => {
            let big_data = [0xABu8; 1024];
            let mut i: u64 = 0;
            loop {
                events::emit(b"boom", &big_data);
                i += 1;
            }
        }

        // Attack 6: Storage read flood — try to overwhelm host with reads.
        // Each read costs 500 + key_len * 10.
        // With 8-byte key: 580 per read → ~1724 reads.
        b"read_flood" => {
            let mut i: u64 = 0;
            loop {
                let _ = storage::get_u64(b"nonexist");
                i += 1;
            }
        }

        // Attack 7: Large return data — try to return a huge response.
        b"big_return" => {
            let data = [0xCDu8; 4096];
            return sdk::return_value(&data);
        }

        // Attack 8: Native transfer drain — try to send 50+ transfers.
        b"transfer_drain" => {
            let recipient = [1u8; 32];
            let mut i = 0;
            loop {
                if !sdk::transfer(&recipient, 1) {
                    // Transfer limit hit — record how many succeeded.
                    storage::set_u64(b"transfers", i);
                    break;
                }
                i += 1;
            }
            return sdk::return_value(&i.to_le_bytes());
        }

        // Attack 9: Deeply nested storage keys — test key length limits.
        b"long_keys" => {
            // Create a 1000-byte key.
            let mut key = [b'A'; 1000];
            key[0] = b'L';
            storage::set(&key, b"long key value");
            let val = storage::get(&key);
            if val.is_some() {
                storage::set_u64(b"long_key_ok", 1);
            }
            return 0;
        }

        // Attack 10: Overflow u128 balance — try to mint/transfer max amounts.
        // This calls transfer_native with u128::MAX.
        b"overflow_transfer" => {
            let recipient = [2u8; 32];
            let max_amount: u128 = u128::MAX;
            let success = sdk::transfer(&recipient, max_amount);
            storage::set_u64(b"overflow_ok", if success { 1 } else { 0 });
            return 0;
        }

        // Attack 11: Stack overflow — deep recursion.
        b"stack_overflow" => {
            fn recurse(depth: u64) -> u64 {
                if depth == 0 { return 1; }
                recurse(depth - 1) + 1
            }
            let result = recurse(1_000_000);
            storage::set_u64(b"depth", result);
            return 0;
        }

        // Attack 12: Write then panic — test rollback.
        b"write_and_panic" => {
            storage::set_u64(b"before_panic", 12345);
            // Trigger a WASM unreachable trap.
            unsafe { core::arch::wasm32::unreachable() }
        }

        // Default: no-op, return method name length.
        _ => {
            return method_end as i32;
        }
    }
}
