# SRC-20: Solen Token Standard

An ERC20-equivalent fungible token contract for the Solen network. Supports minting (owner only), transfers, allowance-based approvals, and transferFrom.

## Build

```bash
# Install the WASM target (one time)
rustup target add wasm32-unknown-unknown

# Build the contract
cargo build --target wasm32-unknown-unknown --release
```

The compiled WASM is at:
```
target/wasm32-unknown-unknown/release/solen_example_token.wasm
```

## Deploy

```bash
solen deploy mykey target/wasm32-unknown-unknown/release/solen_example_token.wasm
# Contract ID: <TOKEN_ID>
```

## Initialize

After deploying, call `init` to set yourself as the token owner:

```bash
solen call mykey <TOKEN_ID> init
```

## Methods

### `mint` — Mint tokens (owner only)

Args: `to[32 bytes] + amount[16 bytes, little-endian u128]`

```bash
# Mint 1,000,000 tokens to alice
solen call mykey <TOKEN_ID> mint --args <alice_id_hex><amount_hex>
```

### `transfer` — Transfer tokens

Args: `to[32 bytes] + amount[16 bytes]`

### `approve` — Set spending allowance

Args: `spender[32 bytes] + amount[16 bytes]`

### `transfer_from` — Transfer using allowance

Args: `from[32 bytes] + to[32 bytes] + amount[16 bytes]`

### `balance_of` — Query balance

Args: `account[32 bytes]` — returns `u128` (16 bytes, little-endian)

### `allowance` — Query allowance

Args: `owner[32 bytes] + spender[32 bytes]` — returns `u128`

### `total_supply` — Query total supply

No args — returns `u128`

## Events

| Event | Data | Emitted by |
|-------|------|-----------|
| `initialized` | owner account ID | `init` |
| `mint` | amount (u128 LE) | `mint` |
| `transfer` | amount (u128 LE) | `transfer`, `transfer_from` |
| `approval` | amount (u128 LE) | `approve` |

## Comparison with ERC20

| ERC20 | SRC-20 | Notes |
|-------|--------|-------|
| `name()` | (set at deploy) | Metadata stored off-chain or in future extension |
| `symbol()` | (set at deploy) | Same |
| `decimals()` | (convention: 18) | Same |
| `totalSupply()` | `total_supply` | Returns u128 |
| `balanceOf(address)` | `balance_of` | Takes 32-byte account ID |
| `transfer(to, amount)` | `transfer` | Same semantics |
| `approve(spender, amount)` | `approve` | Same semantics |
| `transferFrom(from, to, amount)` | `transfer_from` | Same semantics |
| `allowance(owner, spender)` | `allowance` | Same semantics |
