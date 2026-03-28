# solen

Command-line interface for interacting with the Solen network. Query chain state, manage keys, send transfers, deploy contracts, and call contract methods — all from the terminal.

## Installation

From the workspace root:

```bash
cargo build --bin solen --release
```

The binary is at `target/release/solen`. Copy it to your PATH or run via `cargo run --bin solen`.

## Quick Start

```bash
# Start a local node (separate terminal)
cargo run --bin solen-node

# Generate a key
solen key generate mykey

# Import a devnet key (faucet has 1B tokens)
solen key import faucet 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a

# Check status
solen status

# Check balance
solen balance faucet

# Send tokens
solen transfer faucet bob 5000
```

## Global Options

| Option | Default | Description |
|--------|---------|-------------|
| `--rpc <URL>` | `http://127.0.0.1:9944` | JSON-RPC endpoint of the Solen node |

```bash
# Connect to a different node
solen --rpc http://10.0.0.5:9944 status
```

---

## Commands

### `solen status`

Show current chain status.

```bash
$ solen status
Solen Network Status
────────────────────────────────────────
  Height:      42
  State root:  78c88b3eb083...805d24e9
  Pending ops: 0
  Epoch:       0
  Proposer:    8a88e3dd7409f195...
  Gas used:    0
```

---

### `solen balance <account>`

Get the token balance of an account. The account can be a key name, a human-readable name, or a 64-character hex ID.

```bash
$ solen balance faucet
1000000000

$ solen balance alice
10000

$ solen balance 626f620000000000000000000000000000000000000000000000000000000000
5000
```

---

### `solen account <account>`

Get full account details including nonce and code hash.

```bash
$ solen account alice
Account
────────────────────────────────────────
  ID:        616c696365000000000000000000000000000000000000000000000000000000
  Balance:   10000
  Nonce:     0
  Code hash: (none)
```

The code hash shows `(none)` for non-contract accounts and a truncated hash for deployed contracts.

---

### `solen block [height]`

Get block information. Shows the latest block if no height is given.

```bash
$ solen block
Block #42
────────────────────────────────────────
  Epoch:      0
  Proposer:   8a88e3dd7409f195...
  State root: 78c88b3eb08313c6...
  Txs:        0
  Gas used:   0
  Time:       2s ago

$ solen block 1
Block #1
────────────────────────────────────────
  Epoch:      0
  Proposer:   8a88e3dd7409f195...
  State root: 78c88b3eb08313c6...
  Txs:        0
  Gas used:   0
  Time:       84s ago
```

---

### `solen transfer <from> <to> <amount>`

Transfer tokens between accounts. The `from` account must be a key name in your keystore. The `to` account can be a name or hex ID.

The CLI automatically:
1. Looks up the sender's current nonce
2. Builds and signs the operation
3. Simulates against current state
4. Submits if simulation succeeds

```bash
$ solen transfer faucet bob 5000
Simulated OK (gas: 100). Submitting...
Transaction submitted successfully.
  From:   faucet (666175636574...)
  To:     bob (626f62000000...)
  Amount: 5000

$ solen transfer faucet alice 999999999
Simulation failed: insufficient balance: have 999995000, need 999999999
```

---

### `solen deploy <from> <wasm-file>`

Deploy a WASM smart contract. The `from` account must be a key name in your keystore.

The CLI:
1. Reads the WASM file
2. Generates a deterministic salt from sender + nonce
3. Predicts the contract address
4. Simulates, then submits

```bash
$ solen deploy faucet target/wasm32-unknown-unknown/release/my_contract.wasm
Simulated OK (gas: 1000). Deploying...
Contract deployed successfully.
  Contract ID: a7f3b2c1d4e5f6...
  Code hash:   9c8d7e6f5a4b3c...
```

To build an example contract:

```bash
cd examples/contracts/counter
cargo build --target wasm32-unknown-unknown --release
```

---

### `solen call <from> <target> <method> [--args <hex>]`

Call a method on a deployed contract. The `from` account must be a key name. The `target` can be a name or hex ID.

```bash
$ solen call faucet a7f3b2c1d4e5f6... increment
Simulated OK (gas: 1523). Submitting...
Call submitted successfully.
  Target: a7f3b2c1d4e5f6...
  Method: increment

$ solen call faucet mycontract transfer --args deadbeef
Simulated OK (gas: 2100). Submitting...
Call submitted successfully.
  Target: 6d79636f6e7472...
  Method: transfer
```

The `--args` flag accepts raw bytes as a hex string. Omit it for methods that take no arguments.

---

### `solen key generate <name>`

Generate a new Ed25519 keypair and save it to the local keystore.

```bash
$ solen key generate alice
Generated key 'alice'
  Account ID:  616c696365000000000000000000000000000000000000000000000000000000
  Public key:  139c31e8543b19629ea93c90b291d684aec0ca432cc0efda170570572c62e519
  Seed:        a41ef126b339c090fdf0090ca9a094cf6551a6a5d354779b22c1022606ef9a55 (SAVE THIS!)

Saved to ~/.solen/keys.json
```

**Important:** The seed is the only way to recover the key. Save it securely. It is not displayed again.

The account ID is derived from the key name (padded to 32 bytes). This matches the devnet genesis accounts (e.g., `alice` maps to `616c6963650000...`).

---

### `solen key import <name> <seed>`

Import a keypair from a 32-byte hex seed.

```bash
$ solen key import faucet 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a
Imported key 'faucet'
  Account ID: 6661756365740000000000000000000000000000000000000000000000000000
  Public key: 197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d61
```

**Devnet seeds:**

| Account | Seed |
|---------|------|
| faucet (1B tokens) | `2a2a...2a` (32 bytes of `0x2a`) |
| alice (10K tokens) | `0a0a...0a` (32 bytes of `0x0a`) |
| validator | `0101...01` (32 bytes of `0x01`) |

---

### `solen key list`

List all keys in the local keystore.

```bash
$ solen key list
NAME         ACCOUNT ID           PUBLIC KEY
──────────────────────────────────────────────────────────────────────
alice        616c696365000000...  139c31e8543b1962...
faucet       6661756365740000...  197f6b23e16c8532...
```

---

## Account Resolution

All commands that take an `<account>` argument support three formats:

| Format | Example | Resolution |
|--------|---------|------------|
| Key name | `alice` | Looks up `~/.solen/keys.json` first, falls back to name-to-hex |
| Human name | `bob` | Converts to 32-byte zero-padded hex (`626f6200...`) |
| Hex ID | `616c6963...` | Used directly (must be exactly 64 hex chars) |

This means `solen balance alice`, `solen balance 616c696365000000000000000000000000000000000000000000000000000000`, and a locally-stored key named `alice` all resolve to the same account.

---

## Keystore

Keys are stored in `~/.solen/keys.json`. The file contains key names, seeds (hex), public keys, and derived account IDs.

```json
{
  "keys": {
    "alice": {
      "name": "alice",
      "seed_hex": "a41ef126b339...",
      "public_key_hex": "139c31e8543b...",
      "account_id_hex": "616c696365000000..."
    }
  }
}
```

**Security note:** Seeds are stored in plaintext. This is suitable for development. Production wallets should encrypt seeds at rest.

---

## Full Workflow Example

```bash
# 1. Start a node
cargo run --bin solen-node

# 2. Import the faucet key (has 1B tokens on devnet)
solen key import faucet 2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a

# 3. Check faucet balance
solen balance faucet
# 1000000000

# 4. Fund bob
solen transfer faucet bob 50000
# Simulated OK (gas: 100). Submitting...
# Transaction submitted successfully.

# 5. Check bob's balance (wait for next block)
sleep 3
solen balance bob
# 55000

# 6. Deploy a contract
solen deploy faucet examples/contracts/counter/target/wasm32-unknown-unknown/release/solen_example_counter.wasm
# Contract deployed successfully.
#   Contract ID: a7f3b2c1...

# 7. Call the contract
solen call faucet a7f3b2c1... increment
# Call submitted successfully.

# 8. Check chain status
solen status
```

---

## License

MIT OR Apache-2.0
