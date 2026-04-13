# Spec 003: RPC API

**Status:** Draft

## Transport

The RPC server accepts both HTTP POST and WebSocket connections on the same port. All methods below are callable over either transport. WebSocket additionally supports real-time subscriptions.

- **HTTP:** `POST http://host:port` with JSON-RPC 2.0 body
- **WebSocket:** `ws://host:port` with JSON-RPC 2.0 messages

## Endpoint Groups

### State Queries
- `solen_chainStatus()` - Chain height, state root, pending ops, supply stats
- `solen_getBalance(account_id)` - Account balance
- `solen_getAccount(account_id)` - Full account state (balance, nonce, code_hash)
- `solen_getBlock(height)` - Block by height
- `solen_getLatestBlock()` - Latest finalized block
- `solen_getValidators()` - Active validator set with stakes
- `solen_getStakingInfo(account_id)` - Delegations and pending undelegations
- `solen_getVestingInfo(account_id)` - Vesting schedule and claimable amount
- `solen_getGovernanceProposals()` - All governance proposals

### Write
- `solen_submitOperation(signed_op)` - Submit a user operation
- `solen_submitIntent(intent)` - Submit an intent for solver resolution

### Simulation
- `solen_simulateOperation(op)` - Dry-run with gas estimate
- `solen_checkSponsorship(op)` - Check if a paymaster will sponsor
- `solen_callView(contract_id, method, args?)` - Read-only contract call

### Intents
- `solen_getPendingIntents(limit?)` - Pending intents available for solvers
- `solen_submitSolution(solution)` - Submit a solver's solution for an intent

### Rollup
- `solen_getRollupStatus(rollup_id)` - Rollup registration, last state root, batch count
- `solen_getRollupBatches(rollup_id, limit?)` - Verified batch history
- `solen_submitBatch(batch)` - Submit a rollup batch commitment for verification

### WebSocket Subscriptions

Subscriptions use JSON-RPC 2.0 subscription protocol. Connect via WebSocket, send a subscribe request, and receive notifications until unsubscribed or disconnected.

- `solen_subscribeNewBlocks()` → `solen_newBlock` notifications
  - Streams every finalized block: height, epoch, block_hash, state_root, proposer, timestamp_ms, tx_count, gas_used

- `solen_subscribeTxConfirmation(sender, nonce)` → `solen_txConfirmation` notifications
  - Watches for a specific transaction (identified by sender + nonce)
  - Auto-closes after the transaction is confirmed
  - Returns: block_height, tx_hash, sender, nonce, success, gas_used

- `solen_subscribeValidatorChanges()` → `solen_validatorChange` notifications
  - Emitted at epoch boundaries when the validator set changes
  - Returns: epoch, active_count
