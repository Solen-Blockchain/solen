# Solen Management Scripts

Interactive tools for setting up, running, and managing Solen validator nodes.

## Quick Start

```bash
./scripts/solen-manage.sh
```

## Usage

```bash
# Default network (mainnet)
./scripts/solen-manage.sh

# Specify network
./scripts/solen-manage.sh --network devnet
./scripts/solen-manage.sh --network testnet

# Or via environment variable
SOLEN_NETWORK=devnet ./scripts/solen-manage.sh
```

## Menu Options

### Setup & Installation

| # | Option | Description |
|---|--------|-------------|
| 1 | Install dependencies | Installs Rust, build tools, and system packages |
| 2 | Build from source | Compiles solen-node and solen CLI in release mode |
| 3 | Initialize genesis config | Creates genesis.json in the data directory |
| 4 | Configure systemd service | Generates and installs a systemd unit file |

### Node Operations

| # | Option | Description |
|---|--------|-------------|
| 5 | Start node | Start the solen-node systemd service |
| 6 | Stop node | Stop the service |
| 7 | Restart node | Restart with current binary |
| 8 | View live logs | Stream logs via journalctl (Ctrl+C to exit) |
| 9 | Node health check | Shows height, epoch, mempool, and block production rate |

### Validator Management

| # | Option | Description |
|---|--------|-------------|
| 10 | Generate validator key | Create a new Ed25519 keypair in the local keystore |
| 11 | List keys | Show all keys in the keystore |
| 12 | Register as validator | Register with self-stake (min 500,000 SOLEN) |
| 13 | View validator set | List all validators with stake and status |
| 14 | Stake tokens | Delegate tokens to a validator |
| 15 | Unstake tokens | Begin undelegation (subject to unbonding period) |
| 16 | Withdraw matured stake | Claim tokens after unbonding completes |
| 17 | Unjail validator | Reactivate after downtime slash |

### Monitoring

| # | Option | Description |
|---|--------|-------------|
| 18 | Chain status | Current height, epoch, state root, proposer |
| 19 | Network parameters | Governance-configurable values (block time, min stake, etc.) |
| 20 | Check balance | Query any account balance by name, Base58, or hex |

### Maintenance

| # | Option | Description |
|---|--------|-------------|
| 21 | Backup data directory | Safely copy the data directory (stops node if running) |
| 22 | Wipe & resync | Delete chain data and resync from peers via snapshot |
| 23 | Update binary | Git pull, rebuild, and optionally restart the node |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `SOLEN_NETWORK` | `mainnet` | Network to connect to (devnet, testnet, mainnet) |
| `SOLEN_DIR` | Parent of scripts/ | Path to the solen repository root |
| `SOLEN_DATA_DIR` | `/opt/solen/data` | Default data directory for the node |

## Requirements

- Linux (Ubuntu/Debian recommended)
- Rust toolchain (installed via option 1)
- systemd (for service management)
- curl, python3 (for RPC queries)
