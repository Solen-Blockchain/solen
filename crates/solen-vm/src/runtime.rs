//! WASM runtime: bytecode loading, execution with host functions.
//!
//! Uses wasmtime with InstancePre for fast instantiation. Host functions
//! access context through the Store data rather than captured closures,
//! enabling pre-linking and instance pooling.

use std::collections::HashMap;
use std::sync::Mutex;

use wasmtime::*;

use crate::host::{HostContext, HostEvent};
use crate::metering::{fuel_to_gas, DEFAULT_FUEL_LIMIT};
use crate::VmError;

/// Store data: the HostContext lives inside the wasmtime Store.
struct StoreData {
    ctx: HostContext,
}

/// Cached WASM runtime with pre-linked instances.
pub struct VmRuntime {
    engine: Engine,
    /// Cache of pre-linked instances keyed by code hash.
    /// InstancePre has all host functions linked — instantiation only
    /// allocates memory and runs start functions.
    pre_cache: Mutex<HashMap<[u8; 32], InstancePre<StoreData>>>,
    /// Shared linker with all host functions registered.
    linker: Linker<StoreData>,
}

impl VmRuntime {
    pub fn new() -> Result<Self, VmError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        // Canonicalize NaN values to ensure deterministic float behavior
        // across all platforms. Without this, different validators could
        // produce different state roots from the same WASM execution.
        config.cranelift_nan_canonicalization(true);
        let engine =
            Engine::new(&config).map_err(|e| VmError::InvalidBytecode(e.to_string()))?;

        let mut linker = Linker::new(&engine);
        register_host_functions_typed(&mut linker)?;

        Ok(Self {
            engine,
            pre_cache: Mutex::new(HashMap::new()),
            linker,
        })
    }

    /// Validate WASM bytecode without executing it. Checks that the module
    /// compiles and exports the required `memory` and `call` symbols.
    pub fn validate_bytecode(&self, bytecode: &[u8]) -> Result<(), VmError> {
        let module = Module::new(&self.engine, bytecode)
            .map_err(|e| VmError::InvalidBytecode(e.to_string()))?;

        // Check required exports exist.
        let has_memory = module.exports().any(|e| e.name() == "memory");
        let has_call = module.exports().any(|e| e.name() == "call");

        if !has_memory {
            return Err(VmError::MissingExport("memory".into()));
        }
        if !has_call {
            return Err(VmError::MissingExport("call".into()));
        }
        Ok(())
    }

    /// Execute a contract using pre-linked instances.
    pub fn execute(
        &self,
        code_hash: &[u8; 32],
        bytecode: &[u8],
        input: &[u8],
        ctx: HostContext,
        fuel_limit: Option<u64>,
    ) -> Result<ExecutionResult, VmError> {
        let pre = self.get_or_prelink(code_hash, bytecode)?;
        execute_pre(&self.engine, &pre, input, ctx, fuel_limit)
    }

    fn get_or_prelink(
        &self,
        code_hash: &[u8; 32],
        bytecode: &[u8],
    ) -> Result<InstancePre<StoreData>, VmError> {
        let mut cache = self.pre_cache.lock().unwrap();
        if let Some(pre) = cache.get(code_hash) {
            return Ok(pre.clone());
        }
        let module = Module::new(&self.engine, bytecode)
            .map_err(|e| VmError::InvalidBytecode(e.to_string()))?;
        let pre = self
            .linker
            .instantiate_pre(&module)
            .map_err(|e| VmError::Trap(e.to_string()))?;
        cache.insert(*code_hash, pre.clone());
        Ok(pre)
    }

    pub fn cache_size(&self) -> usize {
        self.pre_cache.lock().unwrap().len()
    }
}

/// Execute using a pre-linked instance (fast path).
fn execute_pre(
    engine: &Engine,
    pre: &InstancePre<StoreData>,
    input: &[u8],
    ctx: HostContext,
    fuel_limit: Option<u64>,
) -> Result<ExecutionResult, VmError> {
    let fuel = fuel_limit.unwrap_or(DEFAULT_FUEL_LIMIT);

    let mut store = Store::new(engine, StoreData { ctx });
    store.set_fuel(fuel).unwrap();

    let instance = pre
        .instantiate(&mut store)
        .map_err(|e| VmError::Trap(e.to_string()))?;

    run_instance(&mut store, &instance, input, fuel)
}

/// Execute a WASM contract (standalone, no caching).
pub fn execute(
    bytecode: &[u8],
    input: &[u8],
    ctx: HostContext,
    fuel_limit: Option<u64>,
) -> Result<ExecutionResult, VmError> {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.cranelift_nan_canonicalization(true);

    let engine = Engine::new(&config).map_err(|e| VmError::InvalidBytecode(e.to_string()))?;
    let module =
        Module::new(&engine, bytecode).map_err(|e| VmError::InvalidBytecode(e.to_string()))?;

    let mut linker = Linker::new(&engine);
    register_host_functions_typed(&mut linker)?;

    let fuel = fuel_limit.unwrap_or(DEFAULT_FUEL_LIMIT);
    let mut store = Store::new(&engine, StoreData { ctx });
    store.set_fuel(fuel).unwrap();

    let instance = linker
        .instantiate(&mut store, &module)
        .map_err(|e| VmError::Trap(e.to_string()))?;

    run_instance(&mut store, &instance, input, fuel)
}

/// Shared execution logic: write input, call, extract results.
fn run_instance(
    store: &mut Store<StoreData>,
    instance: &Instance,
    input: &[u8],
    fuel: u64,
) -> Result<ExecutionResult, VmError> {
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| VmError::MissingExport("memory".into()))?;

    let input_offset = 1024u32;
    memory
        .write(&mut *store, input_offset as usize, input)
        .map_err(|e| VmError::Trap(format!("memory write failed: {e}")))?;

    let call_fn = instance
        .get_typed_func::<(i32, i32), i32>(&mut *store, "call")
        .map_err(|e| VmError::MissingExport(format!("call: {e}")))?;

    let _output_len = call_fn
        .call(&mut *store, (input_offset as i32, input.len() as i32))
        .map_err(|e| {
            if store.get_fuel().unwrap_or(0) == 0 {
                VmError::OutOfGas
            } else {
                VmError::Trap(e.to_string())
            }
        })?;

    let fuel_consumed = fuel - store.get_fuel().unwrap_or(0);
    let gas_used = fuel_to_gas(fuel_consumed);

    let data = store.data();
    Ok(ExecutionResult {
        gas_used,
        return_data: data.ctx.return_data.clone(),
        events: data.ctx.events.clone(),
        storage: data.ctx.storage.clone(),
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

/// Safely get memory from a caller. Returns None if memory export is missing.
fn get_memory(caller: &mut Caller<'_, StoreData>) -> Option<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
}

/// Safely read bytes from WASM memory. Returns false on out-of-bounds.
fn safe_read(caller: &Caller<'_, StoreData>, memory: &wasmtime::Memory, ptr: usize, buf: &mut [u8]) -> bool {
    memory.read(caller, ptr, buf).is_ok()
}

/// Safely write bytes to WASM memory. Returns false on out-of-bounds.
fn safe_write(caller: &mut Caller<'_, StoreData>, memory: &wasmtime::Memory, ptr: usize, data: &[u8]) -> bool {
    memory.write(caller, ptr, data).is_ok()
}

/// Register host functions using typed Func API (no closures capturing context).
/// Context is accessed via `Caller::data()` / `Caller::data_mut()`.
/// All memory operations are bounds-checked — invalid pointers return error
/// values instead of panicking.
fn register_host_functions_typed(
    linker: &mut Linker<StoreData>,
) -> Result<(), VmError> {
    // Maximum allocation size for host function buffers (1 MB).
    // Prevents OOM from negative i32 cast to usize or huge allocations.
    const MAX_HOST_ALLOC: usize = 1024 * 1024;

    /// Validate and convert i32 length to usize, rejecting negative or oversized values.
    fn checked_len(len: i32) -> Option<usize> {
        if len < 0 { return None; }
        let n = len as usize;
        if n > MAX_HOST_ALLOC { return None; }
        Some(n)
    }

    linker
        .func_wrap(
            "env",
            "storage_read",
            |mut caller: Caller<'_, StoreData>,
             key_ptr: i32,
             key_len: i32,
             val_ptr: i32|
             -> i32 {
                let klen = match checked_len(key_len) { Some(n) => n, None => return -1 };
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return -1,
                };
                let mut key = vec![0u8; klen];
                if !safe_read(&caller, &memory, key_ptr as usize, &mut key) {
                    return -1;
                }

                let read_cost = crate::metering::storage_read_fuel(key.len());
                {
                    let remaining = caller.get_fuel().unwrap_or(0);
                    if remaining < read_cost { return -1; }
                    let _ = caller.set_fuel(remaining - read_cost);
                }

                let val = caller.data().ctx.storage.get(&key).cloned();
                match val {
                    Some(val) => {
                        if !safe_write(&mut caller, &memory, val_ptr as usize, &val) {
                            return -1;
                        }
                        val.len() as i32
                    }
                    None => -1,
                }
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    linker
        .func_wrap(
            "env",
            "storage_write",
            |mut caller: Caller<'_, StoreData>,
             key_ptr: i32,
             key_len: i32,
             val_ptr: i32,
             val_len: i32| {
                let klen = match checked_len(key_len) { Some(n) => n, None => return };
                let vlen = match checked_len(val_len) { Some(n) => n, None => return };
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return,
                };
                let mut key = vec![0u8; klen];
                let mut val = vec![0u8; vlen];
                if !safe_read(&caller, &memory, key_ptr as usize, &mut key) { return; }
                if !safe_read(&caller, &memory, val_ptr as usize, &mut val) { return; }

                let write_cost = crate::metering::storage_write_fuel(key.len(), val.len());
                {
                    let remaining = caller.get_fuel().unwrap_or(0);
                    if remaining < write_cost { return; }
                    let _ = caller.set_fuel(remaining - write_cost);
                }

                caller.data_mut().ctx.storage.insert(key, val);
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    linker
        .func_wrap(
            "env",
            "emit_event",
            |mut caller: Caller<'_, StoreData>,
             topic_ptr: i32,
             topic_len: i32,
             data_ptr: i32,
             data_len: i32| {
                let tlen = match checked_len(topic_len) { Some(n) => n, None => return };
                let dlen = match checked_len(data_len) { Some(n) => n, None => return };
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return,
                };
                let mut topic = vec![0u8; tlen];
                let mut data = vec![0u8; dlen];
                if !safe_read(&caller, &memory, topic_ptr as usize, &mut topic) { return; }
                if !safe_read(&caller, &memory, data_ptr as usize, &mut data) { return; }

                // Charge fuel for event emission.
                let event_cost = crate::metering::STORAGE_WRITE_BASE_FUEL
                    + ((topic.len() + data.len()) as u64) * crate::metering::STORAGE_WRITE_PER_BYTE_FUEL;
                {
                    let remaining = caller.get_fuel().unwrap_or(0);
                    if remaining < event_cost { return; }
                    let _ = caller.set_fuel(remaining - event_cost);
                }

                caller.data_mut().ctx.events.push(HostEvent { topic, data });
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    linker
        .func_wrap(
            "env",
            "get_caller",
            |mut caller: Caller<'_, StoreData>, out_ptr: i32| {
                let id = caller.data().ctx.caller;
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return,
                };
                let _ = safe_write(&mut caller, &memory, out_ptr as usize, &id);
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    linker
        .func_wrap(
            "env",
            "get_block_height",
            |caller: Caller<'_, StoreData>| -> i64 {
                caller.data().ctx.block_height as i64
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    linker
        .func_wrap(
            "env",
            "set_return_data",
            |mut caller: Caller<'_, StoreData>, ptr: i32, len: i32| {
                let dlen = match checked_len(len) { Some(n) => n, None => return };
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return,
                };
                let mut data = vec![0u8; dlen];
                if !safe_read(&caller, &memory, ptr as usize, &mut data) { return; }

                // Charge fuel for return data.
                let cost = crate::metering::STORAGE_READ_BASE_FUEL + (data.len() as u64) * crate::metering::STORAGE_WRITE_PER_BYTE_FUEL;
                {
                    let remaining = caller.get_fuel().unwrap_or(0);
                    if remaining < cost { return; }
                    let _ = caller.set_fuel(remaining - cost);
                }

                caller.data_mut().ctx.return_data = data;
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COUNTER_CONTRACT: &str = r#"
    (module
        (import "env" "storage_read" (func $storage_read (param i32 i32 i32) (result i32)))
        (import "env" "storage_write" (func $storage_write (param i32 i32 i32 i32)))
        (import "env" "emit_event" (func $emit_event (param i32 i32 i32 i32)))
        (import "env" "get_caller" (func $get_caller (param i32)))
        (import "env" "get_block_height" (func $get_block_height (result i64)))
        (import "env" "set_return_data" (func $set_return_data (param i32 i32)))
        (memory (export "memory") 1)
        (data (i32.const 0) "count")
        (data (i32.const 100) "incremented")
        (func (export "call") (param $input_ptr i32) (param $input_len i32) (result i32)
            (local $val i32)
            (drop (call $storage_read (i32.const 0) (i32.const 5) (i32.const 200)))
            (local.set $val (i32.load (i32.const 200)))
            (local.set $val (i32.add (local.get $val) (i32.const 1)))
            (i32.store (i32.const 200) (local.get $val))
            (call $storage_write (i32.const 0) (i32.const 5) (i32.const 200) (i32.const 4))
            (call $emit_event (i32.const 100) (i32.const 11) (i32.const 200) (i32.const 4))
            (call $set_return_data (i32.const 200) (i32.const 4))
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
        assert_eq!(result.storage.get(b"count".as_ref()).unwrap(), &1u32.to_le_bytes());
    }

    #[test]
    fn counter_increments_with_state() {
        let wasm = wat::parse_str(COUNTER_CONTRACT).expect("WAT parse failed");

        let ctx = HostContext::new([0u8; 32], 1);
        let r1 = execute(&wasm, &[], ctx, None).unwrap();
        assert_eq!(r1.return_data, 1u32.to_le_bytes());

        let ctx2 = HostContext::new([0u8; 32], 2).with_storage(r1.storage);
        let r2 = execute(&wasm, &[], ctx2, None).unwrap();
        assert_eq!(r2.return_data, 2u32.to_le_bytes());
    }

    #[test]
    fn out_of_gas() {
        let wasm = wat::parse_str(COUNTER_CONTRACT).expect("WAT parse failed");
        let ctx = HostContext::new([0u8; 32], 1);
        assert!(matches!(execute(&wasm, &[], ctx, Some(1)), Err(VmError::OutOfGas)));
    }

    #[test]
    fn invalid_bytecode() {
        assert!(matches!(
            execute(b"not wasm", &[], HostContext::new([0u8; 32], 1), None),
            Err(VmError::InvalidBytecode(_))
        ));
    }

    #[test]
    fn cached_runtime_with_instance_pre() {
        let wasm = wat::parse_str(COUNTER_CONTRACT).expect("WAT parse failed");
        let code_hash = solen_crypto::blake3_hash(&wasm);

        let runtime = VmRuntime::new().unwrap();
        assert_eq!(runtime.cache_size(), 0);

        // First call: compile + pre-link + cache.
        let ctx = HostContext::new([0u8; 32], 1);
        let r1 = runtime.execute(&code_hash, &wasm, &[], ctx, None).unwrap();
        assert_eq!(r1.return_data, 1u32.to_le_bytes());
        assert_eq!(runtime.cache_size(), 1);

        // Second call: cache hit — only instantiation, no compilation or linking.
        let ctx2 = HostContext::new([0u8; 32], 2).with_storage(r1.storage);
        let r2 = runtime.execute(&code_hash, &wasm, &[], ctx2, None).unwrap();
        assert_eq!(r2.return_data, 2u32.to_le_bytes());
        assert_eq!(runtime.cache_size(), 1);
    }
}
