# solen-system-contracts

Privileged system contracts for the Solen settlement layer.

## Contracts

### Staking (`staking.rs`)

Validator registration, delegation, undelegation with 7-epoch unbonding, and stake-weighted reward distribution. Minimum validator stake: 1,000 tokens.

### Bridge (`bridge.rs`)

Canonical bridge with per-rollup vaults. Deposits are instant. Withdrawals go through a 100-block challenge window plus 50-block delay. Supports dispute mechanism to block fraudulent withdrawals.

### Governance (`governance.rs`)

Proposal creation, stake-weighted voting, and timelocked execution. Quorum: 30% participation. Pass threshold: 66.67% supermajority. Timelock: 3 epochs. Supports parameter changes, rollup registration, and emergency pause.

### Treasury (`treasury.rs`)

Collects fees (with configurable burn rate), tracks balances, and disburses grants approved through governance.

### Proof Registry (`proof_registry.rs`)

Manages approved proof systems (validity, fraud). Tracks which rollups use which proof types.
