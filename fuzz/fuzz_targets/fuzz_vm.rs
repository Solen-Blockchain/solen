//! Fuzz target: feed random bytecode into the WASM VM.
//!
//! Ensures the VM handles malformed/adversarial bytecode gracefully
//! without panics or resource exhaustion.

#![no_main]

use libfuzzer_sys::fuzz_target;
use solen_vm::host::HostContext;
use solen_vm::runtime::execute;

fuzz_target!(|data: &[u8]| {
    let ctx = HostContext::new([0u8; 32], 1);

    // Execute with strict fuel limit to prevent infinite loops.
    let _ = execute(data, &[], ctx, Some(10_000));
});
