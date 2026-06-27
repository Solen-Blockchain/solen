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
    /// Deterministic resource limits. In strict (post-gate) mode this caps WASM
    /// linear-memory growth so `memory.grow` succeeds/fails identically on every
    /// node regardless of host RAM (C-04). Default = no limits (legacy).
    limits: StoreLimits,
}

/// Maximum WASM linear memory per instance in strict mode (C-04). Chosen well
/// above any legitimate contract's needs but small enough that EVERY validator
/// can always satisfy it — so `memory.grow` outcomes depend only on this fixed
/// bound, never on a node's available RAM. 64 MiB = 1024 wasm pages.
const MAX_WASM_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// Build the wasmtime config. `strict` enables the determinism hardening
/// (C-04/C-05) gated by `determinism_fix_height`. NaN canonicalization and fuel
/// metering are always on; the strict config additionally pins relaxed-SIMD to a
/// deterministic lowering so heterogeneous CPUs cannot diverge (C-05). Memory
/// bounding (C-04) is applied per-Store, not here.
fn build_config(strict: bool) -> Config {
    let mut config = Config::new();
    config.consume_fuel(true);
    // Canonicalize NaN values to ensure deterministic float behavior across all
    // platforms. Without this, different validators could produce different
    // state roots from the same WASM execution.
    config.cranelift_nan_canonicalization(true);
    if strict {
        // C-05: relaxed-SIMD is enabled-by-default in wasmtime and is permitted
        // to lower to hardware-dependent results — a heterogeneous-fleet
        // consensus fork. Force the deterministic lowering (keeps the feature
        // working, removes the nondeterminism). NaN canonicalization does NOT
        // cover relaxed-SIMD lane ops, so this is required separately.
        config.relaxed_simd_deterministic(true);
    }
    config
}

/// Cached WASM runtime with pre-linked instances. Holds two engines: a `legacy`
/// engine (pre-gate behavior, byte-for-byte) and a `strict` engine (C-04/C-05
/// determinism hardening). The engine used per execution is chosen by block
/// height vs `determinism_fix_height`, so the switch is a coordinated,
/// consensus-affecting activation (all nodes flip at the same height).
pub struct VmRuntime {
    engine: Engine,
    engine_strict: Engine,
    /// Cache of pre-linked instances keyed by code hash (legacy engine).
    pre_cache: Mutex<HashMap<[u8; 32], InstancePre<StoreData>>>,
    /// Same, for the strict engine (instances are engine-specific).
    pre_cache_strict: Mutex<HashMap<[u8; 32], InstancePre<StoreData>>>,
    /// Linkers with all host functions registered (one per engine).
    linker: Linker<StoreData>,
    linker_strict: Linker<StoreData>,
    /// Block height at/after which strict determinism config applies. u64::MAX =
    /// off (legacy everywhere).
    determinism_fix_height: u64,
}

impl VmRuntime {
    pub fn new() -> Result<Self, VmError> {
        let engine = Engine::new(&build_config(false))
            .map_err(|e| VmError::InvalidBytecode(e.to_string()))?;
        let engine_strict = Engine::new(&build_config(true))
            .map_err(|e| VmError::InvalidBytecode(e.to_string()))?;

        let mut linker = Linker::new(&engine);
        register_host_functions_typed(&mut linker)?;
        let mut linker_strict = Linker::new(&engine_strict);
        register_host_functions_typed(&mut linker_strict)?;

        Ok(Self {
            engine,
            engine_strict,
            pre_cache: Mutex::new(HashMap::new()),
            pre_cache_strict: Mutex::new(HashMap::new()),
            linker,
            linker_strict,
            determinism_fix_height: u64::MAX,
        })
    }

    /// Set the activation height for the C-04/C-05 determinism hardening.
    pub fn set_determinism_fix_height(&mut self, height: u64) {
        self.determinism_fix_height = height;
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

    /// Execute a contract using pre-linked instances. Strict determinism config
    /// (C-04/C-05) applies when the executing block height is at/after the gate.
    pub fn execute(
        &self,
        code_hash: &[u8; 32],
        bytecode: &[u8],
        input: &[u8],
        ctx: HostContext,
        fuel_limit: Option<u64>,
    ) -> Result<ExecutionResult, VmError> {
        let strict = ctx.block_height >= self.determinism_fix_height;
        if strict {
            let pre = Self::get_or_prelink(
                &self.engine_strict, &self.linker_strict, &self.pre_cache_strict, code_hash, bytecode,
            )?;
            execute_pre(&self.engine_strict, &pre, input, ctx, fuel_limit, true)
        } else {
            let pre = Self::get_or_prelink(
                &self.engine, &self.linker, &self.pre_cache, code_hash, bytecode,
            )?;
            execute_pre(&self.engine, &pre, input, ctx, fuel_limit, false)
        }
    }

    fn get_or_prelink(
        engine: &Engine,
        linker: &Linker<StoreData>,
        cache: &Mutex<HashMap<[u8; 32], InstancePre<StoreData>>>,
        code_hash: &[u8; 32],
        bytecode: &[u8],
    ) -> Result<InstancePre<StoreData>, VmError> {
        let mut cache = cache.lock().unwrap();
        if let Some(pre) = cache.get(code_hash) {
            return Ok(pre.clone());
        }
        let module = Module::new(engine, bytecode)
            .map_err(|e| VmError::InvalidBytecode(e.to_string()))?;
        let pre = linker
            .instantiate_pre(&module)
            .map_err(|e| VmError::Trap(e.to_string()))?;
        // Evict oldest entries if cache exceeds limit to prevent memory DoS.
        const MAX_CACHE_SIZE: usize = 1024;
        if cache.len() >= MAX_CACHE_SIZE {
            // Simple eviction: remove a random entry (HashMap iteration order).
            if let Some(key) = cache.keys().next().copied() {
                cache.remove(&key);
            }
        }
        cache.insert(*code_hash, pre.clone());
        Ok(pre)
    }

    pub fn cache_size(&self) -> usize {
        self.pre_cache.lock().unwrap().len()
    }
}

/// Execute using a pre-linked instance (fast path). In `strict` mode a
/// deterministic memory limiter is installed (C-04).
fn execute_pre(
    engine: &Engine,
    pre: &InstancePre<StoreData>,
    input: &[u8],
    ctx: HostContext,
    fuel_limit: Option<u64>,
    strict: bool,
) -> Result<ExecutionResult, VmError> {
    let fuel = fuel_limit.unwrap_or(DEFAULT_FUEL_LIMIT);

    let limits = if strict {
        StoreLimitsBuilder::new().memory_size(MAX_WASM_MEMORY_BYTES).build()
    } else {
        StoreLimits::default()
    };
    let mut store = Store::new(engine, StoreData { ctx, limits });
    if strict {
        // Bound linear-memory growth deterministically (C-04).
        store.limiter(|data| &mut data.limits);
    }
    store.set_fuel(fuel).unwrap();

    let instance = pre
        .instantiate(&mut store)
        .map_err(|e| VmError::Trap(e.to_string()))?;

    run_instance(&mut store, &instance, input, fuel)
}

/// Execute a WASM contract (standalone, no caching). Legacy config (no gate);
/// used by tests/utilities, not the consensus block path (which goes through
/// `VmRuntime::execute`).
pub fn execute(
    bytecode: &[u8],
    input: &[u8],
    ctx: HostContext,
    fuel_limit: Option<u64>,
) -> Result<ExecutionResult, VmError> {
    let engine = Engine::new(&build_config(false))
        .map_err(|e| VmError::InvalidBytecode(e.to_string()))?;
    let module =
        Module::new(&engine, bytecode).map_err(|e| VmError::InvalidBytecode(e.to_string()))?;

    let mut linker = Linker::new(&engine);
    register_host_functions_typed(&mut linker)?;

    let fuel = fuel_limit.unwrap_or(DEFAULT_FUEL_LIMIT);
    let mut store = Store::new(&engine, StoreData { ctx, limits: StoreLimits::default() });
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
        native_transfers: data.ctx.native_transfers.clone(),
        pending_calls: data.ctx.pending_calls.clone(),
    })
}

/// Result of WASM contract execution.
#[derive(Debug)]
pub struct ExecutionResult {
    pub gas_used: u64,
    pub return_data: Vec<u8>,
    pub events: Vec<HostEvent>,
    pub storage: std::collections::HashMap<Vec<u8>, Vec<u8>>,
    pub native_transfers: Vec<crate::host::NativeTransfer>,
    pub pending_calls: Vec<crate::host::PendingCall>,
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

    // transfer_native(to_ptr: i32, amount_ptr: i32) -> i32
    // Queues a native SOLEN transfer from the contract to the specified account.
    // Returns 0 on success, -1 on failure.
    // The transfer is executed by the executor after WASM completes.
    linker
        .func_wrap(
            "env",
            "transfer_native",
            |mut caller: Caller<'_, StoreData>,
             to_ptr: i32,
             amount_ptr: i32|
             -> i32 {
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return -1,
                };
                let mut to = [0u8; 32];
                if !safe_read(&caller, &memory, to_ptr as usize, &mut to) {
                    return -1;
                }
                let mut amount_buf = [0u8; 16];
                if !safe_read(&caller, &memory, amount_ptr as usize, &mut amount_buf) {
                    return -1;
                }
                let amount = u128::from_le_bytes(amount_buf);
                if amount == 0 {
                    return -1;
                }

                // Charge fuel for the transfer.
                let transfer_cost = 5000u64;
                {
                    let remaining = caller.get_fuel().unwrap_or(0);
                    if remaining < transfer_cost { return -1; }
                    let _ = caller.set_fuel(remaining - transfer_cost);
                }

                // Cap total queued transfers per execution (prevent abuse).
                if caller.data().ctx.native_transfers.len() >= 50 {
                    return -1;
                }

                caller.data_mut().ctx.native_transfers.push(
                    crate::host::NativeTransfer { to, amount }
                );
                0
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    // get_msg_value(out_ptr: i32) — writes the u128 LE amount of native SOLEN
    // transferred to this contract in the current UserOperation (summed across
    // all unconsumed preceding Transfer actions since the last Call to self).
    // Stays constant throughout a single Call frame.
    linker
        .func_wrap(
            "env",
            "get_msg_value",
            |mut caller: Caller<'_, StoreData>, out_ptr: i32| {
                let amount = caller.data().ctx.msg_value;
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return,
                };
                let _ = safe_write(&mut caller, &memory, out_ptr as usize, &amount.to_le_bytes());
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    // get_self_id(out_ptr: i32) — returns the contract's own account ID.
    // Needed so contracts can reference their own address for token operations.
    linker
        .func_wrap(
            "env",
            "get_self_id",
            |mut caller: Caller<'_, StoreData>, out_ptr: i32| {
                let id = caller.data().ctx.contract_id;
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return,
                };
                let _ = safe_write(&mut caller, &memory, out_ptr as usize, &id);
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    // get_self_balance(out_ptr: i32) — writes the u128 LE balance of this
    // contract's own account, snapshotted at frame start. Includes any
    // preceding Action::Transfer to self in the same op (so it includes
    // msg_value), but does NOT reflect outflows queued during this frame —
    // sdk::transfer and sdk::queue_call only execute after WASM returns.
    // Use cases: detect exogenous inflows like auto-credited staking rewards,
    // assert pool invariants in liquid-staking-style contracts.
    linker
        .func_wrap(
            "env",
            "get_self_balance",
            |mut caller: Caller<'_, StoreData>, out_ptr: i32| {
                let balance = caller.data().ctx.self_balance;
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return,
                };
                let _ = safe_write(&mut caller, &memory, out_ptr as usize, &balance.to_le_bytes());
            },
        )
        .map_err(|e| VmError::HostError(e.to_string()))?;

    // queue_contract_call(target_ptr, method_ptr, method_len, args_ptr, args_len) -> i32
    // Queues a contract→contract call. The call is dispatched by the executor
    // AFTER this contract's `call()` returns, so it cannot re-enter the
    // queueing contract. The dispatched call sees `caller = this contract_id`.
    // Returns 0 on success, -1 on invalid memory / method length / cap reached.
    linker
        .func_wrap(
            "env",
            "queue_contract_call",
            |mut caller: Caller<'_, StoreData>,
             target_ptr: i32,
             method_ptr: i32,
             method_len: i32,
             args_ptr: i32,
             args_len: i32|
             -> i32 {
                // Per-execution cap: prevents a single call from fanning out
                // an unbounded number of sub-calls. Matches the native_transfers cap.
                const MAX_PENDING_CALLS: usize = 16;
                // Hard limits on method/args size to bound memory + downstream gas.
                const MAX_METHOD_LEN: usize = 64;
                const MAX_ARGS_LEN: usize = 16 * 1024;

                let mlen = match checked_len(method_len) { Some(n) => n, None => return -1 };
                let alen = match checked_len(args_len) { Some(n) => n, None => return -1 };
                if mlen == 0 || mlen > MAX_METHOD_LEN { return -1; }
                if alen > MAX_ARGS_LEN { return -1; }

                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return -1,
                };

                let mut target = [0u8; 32];
                if !safe_read(&caller, &memory, target_ptr as usize, &mut target) {
                    return -1;
                }
                let mut method = vec![0u8; mlen];
                if !safe_read(&caller, &memory, method_ptr as usize, &mut method) {
                    return -1;
                }
                let mut args = vec![0u8; alen];
                if alen > 0 && !safe_read(&caller, &memory, args_ptr as usize, &mut args) {
                    return -1;
                }

                // Charge fuel for queuing. Actual sub-call gas is charged
                // when the executor dispatches the queued call.
                let queue_cost = 5_000u64 + (mlen as u64 + alen as u64) * 10;
                {
                    let remaining = caller.get_fuel().unwrap_or(0);
                    if remaining < queue_cost { return -1; }
                    let _ = caller.set_fuel(remaining - queue_cost);
                }

                if caller.data().ctx.pending_calls.len() >= MAX_PENDING_CALLS {
                    return -1;
                }

                caller.data_mut().ctx.pending_calls.push(
                    crate::host::PendingCall { target, method, args }
                );
                0
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

    // Tries to grow linear memory by 2000 pages (~131 MB) and returns the
    // memory.grow result: the previous page count on success, or -1 if denied.
    const MEM_BOMB_CONTRACT: &str = r#"
    (module
        (import "env" "set_return_data" (func $srd (param i32 i32)))
        (memory (export "memory") 1)
        (func (export "call") (param i32 i32) (result i32)
            (i32.store (i32.const 0) (memory.grow (i32.const 2000)))
            (call $srd (i32.const 0) (i32.const 4))
            (i32.const 4)
        )
    )
    "#;

    /// Security (C-04): with the determinism fix active, WASM linear-memory
    /// growth is capped deterministically (memory.grow beyond the bound returns
    /// -1 identically on every node, independent of host RAM); with it off the
    /// growth is unbounded (legacy).
    #[test]
    fn determinism_fix_caps_memory_growth_when_active() {
        let wasm = wat::parse_str(MEM_BOMB_CONTRACT).expect("WAT parse failed");
        let code_hash = solen_crypto::blake3_hash(&wasm);

        // Strict (gate at 0): a block at height 5 >= 0 runs strict -> grow denied.
        let mut strict = VmRuntime::new().unwrap();
        strict.set_determinism_fix_height(0);
        let r = strict
            .execute(&code_hash, &wasm, &[], HostContext::new([0u8; 32], 5), None)
            .unwrap();
        assert_eq!(r.return_data, (-1i32).to_le_bytes(), "strict mode must deny the oversized growth");

        // Legacy (gate u64::MAX): height 5 < MAX -> legacy -> grow succeeds
        // (returns previous size = 1 page).
        let legacy = VmRuntime::new().unwrap();
        let r2 = legacy
            .execute(&code_hash, &wasm, &[], HostContext::new([0u8; 32], 5), None)
            .unwrap();
        assert_eq!(r2.return_data, 1i32.to_le_bytes(), "legacy mode allows the growth");
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
