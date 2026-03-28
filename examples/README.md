# Solen Examples

Example smart contracts and usage patterns.

## Contracts

### Counter (`contracts/counter/`)

A minimal contract that maintains a counter in storage. Demonstrates storage read/write, event emission, and return data.

```bash
cd contracts/counter
cargo build --target wasm32-unknown-unknown --release
```

Deploy and call:

```bash
solen key import mykey <seed>
solen deploy mykey target/wasm32-unknown-unknown/release/solen_example_counter.wasm
solen call mykey <contract-id> increment
```

## Writing Your Own Contract

1. Create a new crate with `crate-type = ["cdylib"]`
2. Add `solen-contract-sdk` as a dependency
3. Export a `call(i32, i32) -> i32` function
4. Build with `--target wasm32-unknown-unknown --release`
5. Deploy with `solen deploy`

See `contracts/counter/src/lib.rs` for the simplest possible example.
