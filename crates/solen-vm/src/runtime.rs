//! WASM runtime: bytecode loading, execution with host functions.
//!
//! Contracts export:
//! - `call(input_ptr: i32, input_len: i32) -> i32` (returns output length)
//! - `memory` (shared linear memory)
//!
//! Host imports (module "env"):
//! - `storage_read(key_ptr, key_len, val_ptr) -> i32` (returns value length, -1 if not found)
//! - `storage_write(key_ptr, key_len, val_ptr, val_len)`
//! - `emit_event(topic_ptr, topic_len, data_ptr, data_len)`
//! - `get_caller(out_ptr)` (writes 32 bytes)
//! - `get_block_height() -> i64`
//! - `set_return_data(ptr, len)`

use std::sync::{Arc, Mutex};

use wasmtime::*;

use crate::host::{HostContext, HostEvent};
use crate::metering::{fuel_to_gas, DEFAULT_FUEL_LIMIT};
use crate::VmError;

/// Execute a WASM contract.
///
/// Returns (gas_used, return_data, events, updated_storage).
pub fn execute(
    bytecode: &[u8],
    input: &[u8],
    ctx: HostContext,
    fuel_limit: Option<u64>,
) -> Result<ExecutionResult, VmError> {
    let fuel = fuel_limit.unwrap_or(DEFAULT_FUEL_LIMIT);

    let mut config = Config::new();
    config.consume_fuel(true);

    let engine = Engine::new(&config).map_err(|e| VmError::InvalidBytecode(e.to_string()))?;
    let module =
        Module::new(&engine, bytecode).map_err(|e| VmError::InvalidBytecode(e.to_string()))?;

    let ctx = Arc::new(Mutex::new(ctx));
    let mut store = Store::new(&engine, ctx.clone());
    store.set_fuel(fuel).unwrap();

    let mut linker = Linker::new(&engine);
    register_host_functions(&mut linker, ctx.clone())?;

    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| VmError::Trap(e.to_string()))?;

    // Write input into WASM memory.
    let memory = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| VmError::MissingExport("memory".into()))?;

    let input_offset = 1024u32; // Fixed offset for input data
    memory
        .write(&mut store, input_offset as usize, input)
        .map_err(|e| VmError::Trap(format!("memory write failed: {e}")))?;

    // Call the contract's `call` function.
    let call_fn = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "call")
        .map_err(|e| VmError::MissingExport(format!("call: {e}")))?;

    let _output_len = call_fn
        .call(&mut store, (input_offset as i32, input.len() as i32))
        .map_err(|e| {
            if store.get_fuel().unwrap_or(0) == 0 {
                VmError::OutOfGas
            } else {
                VmError::Trap(e.to_string())
            }
        })?;

    let fuel_consumed = fuel - store.get_fuel().unwrap_or(0);
    let gas_used = fuel_to_gas(fuel_consumed);

    let ctx = ctx.lock().unwrap();
    Ok(ExecutionResult {
        gas_used,
        return_data: ctx.return_data.clone(),
        events: ctx.events.clone(),
        storage: ctx.storage.clone(),
    })
}

/// Result of WASM contract execution.
#[derive(Debug)]
pub struct ExecutionResult {
    pub gas_used: u64,
    pub return_data: Vec<u8>,
    pub events: Vec<HostEvent>,
    pub storage: std::collections::HashMap<Vec<u8>, Vec<u8>>,
}

fn register_host_functions(
    linker: &mut Linker<Arc<Mutex<HostContext>>>,
    ctx: Arc<Mutex<HostContext>>,
) -> Result<(), VmError> {
    // storage_read(key_ptr, key_len, val_ptr) -> i32
    let ctx_clone = ctx.clone();
    linker
        .func_wrap(
            "env",
            "storage_read",
            move |mut caller: Caller<'_, Arc<Mutex<HostContext>>>,
                  key_ptr: i32,
                  key_len: i32,
                  val_ptr: i32|
                  -> i32 {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let mut key = vec![0u8; key_len as usize];
                memory
                    .read(&caller, key_ptr as usize, &mut key)
                    .unwrap();

                let val = {
                    let ctx = ctx_clone.lock().unwrap();
                    ctx.storage.get(&key).cloned()
                };
                match val {
                    Some(val) => {
                        memory
                            .write(&mut caller, val_ptr as usize, &val)
                            .unwrap();
                        val.len() as i32
                    }
                    None => -1,
                }
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    // storage_write(key_ptr, key_len, val_ptr, val_len)
    let ctx_clone = ctx.clone();
    linker
        .func_wrap(
            "env",
            "storage_write",
            move |mut caller: Caller<'_, Arc<Mutex<HostContext>>>,
                  key_ptr: i32,
                  key_len: i32,
                  val_ptr: i32,
                  val_len: i32| {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let mut key = vec![0u8; key_len as usize];
                let mut val = vec![0u8; val_len as usize];
                memory.read(&caller, key_ptr as usize, &mut key).unwrap();
                memory.read(&caller, val_ptr as usize, &mut val).unwrap();

                let mut ctx = ctx_clone.lock().unwrap();
                ctx.storage.insert(key, val);
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    // emit_event(topic_ptr, topic_len, data_ptr, data_len)
    let ctx_clone = ctx.clone();
    linker
        .func_wrap(
            "env",
            "emit_event",
            move |mut caller: Caller<'_, Arc<Mutex<HostContext>>>,
                  topic_ptr: i32,
                  topic_len: i32,
                  data_ptr: i32,
                  data_len: i32| {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let mut topic = vec![0u8; topic_len as usize];
                let mut data = vec![0u8; data_len as usize];
                memory
                    .read(&caller, topic_ptr as usize, &mut topic)
                    .unwrap();
                memory
                    .read(&caller, data_ptr as usize, &mut data)
                    .unwrap();

                let mut ctx = ctx_clone.lock().unwrap();
                ctx.events.push(HostEvent { topic, data });
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    // get_caller(out_ptr)
    let ctx_clone = ctx.clone();
    linker
        .func_wrap(
            "env",
            "get_caller",
            move |mut caller: Caller<'_, Arc<Mutex<HostContext>>>, out_ptr: i32| {
                let ctx = ctx_clone.lock().unwrap();
                let id = ctx.caller;
                drop(ctx);
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                memory
                    .write(&mut caller, out_ptr as usize, &id)
                    .unwrap();
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    // get_block_height() -> i64
    let ctx_clone = ctx.clone();
    linker
        .func_wrap(
            "env",
            "get_block_height",
            move |_caller: Caller<'_, Arc<Mutex<HostContext>>>| -> i64 {
                let ctx = ctx_clone.lock().unwrap();
                ctx.block_height as i64
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    // set_return_data(ptr, len)
    let ctx_clone = ctx.clone();
    linker
        .func_wrap(
            "env",
            "set_return_data",
            move |mut caller: Caller<'_, Arc<Mutex<HostContext>>>, ptr: i32, len: i32| {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let mut data = vec![0u8; len as usize];
                memory.read(&caller, ptr as usize, &mut data).unwrap();

                let mut ctx = ctx_clone.lock().unwrap();
                ctx.return_data = data;
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal WAT contract that stores a value and emits an event.
    const COUNTER_CONTRACT: &str = r#"
    (module
        (import "env" "storage_read" (func $storage_read (param i32 i32 i32) (result i32)))
        (import "env" "storage_write" (func $storage_write (param i32 i32 i32 i32)))
        (import "env" "emit_event" (func $emit_event (param i32 i32 i32 i32)))
        (import "env" "get_caller" (func $get_caller (param i32)))
        (import "env" "get_block_height" (func $get_block_height (result i64)))
        (import "env" "set_return_data" (func $set_return_data (param i32 i32)))

        (memory (export "memory") 1)

        ;; Key "count" at offset 0
        (data (i32.const 0) "count")
        ;; Event topic "incremented" at offset 100
        (data (i32.const 100) "incremented")

        (func (export "call") (param $input_ptr i32) (param $input_len i32) (result i32)
            (local $val i32)

            ;; Read current counter from storage
            ;; storage_read(key_ptr=0, key_len=5, val_ptr=200) -> len
            (drop (call $storage_read (i32.const 0) (i32.const 5) (i32.const 200)))

            ;; Load the 4-byte counter value (defaults to 0)
            (local.set $val (i32.load (i32.const 200)))

            ;; Increment
            (local.set $val (i32.add (local.get $val) (i32.const 1)))

            ;; Store back
            (i32.store (i32.const 200) (local.get $val))
            (call $storage_write (i32.const 0) (i32.const 5) (i32.const 200) (i32.const 4))

            ;; Emit event
            (call $emit_event (i32.const 100) (i32.const 11) (i32.const 200) (i32.const 4))

            ;; Set return data (the counter value)
            (call $set_return_data (i32.const 200) (i32.const 4))

            ;; Return 4 (length of output)
            (i32.const 4)
        )
    )
    "#;

    #[test]
    fn execute_counter_contract() {
        let wasm = wat::parse_str(COUNTER_CONTRACT).expect("WAT parse failed");
        let ctx = HostContext::new([0u8; 32], 42);

        let result = execute(&wasm, &[], ctx, None).expect("execution failed");

        assert!(result.gas_used > 0);
        assert_eq!(result.return_data, 1u32.to_le_bytes());
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].topic, b"incremented");

        // Counter should be 1 in storage.
        let stored = result.storage.get(b"count".as_ref()).unwrap();
        assert_eq!(stored, &1u32.to_le_bytes());
    }

    #[test]
    fn counter_increments_with_state() {
        let wasm = wat::parse_str(COUNTER_CONTRACT).expect("WAT parse failed");

        // First call: counter = 0 -> 1
        let ctx = HostContext::new([0u8; 32], 1);
        let result1 = execute(&wasm, &[], ctx, None).unwrap();
        assert_eq!(result1.return_data, 1u32.to_le_bytes());

        // Second call with previous storage: counter = 1 -> 2
        let ctx2 = HostContext::new([0u8; 32], 2).with_storage(result1.storage);
        let result2 = execute(&wasm, &[], ctx2, None).unwrap();
        assert_eq!(result2.return_data, 2u32.to_le_bytes());
    }

    #[test]
    fn out_of_gas() {
        let wasm = wat::parse_str(COUNTER_CONTRACT).expect("WAT parse failed");
        let ctx = HostContext::new([0u8; 32], 1);

        // Give very little fuel.
        let result = execute(&wasm, &[], ctx, Some(1));
        assert!(matches!(result, Err(VmError::OutOfGas)));
    }

    #[test]
    fn invalid_bytecode() {
        let result = execute(b"not wasm", &[], HostContext::new([0u8; 32], 1), None);
        assert!(matches!(result, Err(VmError::InvalidBytecode(_))));
    }
}
