# solen-types

Core types shared across all Solen crates.

## Key Types

| Type | Description |
|------|-------------|
| `Hash` | `[u8; 32]` — BLAKE3 hash |
| `AccountId` | `[u8; 32]` — account identifier |
| `ValidatorId` | `[u8; 32]` — validator identifier |
| `RollupId` | `u64` — rollup domain identifier |
| `BlockHeight` | `u64` |
| `Epoch` | `u64` |

## Modules

- **`block`** — `BlockHeader` with height, epoch, state root, proposer, timestamps
- **`account`** — `Account`, `AuthMethod` (Ed25519, Passkey, Threshold, Guardian)
- **`transaction`** — `UserOperation`, `Action` (Transfer, Call, Deploy), `Intent`
- **`crypto`** — `Signature`, `ZkProof`
- **`rollup`** — `RollupRegistration`, `BatchCommitment`, `BridgeConfig`, `ProofType`

## Usage

```rust
use solen_types::{AccountId, Hash, BlockHeight};
use solen_types::transaction::{UserOperation, Action};
use solen_types::account::{Account, AuthMethod};
```
