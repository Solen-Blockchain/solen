# solen-indexer

Event indexer and REST API for the Solen block explorer.

## Components

- **Indexer** — polls the consensus engine for finalized blocks, extracts events and receipts into an in-memory store
- **REST API** — serves indexed data over HTTP for the explorer frontend

## API Endpoints

| Endpoint | Description |
|----------|-------------|
| `GET /api/status` | Index height, block/tx/event counts |
| `GET /api/blocks?limit=N` | Recent blocks |
| `GET /api/blocks/{height}` | Block by height |
| `GET /api/accounts/{id}/txs?limit=N` | Account transaction history |
| `GET /api/events?limit=N` | Recent events |

## Usage

```rust
use solen_indexer::indexer::run_indexer;
use solen_indexer::api::start_explorer_api;

// Start indexer (background task)
tokio::spawn(run_indexer(engine, index_store.clone(), cancel_rx));

// Start REST API
start_explorer_api("127.0.0.1:9955".parse()?, index_store).await?;
```

Default port: `9955` (configured via `--explorer-port` on the node).
