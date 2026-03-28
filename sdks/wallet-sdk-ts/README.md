# @solen/wallet-sdk

TypeScript client SDK for interacting with the Solen network. Provides RPC communication, smart account management, transaction building, and utility functions.

## Installation

```bash
npm install @solen/wallet-sdk
```

## Quick Start

```typescript
import { SolenClient, SmartAccount, nameToHex } from "@solen/wallet-sdk";

// Connect to a Solen node
const client = new SolenClient({ rpcUrl: "http://127.0.0.1:9944" });

// Check chain status
const status = await client.chainStatus();
console.log(`Chain height: ${status.height}`);

// Query an account
const aliceId = nameToHex("alice");
const balance = await client.getBalance(aliceId);
console.log(`Alice balance: ${balance}`);

// Work with a smart account
const alice = new SmartAccount(aliceId, client);
const op = await alice.buildTransfer(nameToHex("bob"), 500);
// ... sign op.signature with Ed25519 ...
const result = await alice.submit(op);
console.log(`Accepted: ${result.accepted}`);
```

---

## API Reference

### `SolenClient`

Low-level JSON-RPC client for communicating with a Solen node.

#### Constructor

```typescript
new SolenClient(config: SolenConfig)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `config.rpcUrl` | `string` | URL of the Solen node JSON-RPC endpoint |

#### Methods

##### `getBalance(accountId)`

Get the token balance of an account.

```typescript
const balance: bigint = await client.getBalance(
  "616c696365000000000000000000000000000000000000000000000000000000"
);
// balance = 10000n
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `accountId` | `string` | 32-byte account ID as hex string |
| **Returns** | `Promise<bigint>` | Account balance |

---

##### `getAccount(accountId)`

Get full account information including nonce and code hash.

```typescript
const info: AccountInfo = await client.getAccount(aliceId);
console.log(info.balance); // "10000"
console.log(info.nonce);   // 0
console.log(info.code_hash); // "0000...0000" (no contract code)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `accountId` | `string` | 32-byte account ID as hex string |
| **Returns** | `Promise<AccountInfo>` | Account details |

**`AccountInfo` fields:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | `string` | Account ID (hex) |
| `balance` | `string` | Balance as decimal string |
| `nonce` | `number` | Next expected nonce |
| `code_hash` | `string` | Contract code hash (hex), all zeros if not a contract |

---

##### `getBlock(height)`

Get a block by its height.

```typescript
const block: BlockInfo = await client.getBlock(1);
console.log(block.tx_count); // 0
console.log(block.gas_used); // 0
console.log(block.proposer); // "8a88e3dd..."
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `height` | `number` | Block height |
| **Returns** | `Promise<BlockInfo>` | Block details |

**`BlockInfo` fields:**

| Field | Type | Description |
|-------|------|-------------|
| `height` | `number` | Block height |
| `epoch` | `number` | Epoch number |
| `parent_hash` | `string` | Parent block hash (hex) |
| `state_root` | `string` | State root after execution (hex) |
| `transactions_root` | `string` | Merkle root of transactions (hex) |
| `receipts_root` | `string` | Merkle root of receipts (hex) |
| `proposer` | `string` | Validator ID that proposed the block (hex) |
| `timestamp_ms` | `number` | Unix timestamp in milliseconds |
| `tx_count` | `number` | Number of transactions in the block |
| `gas_used` | `number` | Total gas consumed |

---

##### `getLatestBlock()`

Get the most recently finalized block.

```typescript
const latest: BlockInfo = await client.getLatestBlock();
console.log(`Latest height: ${latest.height}`);
```

| **Returns** | `Promise<BlockInfo>` | Latest block details |

---

##### `chainStatus()`

Get current chain status including height, state root, and pending operation count.

```typescript
const status: ChainStatus = await client.chainStatus();
console.log(status.height);            // 42
console.log(status.latest_state_root); // "78c88b3e..."
console.log(status.pending_ops);       // 0
```

| **Returns** | `Promise<ChainStatus>` | Chain status |

**`ChainStatus` fields:**

| Field | Type | Description |
|-------|------|-------------|
| `height` | `number` | Current block height |
| `latest_state_root` | `string` | State root hash (hex) |
| `pending_ops` | `number` | Operations waiting in mempool |

---

##### `submitOperation(op)`

Submit a signed user operation to the mempool.

```typescript
const result: SubmitResult = await client.submitOperation(signedOp);
if (result.accepted) {
  console.log("Transaction submitted");
} else {
  console.error(`Rejected: ${result.error}`);
}
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `op` | `UserOperation` | Signed operation |
| **Returns** | `Promise<SubmitResult>` | Submission result |

**`SubmitResult` fields:**

| Field | Type | Description |
|-------|------|-------------|
| `accepted` | `boolean` | Whether the operation was accepted into the mempool |
| `error` | `string \| null` | Error message if rejected |

---

##### `simulateOperation(op)`

Simulate an operation against current state without modifying it. Useful for gas estimation and pre-flight checks.

```typescript
const sim: SimulationResult = await client.simulateOperation(op);
if (sim.success) {
  console.log(`Estimated gas: ${sim.gas_used}`);
  console.log(`Events: ${sim.events.length}`);
} else {
  console.error(`Would fail: ${sim.error}`);
}
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `op` | `UserOperation` | Operation to simulate (signature not required) |
| **Returns** | `Promise<SimulationResult>` | Simulation result |

**`SimulationResult` fields:**

| Field | Type | Description |
|-------|------|-------------|
| `success` | `boolean` | Whether execution would succeed |
| `gas_used` | `number` | Gas that would be consumed |
| `error` | `string \| null` | Error message if it would fail |
| `events` | `Array<{emitter, topic}>` | Events that would be emitted |

---

### `SmartAccount`

High-level smart account interface for building and submitting transactions.

#### Constructor

```typescript
new SmartAccount(accountIdHex: string, client: SolenClient)
```

| Parameter | Type | Description |
|-----------|------|-------------|
| `accountIdHex` | `string` | 32-byte account ID as hex string |
| `client` | `SolenClient` | Connected RPC client |

#### Properties

| Property | Type | Description |
|----------|------|-------------|
| `id` | `string` | Account ID (hex) |

#### Methods

##### `getInfo()`

Fetch full account info from the chain.

```typescript
const info = await alice.getInfo();
console.log(`Balance: ${info.balance}, Nonce: ${info.nonce}`);
```

---

##### `getBalance()`

Fetch current balance.

```typescript
const balance: bigint = await alice.getBalance();
```

---

##### `getNonce()`

Fetch current nonce (used to build the next operation).

```typescript
const nonce: number = await alice.getNonce();
```

---

##### `buildTransfer(toHex, amount, maxFee?)`

Build an unsigned transfer operation. Automatically fetches the current nonce.

```typescript
const op = await alice.buildTransfer(
  "626f620000000000000000000000000000000000000000000000000000000000",
  1000,   // amount
  5000    // maxFee (optional, default: 10000)
);
// op.signature is empty — sign it before submitting
```

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `toHex` | `string` | | Recipient account ID (hex) |
| `amount` | `number` | | Amount to transfer |
| `maxFee` | `number` | `10000` | Maximum fee willing to pay |
| **Returns** | `Promise<UserOperation>` | | Unsigned operation |

---

##### `buildCall(targetHex, method, args?, maxFee?)`

Build an unsigned contract call operation.

```typescript
const op = await alice.buildCall(
  contractIdHex,       // target contract
  "increment",         // method name
  [1, 2, 3],           // args as bytes (optional)
  50000                // maxFee (optional)
);
```

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `targetHex` | `string` | | Contract account ID (hex) |
| `method` | `string` | | Method name to invoke |
| `args` | `number[]` | `[]` | Argument bytes |
| `maxFee` | `number` | `50000` | Maximum fee |
| **Returns** | `Promise<UserOperation>` | | Unsigned operation |

---

##### `buildDeploy(code, salt, maxFee?)`

Build an unsigned contract deployment operation.

```typescript
const wasmBytes = Array.from(fs.readFileSync("contract.wasm"));
const salt = new Array(32).fill(0); // unique salt for address derivation

const op = await alice.buildDeploy(wasmBytes, salt, 100000);
```

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `code` | `number[]` | | WASM bytecode as byte array |
| `salt` | `number[]` | | 32-byte salt for address derivation |
| `maxFee` | `number` | `100000` | Maximum fee |
| **Returns** | `Promise<UserOperation>` | | Unsigned operation |

---

##### `simulate(op)`

Simulate an operation without modifying state.

```typescript
const sim = await alice.simulate(op);
console.log(`Would succeed: ${sim.success}, gas: ${sim.gas_used}`);
```

---

##### `submit(op)`

Submit a signed operation to the network.

```typescript
const result = await alice.submit(signedOp);
```

---

### `PasskeyAuth`

WebAuthn/passkey authentication support (browser environments).

#### Static Methods

##### `PasskeyAuth.isAvailable()`

Check if WebAuthn is supported in the current environment.

```typescript
if (PasskeyAuth.isAvailable()) {
  console.log("Passkeys supported");
}
```

---

### Utility Functions

##### `hexToBytes(hex)`

Convert a hex string to a byte array. Accepts optional `0x` prefix.

```typescript
import { hexToBytes } from "@solen/wallet-sdk";

hexToBytes("deadbeef");     // [222, 173, 190, 239]
hexToBytes("0xdeadbeef");   // [222, 173, 190, 239]
```

---

##### `bytesToHex(bytes)`

Convert a byte array or `Uint8Array` to a hex string.

```typescript
import { bytesToHex } from "@solen/wallet-sdk";

bytesToHex([222, 173, 190, 239]); // "deadbeef"
```

---

##### `nameToAccountId(name)`

Create a 32-byte account ID from a human-readable name (zero-padded).

```typescript
import { nameToAccountId } from "@solen/wallet-sdk";

nameToAccountId("alice");
// [97, 108, 105, 99, 101, 0, 0, ..., 0]  (32 bytes)
```

---

##### `nameToHex(name)`

Create a hex account ID from a name. Shorthand for `bytesToHex(nameToAccountId(name))`.

```typescript
import { nameToHex } from "@solen/wallet-sdk";

nameToHex("alice");
// "616c696365000000000000000000000000000000000000000000000000000000"
```

---

## Types

### `UserOperation`

```typescript
interface UserOperation {
  sender: number[];       // 32-byte sender account ID
  nonce: number;          // Sequence number
  actions: Action[];      // List of actions to execute
  max_fee: number;        // Maximum fee willing to pay
  signature: number[];    // Ed25519 signature bytes (64 bytes)
}
```

### `Action`

```typescript
type Action =
  | { Transfer: { to: string; amount: number } }
  | { Call: { target: string; method: string; args: number[] } }
  | { Deploy: { code: number[]; salt: number[] } };
```

---

## Full Examples

### Query chain state

```typescript
import { SolenClient, nameToHex } from "@solen/wallet-sdk";

const client = new SolenClient({ rpcUrl: "http://127.0.0.1:9944" });

// Chain status
const status = await client.chainStatus();
console.log(`Height: ${status.height}, Pending: ${status.pending_ops}`);

// Latest block
const block = await client.getLatestBlock();
console.log(`Block ${block.height}: ${block.tx_count} txs, ${block.gas_used} gas`);

// Account balance
const balance = await client.getBalance(nameToHex("faucet"));
console.log(`Faucet balance: ${balance}`);
```

### Build, simulate, and submit a transfer

```typescript
import { SolenClient, SmartAccount, nameToHex } from "@solen/wallet-sdk";

const client = new SolenClient({ rpcUrl: "http://127.0.0.1:9944" });
const alice = new SmartAccount(nameToHex("alice"), client);

// Build the transfer
const op = await alice.buildTransfer(nameToHex("bob"), 500);

// Simulate first to check it will succeed
const sim = await alice.simulate(op);
if (!sim.success) {
  console.error(`Simulation failed: ${sim.error}`);
  process.exit(1);
}
console.log(`Estimated gas: ${sim.gas_used}`);

// Sign the operation (Ed25519)
// op.signature = sign(privateKey, signingMessage(op));

// Submit
const result = await alice.submit(op);
console.log(`Accepted: ${result.accepted}`);
```

### Deploy a contract

```typescript
import { SolenClient, SmartAccount, nameToHex } from "@solen/wallet-sdk";
import { readFileSync } from "fs";

const client = new SolenClient({ rpcUrl: "http://127.0.0.1:9944" });
const deployer = new SmartAccount(nameToHex("alice"), client);

const wasm = Array.from(readFileSync("my_contract.wasm"));
const salt = new Array(32).fill(42);

const op = await deployer.buildDeploy(wasm, salt);
// sign op.signature ...
const result = await deployer.submit(op);
console.log(`Deploy accepted: ${result.accepted}`);
```

### Call a contract

```typescript
const op = await alice.buildCall(
  contractIdHex,
  "increment",
  [],      // no args
  100000   // max fee
);
// sign op.signature ...
const result = await alice.submit(op);
```

### Watch blocks

```typescript
const client = new SolenClient({ rpcUrl: "http://127.0.0.1:9944" });

let lastHeight = 0;
setInterval(async () => {
  const status = await client.chainStatus();
  if (status.height > lastHeight) {
    const block = await client.getBlock(status.height);
    console.log(
      `Block ${block.height}: ${block.tx_count} txs, ` +
      `${block.gas_used} gas, proposer ${block.proposer.slice(0, 8)}...`
    );
    lastHeight = status.height;
  }
}, 1000);
```

---

## License

MIT OR Apache-2.0
