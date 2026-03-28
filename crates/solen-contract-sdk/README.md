# solen-contract-sdk

Rust SDK for writing Solen smart contracts that compile to WASM.

## Quick Start

```rust
#![no_std]
use solen_contract_sdk::{sdk, storage, events};

#[no_mangle]
pub extern "C" fn call(_input_ptr: i32, _input_len: i32) -> i32 {
    let count = storage::get_u64(b"count").unwrap_or(0) + 1;
    storage::set_u64(b"count", count);
    events::emit(b"incremented", &count.to_le_bytes());
    sdk::return_value(&count.to_le_bytes())
}
```

Build:
```bash
cargo build --target wasm32-unknown-unknown --release
```

Deploy:
```bash
solen deploy mykey target/wasm32-unknown-unknown/release/my_contract.wasm
```

## Modules

### `sdk`
| Function | Description |
|----------|-------------|
| `read_input(ptr, len)` | Read input bytes passed to `call` |
| `return_value(data)` | Set return data and return its length |
| `caller()` | Get the 32-byte caller account ID |
| `block_height()` | Get the current block height |

### `storage`
| Function | Description |
|----------|-------------|
| `get(key)` / `set(key, value)` | Raw byte storage |
| `get_u64(key)` / `set_u64(key, value)` | u64 convenience |
| `get_u128(key)` / `set_u128(key, value)` | u128 convenience |

### `events`
| Function | Description |
|----------|-------------|
| `emit(topic, data)` | Emit an event with topic and data |

## Notes

- `#![no_std]` — contracts run in WASM with no standard library
- Stack allocation only — the default allocator is a no-op stub
- Contracts must export `call(i32, i32) -> i32` and `memory`
