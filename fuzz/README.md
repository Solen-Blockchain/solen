# Solen Fuzz Targets

Fuzz testing infrastructure using `cargo-fuzz` (libFuzzer).

## Targets

| Target | What it fuzzes | What it catches |
|--------|---------------|-----------------|
| `fuzz_executor` | Random `UserOperation`s through the block executor | Panics, overflows, state corruption |
| `fuzz_vm` | Random bytes as WASM bytecode into wasmtime | VM crashes, resource exhaustion |
| `fuzz_tx_decode` | Random bytes deserialized as transactions | Serde panics, malformed input handling |

## Running

```bash
cargo install cargo-fuzz

# Run a target for 5 minutes
cargo fuzz run fuzz_executor -- -max_total_time=300

# Run with more parallelism
cargo fuzz run fuzz_vm -- -max_total_time=300 -jobs=4 -workers=4

# Run all targets
for target in fuzz_executor fuzz_vm fuzz_tx_decode; do
  cargo fuzz run $target -- -max_total_time=60
done
```

## Corpus

Interesting inputs are saved to `corpus/<target>/`. These persist across runs so the fuzzer builds on previous discoveries.

## Crashes

Crash-inducing inputs are saved to `artifacts/<target>/`. Reproduce with:

```bash
cargo fuzz run fuzz_executor artifacts/fuzz_executor/crash-<hash>
```
