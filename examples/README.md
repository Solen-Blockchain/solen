# Solen Examples

Example smart contracts for the Solen network.

> **New here?** Follow the [Build Your First dApp](../docs/tutorial-build-your-first-dapp.md) tutorial for a guided walkthrough.

## Contracts

### Counter (`contracts/counter/`)

A minimal contract that maintains a counter in storage. Demonstrates `storage::get_u64` / `set_u64`, `events::emit`, and `sdk::return_value`.

### Token (`contracts/token/`)

An ERC20-equivalent (SRC-20) fungible token with minting, transfers, allowances, and transferFrom. Full implementation of the standard token interface.

## Building Contracts

```bash
# Install WASM target (one time)
rustup target add wasm32-unknown-unknown

# Build a contract
cd contracts/counter   # or contracts/token
cargo build --target wasm32-unknown-unknown --release
```

## Deploying

```bash
solen deploy mykey target/wasm32-unknown-unknown/release/solen_example_counter.wasm
```

## Writing Your Own

1. Create a new crate with `crate-type = ["cdylib"]`
2. Add `solen-contract-sdk` as a dependency
3. Add `#![no_std]` and export a `call(i32, i32) -> i32` function
4. Build with `--target wasm32-unknown-unknown --release`
5. Deploy with `solen deploy`

See the [Contract SDK README](../crates/solen-contract-sdk/README.md) for the full API.
