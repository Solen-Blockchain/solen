# solen-vm

WASM virtual machine for Solen smart contract execution, powered by wasmtime.

## Host Functions

Contracts import these from the `"env"` module:

| Function | Signature | Description |
|----------|-----------|-------------|
| `storage_read` | `(key_ptr, key_len, val_ptr) -> i32` | Read from contract storage (-1 if missing) |
| `storage_write` | `(key_ptr, key_len, val_ptr, val_len)` | Write to contract storage |
| `emit_event` | `(topic_ptr, topic_len, data_ptr, data_len)` | Emit an event |
| `get_caller` | `(out_ptr)` | Write 32-byte caller ID |
| `get_block_height` | `() -> i64` | Current block height |
| `set_return_data` | `(ptr, len)` | Set return data |

## Contract Interface

Contracts must export:
- `memory` — linear memory
- `call(input_ptr: i32, input_len: i32) -> i32` — entry point

## Gas Metering

Uses wasmtime's fuel mechanism. Default limit: 1,000,000 fuel units. Fuel maps 1:1 to Solen gas.

## Usage

```rust
use solen_vm::host::HostContext;
use solen_vm::runtime::execute;

let ctx = HostContext::new(caller_id, block_height);
let result = execute(&wasm_bytecode, &input, ctx, Some(100_000))?;
// result.gas_used, result.return_data, result.events, result.storage
```

See `solen-contract-sdk` for the Rust SDK that makes writing contracts ergonomic.
