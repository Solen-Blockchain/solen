# solen-node

The Solen validator node binary. Wires together consensus, execution, networking, RPC, and indexer into a single process.

## Usage

```bash
cargo run --bin solen-node
```

See `solen-node --help` for all options:

```
Options:
    --rpc-port <PORT>            JSON-RPC port [default: 9944]
    --p2p-port <PORT>            P2P port [default: 30333]
    --data-dir <DIR>             RocksDB path [default: data/solen-db]
    --block-time <MS>            Block interval [default: 2000]
    --bootstrap <MULTIADDR>      Peer address (repeatable)
    --validator-seed <HEX>       32-byte validator key seed
    --no-p2p                     Single-node mode
    --in-memory                  No persistence
    --explorer-port <PORT>       Explorer API port [default: 9955]
```

## Genesis Accounts

On first run, the node creates three devnet accounts:

| Name | Balance | Seed (for CLI import) |
|------|---------|----------------------|
| faucet | 1,000,000,000 | `2a` repeated 32 times |
| alice | 10,000 | `0a` repeated 32 times |
| bob | 5,000 | (no key — receive only) |

## Multi-Node

```bash
# Node A
solen-node

# Node B
solen-node --rpc-port 9945 --p2p-port 30334 --data-dir data/db2 \
  --explorer-port 9956 --bootstrap /ip4/127.0.0.1/tcp/30333
```

## Features

- `rocksdb` (default) — persistent storage via RocksDB
- Pass `--no-default-features` to compile without RocksDB
