<p align="center">
  <img src="solenlogo.png" alt="Solen" width="400" />
</p>

<h1 align="center">Solen</h1>

A modular settlement network with native rollups, smart accounts, privacy primitives, and intent-aware execution.

Solen narrows the responsibilities of the base layer and treats execution domains as first-class components. The design combines a minimal settlement chain, native rollups, smart accounts by default, privacy-capable proof interfaces, and intent-oriented transaction flow.

> **Status:** Development prototype (v0.1.0). Not audited. Not for production use.
>
> **[Tokenomics](TOKENOMICS.md)** | **[Build Your First dApp](docs/tutorial-build-your-first-dapp.md)** | **[Deploy a Testnet](deploy/README.md)**

> **Getting started?** Follow the [Build Your First dApp](docs/tutorial-build-your-first-dapp.md) tutorial to deploy a token contract in 15 minutes.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Solen Node                               │
│                                                                 │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌───────────────┐  │
│  │Consensus │  │Execution │  │   WASM   │  │  System       │  │
│  │  Engine  │◄─┤  Engine  │◄─┤    VM    │  │  Contracts    │  │
│  │  (BFT)   │  │          │  │(wasmtime)│  │               │  │
│  └────┬─────┘  └────┬─────┘  └──────────┘  │ ■ Staking     │  │
│       │              │                       │ ■ Bridge      │  │
│  ┌────┴─────┐  ┌────┴─────┐                │ ■ Governance  │  │
│  │   P2P    │  │ Storage  │                │ ■ Treasury    │  │
│  │(libp2p)  │  │(RocksDB) │                └───────────────┘  │
│  └──────────┘  └──────────┘                                    │
│                                                                 │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐                     │
│  │ JSON-RPC │  │ Indexer  │  │ Explorer │                     │
│  │  :9944   │  │          │  │ API :9955│                     │
│  └──────────┘  └──────────┘  └──────────┘                     │
└─────────────────────────────────────────────────────────────────┘
```

**Settlement layer** — BFT proof-of-stake consensus with deterministic finality, round-robin proposers, 2/3+ stake-weighted quorum, epoch-based rewards, and slashing for double-sign and downtime.

**Execution engine** — Processes user operations (transfer, contract call, deploy), manages account state, verifies signatures, deducts fees, and supports transaction simulation.

**WASM VM** — Contracts execute as WASM bytecode via wasmtime. Host functions provide storage read/write, event emission, caller identity, and block height. Gas metering uses wasmtime's fuel mechanism.

**Smart accounts** — Every account is programmable. No externally owned accounts. Supports Ed25519 keys, passkeys, threshold authorization, guardians, and session credentials.

---

## Repository Layout

```
solen/
├── crates/
│   ├── solen-types/             # Core types: Hash, AccountId, BlockHeader, UserOperation
│   ├── solen-crypto/            # Ed25519 signing, BLAKE3 hashing
│   ├── solen-storage/           # StateStore trait + MemoryStore + RocksDB backend
│   ├── solen-consensus/         # BFT engine, validator set, epochs, slashing, checkpoints
│   ├── solen-execution/         # Block executor, state manager, fees, genesis
│   ├── solen-vm/                # Wasmtime WASM runtime with host functions
│   ├── solen-system-contracts/  # Staking, bridge, governance, treasury
│   ├── solen-rollup-kit/        # Sequencer, batch publisher, prover, messenger
│   ├── solen-p2p/               # libp2p gossipsub networking
│   ├── solen-rpc/               # JSON-RPC server (jsonrpsee)
│   ├── solen-indexer/           # Event indexer + explorer REST API
│   └── solen-node/              # Node binary (ties everything together)
├── sdks/
│   ├── wallet-sdk-rs/           # Rust wallet SDK
│   └── wallet-sdk-ts/           # TypeScript wallet SDK
├── tools/
│   ├── explorer/                # Next.js block explorer (frontend)
│   └── devnet/                  # Local devnet launcher
├── fuzz/                        # Fuzz targets (executor, VM, tx decode)
├── specs/                       # Protocol specifications
└── audits/                      # Security audit reports (placeholder)
```

---

## Quick Start

### Prerequisites

- Rust 1.78+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- C build tools (`build-essential`, `clang`)

### Build

```bash
cargo build --workspace
```

> **Note:** On some systems, RocksDB compilation requires:
> ```bash
> export C_INCLUDE_PATH=/usr/lib/gcc/x86_64-linux-gnu/11/include
> ```

### Run Tests

```bash
cargo test --workspace
```

135 tests covering storage, crypto, execution, consensus, VM, system contracts, rollup kit, intents, wallet SDK, and 4 property-based invariant tests.

### Start a Node

```bash
cargo run --bin solen-node
```

This starts a single-validator devnet with:
- JSON-RPC on `http://127.0.0.1:29944`
- Explorer API on `http://127.0.0.1:29955`
- P2P on port `50333`
- RocksDB persistence at `data/devnet`
- 2-second block times
- Three genesis accounts (faucet with 1B tokens, alice with 10K, bob with 5K)

### Network Environments

Use `--network` to select the environment. Each sets default ports, data directory, and block time:

```bash
solen-node --network devnet    # default
solen-node --network testnet
solen-node --network mainnet
```

| | RPC | P2P | Explorer | Data Directory | Block Time |
|---|---|---|---|---|---|
| **mainnet** | 9944 | 30333 | 9955 | `data/mainnet` | 6s |
| **testnet** | 19944 | 40333 | 19955 | `data/testnet` | 2s |
| **devnet** | 29944 | 50333 | 29955 | `data/devnet` | 2s |

Any default can be overridden explicitly:

```bash
solen-node --network testnet --rpc-port 8888
```

### CLI Options

```
solen-node [OPTIONS]

Options:
    --network <NETWORK>            devnet, testnet, or mainnet [default: devnet]
    --rpc-port <PORT>              JSON-RPC port
    --p2p-port <PORT>              P2P listen port
    --data-dir <DIR>               RocksDB data directory
    --block-time <MS>              Block interval in ms
    --bootstrap <MULTIADDR>        Bootstrap peer address (repeatable)
    --validator-seed <HEX>         32-byte hex seed for validator key
    --no-p2p                       Disable P2P networking
    --in-memory                    Use in-memory storage (no persistence)
    --explorer-port <PORT>         Explorer API port (0 to disable)
```

### Multi-Node Devnet

```bash
# Terminal 1 — Node A
cargo run --bin solen-node

# Terminal 2 — Node B (auto-avoids port conflicts with different data dir)
cargo run --bin solen-node -- \
    --rpc-port 29945 \
    --p2p-port 50334 \
    --data-dir data/devnet-2 \
    --explorer-port 29956 \
    --bootstrap /ip4/127.0.0.1/tcp/50333
```

---

## JSON-RPC API

All methods use standard JSON-RPC 2.0 over HTTP POST to the RPC port.

| Method | Parameters | Description |
|--------|-----------|-------------|
| `solen_chainStatus` | — | Chain height, state root, pending op count |
| `solen_getBalance` | `account_id` (hex) | Account balance as string |
| `solen_getAccount` | `account_id` (hex) | Full account info (balance, nonce, code_hash) |
| `solen_getBlock` | `height` (u64) | Block header and execution summary |
| `solen_getLatestBlock` | — | Latest finalized block |
| `solen_submitOperation` | `UserOperation` | Submit a signed operation to the mempool |
| `solen_simulateOperation` | `UserOperation` | Dry-run without state changes |

**Example:**

```bash
curl -s -X POST http://127.0.0.1:9944 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"solen_chainStatus","params":[],"id":1}'
```

---

## Explorer REST API

Available on the explorer port (default `9955`).

| Endpoint | Description |
|----------|-------------|
| `GET /api/status` | Indexing status (height, block/tx/event counts) |
| `GET /api/blocks?limit=N` | Recent blocks |
| `GET /api/blocks/{height}` | Block by height |
| `GET /api/accounts/{id}/txs?limit=N` | Account transaction history |
| `GET /api/events?limit=N` | Recent events |

---

## Smart Contracts

Solen contracts are WASM modules compiled from any language that targets WebAssembly. Contracts interact with chain state through host functions.

### Host Functions

| Function | Signature | Description |
|----------|-----------|-------------|
| `storage_read` | `(key_ptr, key_len, val_ptr) -> i32` | Read from contract storage |
| `storage_write` | `(key_ptr, key_len, val_ptr, val_len)` | Write to contract storage |
| `emit_event` | `(topic_ptr, topic_len, data_ptr, data_len)` | Emit an event |
| `get_caller` | `(out_ptr)` | Get the caller's 32-byte account ID |
| `get_block_height` | `() -> i64` | Current block height |
| `set_return_data` | `(ptr, len)` | Set return data |

### Contract Interface

Contracts must export:
- `memory` — linear memory
- `call(input_ptr: i32, input_len: i32) -> i32` — entry point, returns output length

### Deploy and Call

Contracts are deployed via `Action::Deploy` and called via `Action::Call` within a `UserOperation`. Gas is metered using wasmtime's fuel mechanism.

---

## System Contracts

### Staking

Validators register with a minimum stake (1,000 tokens). Delegators stake to validators and share epoch rewards proportionally. Undelegation has a 7-epoch cooldown.

### Bridge

Each rollup has a bridge vault. Deposits are instant. Withdrawals go through a challenge window (100 blocks) plus a withdrawal delay (50 blocks). Disputed withdrawals are blocked.

### Governance

Proposals require 30% quorum and 66.67% supermajority to pass. Passed proposals have a 3-epoch timelock before execution. Supports parameter changes, rollup registration, and emergency pause/resume.

---

## Fee Model

Each block has a configurable base fee per gas unit. After an operation executes, fees are deducted from the sender's balance. A configurable portion is burned and the remainder credited to the treasury account.

| Parameter | Default |
|-----------|---------|
| Base fee per gas | 1 |
| Burn rate | 50% |
| Treasury share | 50% |

---

## Security

### Testing

- **Unit tests:** 76 tests across all crates
- **Property-based tests:** Supply conservation, nonce monotonicity, state root determinism, no negative balances (via proptest)
- **Fuzz targets:** Executor, WASM VM, and transaction deserialization

### Running Fuzz Tests

```bash
cargo install cargo-fuzz
cd fuzz
cargo fuzz run fuzz_executor -- -max_total_time=300
cargo fuzz run fuzz_vm -- -max_total_time=300
cargo fuzz run fuzz_tx_decode -- -max_total_time=300
```

### Slashing

Validators are slashed for:
- **Double signing** — 10% stake penalty + jailing
- **Downtime** — 1% penalty after 50 consecutive missed blocks

---

## TypeScript SDK

```bash
cd sdks/wallet-sdk-ts
npm install
npm run build
```

```typescript
import { SolenClient, SmartAccount } from "@solen/wallet-sdk";

const client = new SolenClient({ rpcUrl: "http://127.0.0.1:9944" });

// Query chain status
const status = await client.chainStatus();
console.log(`Height: ${status.height}`);

// Query account
const balance = await client.getBalance("616c69636500...");

// Build and submit a transfer
const alice = new SmartAccount("616c69636500...", client);
const op = await alice.buildTransfer("626f6200...", 500);
// ... sign op.signature with Ed25519 ...
const result = await alice.submit(op);
```

---

## Design Principles

| Principle | Implication |
|-----------|-------------|
| Minimal base layer | L1 handles consensus, settlement, and verification — not general-purpose execution |
| Native modularity | Rollups, proofs, and messaging are protocol-native |
| User safety by default | Every account is a smart account with recovery and policy |
| Bounded privacy | Proof systems allow selective disclosure without full transparency |
| Decentralization over throughput | Credible settlement over single-chain benchmarks |

---

## Project Stats

| Metric | Value |
|--------|-------|
| Rust crates | 17 |
| TypeScript packages | 2 |
| Rust source lines | ~18,000 |
| Test functions | 135 |
| Property tests | 4 (hundreds of cases each) |
| Fuzz targets | 3 |
| RPC methods | 7 |
| Explorer endpoints | 5 |
| Transfer TPS | ~14,000 |
| Contract call TPS | ~10,000 |

---

## License

Licensed under MIT OR Apache-2.0 at your option.
