//! VM sandbox and malformed input tests.

use solen_vm::runtime::VmRuntime;
use solen_vm::host::HostContext;

// ── Test #21: Malformed WASM deploy validation ────────────────

#[test]
fn reject_invalid_wasm_bytecode() {
    let vm = VmRuntime::new().unwrap();
    let result = vm.validate_bytecode(b"not wasm at all");
    assert!(result.is_err(), "invalid WASM must be rejected");
}

#[test]
fn reject_wasm_missing_memory_export() {
    let vm = VmRuntime::new().unwrap();
    // Valid WASM module but no memory export.
    let wasm = wat::parse_str(r#"(module
        (func (export "call") (param i32 i32) (result i32) (i32.const 0))
    )"#).unwrap();
    let result = vm.validate_bytecode(&wasm);
    assert!(result.is_err(), "WASM without memory export must be rejected");
}

#[test]
fn reject_wasm_missing_call_export() {
    let vm = VmRuntime::new().unwrap();
    // Valid WASM with memory but no call export.
    let wasm = wat::parse_str(r#"(module
        (memory (export "memory") 1)
    )"#).unwrap();
    let result = vm.validate_bytecode(&wasm);
    assert!(result.is_err(), "WASM without call export must be rejected");
}

#[test]
fn accept_valid_wasm() {
    let vm = VmRuntime::new().unwrap();
    let wasm = wat::parse_str(r#"(module
        (memory (export "memory") 1)
        (func (export "call") (param i32 i32) (result i32) (i32.const 0))
    )"#).unwrap();
    let result = vm.validate_bytecode(&wasm);
    assert!(result.is_ok(), "valid WASM must be accepted");
}

// ── Test #22: Host functions with boundary parameters ─────────

#[test]
fn host_function_negative_length_does_not_panic() {
    let vm = VmRuntime::new().unwrap();

    // Contract that calls storage_read with len=-1 (i32::MIN mapped).
    let wasm = wat::parse_str(r#"(module
        (import "env" "storage_read" (func $read (param i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "call") (param i32 i32) (result i32)
            ;; Call storage_read with key_ptr=0, key_len=-1 (negative), val_ptr=0
            (call $read (i32.const 0) (i32.const -1) (i32.const 0))
        )
    )"#).unwrap();

    let code_hash = solen_crypto::blake3_hash(&wasm);
    let ctx = HostContext {
        caller: [0u8; 32],
        block_height: 1,
        storage: std::collections::HashMap::new(),
        events: Vec::new(),
        return_data: Vec::new(),
    };

    // Must not panic — should return an error or trap.
    let result = vm.execute(&code_hash, &wasm, &[], ctx, None);
    // We don't care if it's Ok or Err, just that it didn't panic.
    let _ = result;
}

#[test]
fn host_function_huge_length_does_not_panic() {
    let vm = VmRuntime::new().unwrap();

    // Contract that calls storage_write with huge lengths.
    let wasm = wat::parse_str(r#"(module
        (import "env" "storage_write" (func $write (param i32 i32 i32 i32)))
        (memory (export "memory") 1)
        (func (export "call") (param i32 i32) (result i32)
            ;; key_ptr=0, key_len=2147483647 (i32::MAX), val_ptr=0, val_len=0
            (call $write (i32.const 0) (i32.const 2147483647) (i32.const 0) (i32.const 0))
            (i32.const 0)
        )
    )"#).unwrap();

    let code_hash = solen_crypto::blake3_hash(&wasm);
    let ctx = HostContext {
        caller: [0u8; 32],
        block_height: 1,
        storage: std::collections::HashMap::new(),
        events: Vec::new(),
        return_data: Vec::new(),
    };

    let result = vm.execute(&code_hash, &wasm, &[], ctx, None);
    let _ = result; // Must not panic.
}

#[test]
fn set_return_data_bounded() {
    let vm = VmRuntime::new().unwrap();

    // Contract that calls set_return_data with huge length.
    let wasm = wat::parse_str(r#"(module
        (import "env" "set_return_data" (func $set_ret (param i32 i32)))
        (memory (export "memory") 1)
        (func (export "call") (param i32 i32) (result i32)
            ;; ptr=0, len=2000000000 (2GB — way over 1MB limit)
            (call $set_ret (i32.const 0) (i32.const 2000000000))
            (i32.const 0)
        )
    )"#).unwrap();

    let code_hash = solen_crypto::blake3_hash(&wasm);
    let ctx = HostContext {
        caller: [0u8; 32],
        block_height: 1,
        storage: std::collections::HashMap::new(),
        events: Vec::new(),
        return_data: Vec::new(),
    };

    let result = vm.execute(&code_hash, &wasm, &[], ctx, None);
    // Should complete without OOM or panic.
    // Return data should be empty (the call was rejected by checked_len).
    if let Ok(exec_result) = result {
        assert!(
            exec_result.return_data.len() <= 1_048_576,
            "return data must be bounded by 1MB"
        );
    }
}

// ── Test: emit_event consumes fuel ────────────────────────────

#[test]
fn emit_event_consumes_fuel() {
    let vm = VmRuntime::new().unwrap();

    // Contract that emits 1000 events in a loop.
    let wasm = wat::parse_str(r#"(module
        (import "env" "emit_event" (func $emit (param i32 i32 i32 i32)))
        (memory (export "memory") 1)
        (data (i32.const 0) "test")
        (func (export "call") (param i32 i32) (result i32)
            (local $i i32)
            (local.set $i (i32.const 0))
            (block $break
                (loop $loop
                    (call $emit (i32.const 0) (i32.const 4) (i32.const 0) (i32.const 4))
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br_if $break (i32.ge_u (local.get $i) (i32.const 10000)))
                    (br $loop)
                )
            )
            (i32.const 0)
        )
    )"#).unwrap();

    let code_hash = solen_crypto::blake3_hash(&wasm);
    let ctx = HostContext {
        caller: [0u8; 32],
        block_height: 1,
        storage: std::collections::HashMap::new(),
        events: Vec::new(),
        return_data: Vec::new(),
    };

    // Execute with limited fuel — should run out before completing all 10000 events.
    let result = vm.execute(&code_hash, &wasm, &[], ctx, Some(100_000));
    match result {
        Ok(r) => {
            // If it completed, it should have consumed significant gas.
            assert!(r.gas_used > 0, "events should consume fuel");
            assert!(
                r.events.len() < 10_000,
                "should run out of fuel before 10000 events"
            );
        }
        Err(_) => {
            // Out of fuel trap — expected.
        }
    }
}
