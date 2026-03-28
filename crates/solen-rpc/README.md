# solen-rpc

JSON-RPC server for the Solen node, built on jsonrpsee.

## Methods

| Method | Parameters | Description |
|--------|-----------|-------------|
| `solen_chainStatus` | — | Height, state root, pending ops |
| `solen_getBalance` | `account_id` (hex) | Account balance |
| `solen_getAccount` | `account_id` (hex) | Full account info |
| `solen_getBlock` | `height` (u64) | Block by height |
| `solen_getLatestBlock` | — | Latest finalized block |
| `solen_submitOperation` | `UserOperation` | Submit to mempool |
| `solen_simulateOperation` | `UserOperation` | Dry-run simulation |

## Usage

```rust
use solen_rpc::server::start_rpc_server;

let handle = start_rpc_server("127.0.0.1:9944".parse()?, engine.clone()).await?;
```

```bash
curl -X POST http://127.0.0.1:9944 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"solen_chainStatus","params":[],"id":1}'
```
