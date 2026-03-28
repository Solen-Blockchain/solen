# Solen Devnet

Local development network launcher.

## Quick Start

```bash
./launch.sh
```

This builds the workspace and starts a single-validator node with default settings.

## Configuration

Edit `devnet-config.toml`:

```toml
[network]
chain_id = 1337
listen_addr = "127.0.0.1:9944"

[consensus]
block_time_ms = 2000
validators = 1

[rpc]
enabled = true
addr = "127.0.0.1:8545"
```

## Manual Start

```bash
cargo run --bin solen-node -- --no-p2p --block-time 1000 --in-memory
```
