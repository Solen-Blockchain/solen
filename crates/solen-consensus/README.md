# solen-consensus

BFT proof-of-stake consensus engine for the Solen settlement layer.

## Features

- **Multi-validator BFT** — round-robin block proposers, 2/3+ stake-weighted quorum
- **Epoch transitions** — reward distribution, validator rotation every 100 blocks
- **Slashing** — double-sign (10% penalty) and downtime (1% after 50 missed blocks)
- **Mempool** — thread-safe operation pool with configurable capacity
- **Encrypted mempool** — commit-reveal scheme for MEV protection
- **Checkpoints** — periodic state snapshots for fast node sync

## Modules

| Module | Description |
|--------|-------------|
| `engine` | `ConsensusEngine` — block production, finalization, chain management |
| `validator` | `ValidatorSet` — active set, quorum checks, proposer selection, jailing |
| `epoch` | `EpochManager` — epoch boundaries, reward distribution |
| `slashing` | Double-sign detection, downtime tracking, penalty application |
| `mempool` | `Mempool` — pending operation collection |
| `encrypted_mempool` | `EncryptedMempool` — commit-reveal for frontrunning protection |
| `checkpoint` | `CheckpointStore` — periodic state snapshots |

## Usage

```rust
use solen_consensus::engine::{ConsensusEngine, EngineConfig};
use solen_consensus::mempool::Mempool;

let mempool = Mempool::new(10_000);
let engine = ConsensusEngine::new(EngineConfig::default(), store, mempool);
let block = engine.produce_block();
```
