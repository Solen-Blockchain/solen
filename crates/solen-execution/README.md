# solen-execution

Settlement execution engine: processes user operations, manages state, verifies proofs, and handles fees.

## Modules

| Module | Description |
|--------|-------------|
| `executor` | `BlockExecutor` — validates signatures, executes actions (transfer, call, deploy), deducts fees |
| `state` | `StateManager` / `ReadonlyStateManager` — account CRUD, balances, nonces, contract storage |
| `fees` | `FeeConfig` — base fee per gas, burn rate, treasury crediting |
| `genesis` | `apply_genesis()` — initialize state with genesis accounts |
| `proof` | `ProofVerifierRegistry` — verify rollup batch commitments with pluggable backends |
| `receipt` | `ExecutionReceipt`, `BlockResult` — execution outcomes and events |

## Key Flow

```
UserOperation → validate signature → consume nonce → execute actions → deduct fees → receipt
```

Actions:
- **Transfer** — move tokens between accounts (100 gas)
- **Call** — execute WASM contract via wasmtime VM (500+ gas)
- **Deploy** — store bytecode and create contract account (1000 gas)

## Usage

```rust
use solen_execution::executor::BlockExecutor;
use solen_execution::fees::FeeConfig;

let executor = BlockExecutor::with_fee_config(FeeConfig::default());
let result = executor.execute_block(&mut store, &operations);
// result.receipts, result.state_root, result.gas_used

let receipt = executor.simulate(&store, &op); // dry-run, no state changes
```
