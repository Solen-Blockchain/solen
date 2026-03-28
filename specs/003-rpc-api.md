# Spec 003: RPC API

**Status:** Draft

## Endpoint Groups

### State Queries
- `solen_getBalance(account_id)` - Account balance
- `solen_getAccount(account_id)` - Full account state
- `solen_getBlock(height | "latest")` - Block by height
- `solen_getValidatorSet(epoch?)` - Validator set

### Write
- `solen_submitOperation(signed_op)` - Submit a user operation
- `solen_submitIntent(signed_intent)` - Submit an intent

### Simulation
- `solen_simulateOperation(op)` - Dry-run with gas estimate and action summary
- `solen_checkSponsorship(op)` - Check if a paymaster will sponsor

### Rollup
- `solen_getRollupStatus(rollup_id)` - Rollup registration and latest commitment
- `solen_submitBatch(batch)` - Submit a rollup batch commitment
