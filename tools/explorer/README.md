# Solen Explorer

Block explorer frontend for the Solen network, built with Next.js.

## Features

- Live chain status dashboard (height, blocks, txs, events)
- Recent blocks table with auto-refresh
- Connects to the indexer REST API

## Setup

```bash
npm install
npm run dev
```

Open `http://localhost:3000`.

## Configuration

Set the API URL via environment variable:

```bash
NEXT_PUBLIC_API_URL=http://127.0.0.1:9955 npm run dev
```

Default: `http://127.0.0.1:9955` (the node's `--explorer-port`).

## Requirements

The Solen node must be running with the indexer enabled (default). The explorer reads from the indexer REST API, not directly from the node RPC.
