# stSOLEN — Audit Preparation Pack

> v1, deployed mainnet 2026-05-01 at block 151583.
> Contract address: `bee37513c713e55113115dda2ae41d1ddd67802d99610708ec289130c1c8edc5`.

This document orients an auditor to the stSOLEN liquid-staking contract.
Read top-to-bottom; the system is small enough that a thorough review can be
self-contained. All file/line references are to this repository.

---

## 1. Executive summary

stSOLEN is a liquid-staking derivative for Solen. Users deposit native SOLEN;
the contract delegates that SOLEN to a curated allowlist of validators on the
staking system contract and mints `stSOLEN` (an SRC-20 receipt token) to the
depositor at the current exchange rate. Rewards auto-credit each epoch and
raise the exchange rate against `stSOLEN`. Withdrawals run through a
FIFO queue that respects the staking module's 7-epoch unbonding period.

**Key files (all paths relative to `~/solen`):**

| File | Purpose | LOC |
|---|---|---|
| `examples/contracts/stsolen/src/lib.rs` | Contract source (no_std, compiles to WASM) | ~1100 |
| `examples/contracts/stsolen/Cargo.toml` | Crate manifest | ~15 |
| `crates/solen-execution/tests/stsolen_lifecycle.rs` | End-to-end tests | ~700 |
| `crates/solen-system-contracts/src/staking.rs` | Solen staking module the contract delegates through | — |
| `crates/solen-execution/src/system_calls.rs` | System-call dispatcher | — |
| `crates/solen-execution/src/executor.rs` | Block executor (incl. patched dispatch_contract_call) | — |
| `tools/stsolen-deploy/src/main.rs` | One-shot deploy + init + operators | ~300 |
| `tools/stsolen-deposit/src/main.rs` | Deposit tool | ~250 |

---

## 2. Trust model & assets

### Assets

- **Pool (`total_pooled_solen`):** native SOLEN backing all stSOLEN. Lives
  partly in the staking system contract (delegated, earning rewards) and
  partly in the contract's own account balance (un-delegated reserve +
  matured undelegations awaiting payout + recently arrived rewards).
- **Receipt (`stSOLEN`):** SRC-20 token, redeemable for a proportional share
  of the pool at the current exchange rate. Total supply tracked in storage.

### Roles & their authorities

| Role | Storage key | Powers | Ideal custody |
|---|---|---|---|
| **Owner** | `owner` | Pause/unpause, set treasury, set slash oracle, set/remove operators, change fee bps (≤ 2000), change op cap bps. | Multisig |
| **Treasury** | `treasury` | Receives stSOLEN minted as protocol fee. No signing authority. | Cold or multisig |
| **Slash oracle** | `slash_oracle` | Only authorized caller of `report_slash`. Decrements `op_stake[op]` and `total_pooled_solen` by reported loss. | Hot key with off-chain monitor |
| **Cranker** | (none — permissionless) | Calls `crank_undelegations` / `claim_withdrawal` on schedule. Pays only gas. | Hot key, no contract permissions |
| **Operators** | `op/{i}` slots | Existing on-chain validator IDs; the contract delegates *to* them. They cannot interact with stSOLEN directly. | Out of scope — third-party validators |

### Trust assumptions on the underlying chain

1. **Solen consensus**: BFT, single-slot finality, no reorgs. The contract
   relies on this for `pending_withdrawal_solen` accounting (we decrement on
   user payout assuming the payout won't roll back).
2. **Staking module slashing affects `validator.self_stake` only, not
   delegations** (`crates/solen-system-contracts/src/staking.rs` → `slash`
   handler). This is load-bearing for the safety claim that delegators don't
   lose principal under current Solen rules. If this changes, `report_slash`
   becomes load-bearing too.
3. **`STAKING_ADDRESS:withdraw` drains all matured undelegations for the
   delegator atomically**, with no return value. The contract uses an
   internal `un_log` to compute the expected matured total, paired with the
   queued `withdraw` call.
4. **The Arc-1 queued-call model**: queued calls fire after the queueing
   contract returns; failures roll back the entire UserOp. No re-entrancy
   into the queueing contract within the same frame. **No return values from
   queued calls** — the contract cannot branch on success/failure of queued
   sub-calls.

---

## 3. Critical invariants

The contract is correct iff all of these hold at the end of every
state-mutating method:

- **I1 — Supply conservation:** `sum(bal/{addr}) == total_supply`.
  Mints/burns updated atomically with the corresponding `total_supply`.
  Asserted via integration tests; not asserted at runtime (would be O(n) over
  all balances, prohibitive).
- **I2 — Pool ≥ pending obligations:** `total_pooled_solen + un_delegated_in_account_balance ≥ pending_withdrawal_solen`. Holds outside of slash-induced shortfalls; documented `claim_shortfall` event covers the
  pathological case (see §6).
- **I3 — Operator allowlist hygiene:** `op_count ≤ MAX_OPERATORS (32)`,
  `op_cap_bps ≤ 10_000`, `protocol_fee_bps ≤ FEE_BPS_HARD_CAP (2000)`.
  All checked in admin setters (`do_admin_set_op_count`,
  `do_set_op_cap_bps`, `do_set_protocol_fee_bps`).
- **I4 — `op_stake[op]` non-zero only for allowlisted operators:** enforced
  by `remove_operator` rejecting if `op_stake[op] > 0`.
- **I5 — Withdrawal-queue FIFO:** `claim_withdrawal` requires
  `seq == wq_head`, no skipping.
- **I6 — Fee reserve:** the contract retains ≥ `MIN_FEE_RESERVE` (10_000 base
  units) after every deposit/recompound, so the staking system call's
  `caller.balance ≥ amount + MIN_FEE_RESERVE` precondition is always met.
  Enforced by `to_delegate = bal_now.saturating_sub(MIN_FEE_RESERVE).min(msg_value)`
  in `do_deposit`.

---

## 4. Storage layout

All keys live under the contract-storage prefix `cs/{contract_id}/`. The
contract operates only on the suffix; see `crates/solen-execution/src/state.rs::contract_storage_key`.

### SRC-20 (standard layout, copied from `examples/contracts/token`)

| Key | Type | Notes |
|---|---|---|
| `bal/{account}` | u128 LE | 36-byte key |
| `allow/{owner}/{spender}` | u128 LE | 71-byte key |
| `total_supply` | u128 LE | |

### Roles & flags

| Key | Type |
|---|---|
| `owner` | `[u8;32]` |
| `treasury` | `[u8;32]` |
| `slash_oracle` | `[u8;32]` |
| `paused` | `u8` (0 or 1) |

### Pool & rate accounting

| Key | Type | Notes |
|---|---|---|
| `total_pooled_solen` | u128 LE | SOLEN backing all stSOLEN |
| `last_balance_seen` | u128 LE | Snapshot at end of last `sync_rewards`; used to detect reward inflow |
| `pending_withdrawal_solen` | u128 LE | Σ `solen_owed` of unclaimed wq entries |
| `withdrawal_buffer` | u128 LE | SOLEN sitting in account balance from past matured `STAKING_ADDRESS:withdraw` calls, awaiting claim payout |
| `protocol_fee_bps` | u64 LE | Default 1000 (10%) |
| `last_recompound_epoch` | u64 LE | Rate-limit for `recompound_rewards` |

### Operator allowlist

| Key | Type | Notes |
|---|---|---|
| `op/{i}` | `[u8;32]` | Validator ID at allowlist slot `i`; `i` is u64 LE in the suffix |
| `op_count` | u64 LE | Number of populated slots |
| `op_cursor` | u64 LE | Round-robin pointer |
| `op_cap_bps` | u64 LE | Per-operator stake cap, default 2500 (25%) |
| `op_stake/{op}` | u128 LE | Tracked stake we believe is delegated to `op` |

### Withdrawal queue

| Key | Type | Notes |
|---|---|---|
| `wq/{seq}` | 56 bytes | `account[32] ‖ solen_owed[16] ‖ requested_epoch[8]` |
| `wq_head` | u64 LE | Next seq to serve |
| `wq_tail` | u64 LE | Next seq to assign |
| `pue/{op}` | u128 LE | Pending undelegate amount for `op`, awaiting next crank |
| `ifu/{op}` | u64 LE | In-flight undelegations (≤ 7 cap from the staking module) |

### Undelegation log (matured-tracking)

| Key | Type | Notes |
|---|---|---|
| `un/{seq}` | 56 bytes | `epoch[8] ‖ amount[16] ‖ operator[32]` |
| `un_log_head` | u64 LE | Next un_log seq to consume |
| `un_log_tail` | u64 LE | Next un_log seq to assign |

---

## 5. Method reference

For each public method: pre-conditions, effects on state, queued sub-calls,
errors. Internal helpers are documented inline in the source.

### SRC-20 (standard, mirrors token example)

- **`init(treasury[32] ‖ slash_oracle[32])`** — one-shot. Refuses if `owner`
  already set. Emits `initialized(caller)`.
- **`transfer(to[32] ‖ amount[16])`** — caller-initiated. Allowed even when
  `paused`. Emits `transfer(to ‖ amount)`.
- **`transfer_from(from[32] ‖ to[32] ‖ amount[16])`** — allowance-based.
- **`approve(spender[32] ‖ amount[16])`**.
- **`balance_of`, `allowance`, `total_supply`, `name`, `symbol`, `decimals`** — view.

### Lifecycle

- **`deposit()`** — `msg_value` is the SOLEN-in.
  - Pre: `paused == 0`, `msg_value > 0`, `op_count > 0`. First-ever deposit
    requires `msg_value ≥ MIN_FIRST_DEPOSIT (11_100)`.
  - Effects: `sync_rewards()`. Computes `to_delegate =
    min(msg_value, self_balance - MIN_FEE_RESERVE)`. On first deposit, mints
    1000 stSOLEN to `DEAD_ADDRESS` and `(msg_value - 1000)` stSOLEN to
    caller; otherwise mints `msg_value * total_supply / total_pooled_solen`.
    `total_pooled_solen += msg_value`. `op_stake[chosen] += to_delegate`.
    Updates `last_balance_seen -= to_delegate`.
  - Queues: `STAKING_ADDRESS:delegate(chosen ‖ to_delegate)`.
  - Errors: `paused`, `zero_value`, `deposit_too_small_for_reserve`,
    `first_deposit_too_small`, `mint_zero`, `no_operators`,
    `invariant_pool_zero`, `queue_full`.
  - Events: `deposit(caller ‖ solen_in ‖ stsolen_out ‖ operator)` (96 B),
    `mint(to ‖ amount)` for caller, plus `mint(DEAD ‖ 1000)` on first deposit.

- **`request_withdrawal(stsolen_burn[16])`**
  - Pre: `paused == 0`, `bal[caller] ≥ stsolen_burn`, pool non-empty.
  - Effects: `sync_rewards()`. Locks `solen_owed = stsolen_burn * total_pooled / total_supply`.
    Burns stSOLEN. `total_pooled -= solen_owed`,
    `pending_withdrawal_solen += solen_owed`. Distributes solen_owed pro-rata
    across operators by `op_stake[i]/Σ op_stake` into `pue/{op}`. Residual
    rounding to last non-empty operator. Appends to `wq[wq_tail++]`.
  - Queues: nothing — undelegates are batched by `crank_undelegations`.
  - Errors: `paused`, `invalid_args`, `zero_amount`, `insufficient_balance`,
    `empty_pool`, `owed_zero`, `no_delegated_stake`.
  - Events: `withdrawal_requested(caller ‖ stsolen_burned ‖ seq ‖ eligible_epoch)`.

- **`crank_undelegations()`** — permissionless.
  - Effects: `sync_rewards()`. Walks `un_log` from `un_log_head`, sums matured
    entries (`epoch + UNBONDING_EPOCHS ≤ now`); commits drain (advances head,
    decrements `ifu[op]`), updates `withdrawal_buffer +=
    matured`, pre-emptively bumps `last_balance_seen += matured`. Then for
    each op with `pue/{op} > 0` and `ifu/{op} < MAX_UNDELEGATIONS_PER_OP - 1`:
    `op_stake -= amount`, appends `un_log[un_log_tail++]`, increments `ifu`.
  - Queues: `STAKING_ADDRESS:withdraw()` (if matured), then per-op
    `STAKING_ADDRESS:undelegate(op ‖ amount)`.
  - Errors: `queue_full`.
  - Events: `crank(operators_processed ‖ total_undelegated)`.

- **`claim_withdrawal(seq[8])`** — permissionless.
  - Pre: `seq == wq_head`, entry exists, `now ≥ requested_epoch + UNBONDING_EPOCHS + 1`,
    `withdrawal_buffer ≥ solen_owed`.
  - Effects: `sync_rewards()`. `withdrawal_buffer -= solen_owed`,
    `pending_withdrawal_solen -= solen_owed`. Tombstones `wq[seq]`,
    advances `wq_head`. Pre-emptively `last_balance_seen -= solen_owed`.
  - Queues: native `transfer(account, solen_owed)` — settles post-return.
  - Errors: `invalid_args`, `not_head_of_queue`, `no_such_request`,
    `not_yet_eligible`, `buffer_insufficient`, `transfer_queue_full`.
  - Events: `withdrawal_claimed(account ‖ solen_owed ‖ seq)`.

- **`recompound_rewards()`** — permissionless. Rate-limited: refuses if
  `now ≤ last_recompound_epoch && total_pooled > 0`.
  - Effects: `sync_rewards()`. Computes `available = self_balance -
    pending_withdrawal_solen - MIN_FEE_RESERVE`. If `available ≥ 100 SOLEN`
    (10⁸·100 base units), picks operator, `op_stake[op] += available`,
    queues `STAKING_ADDRESS:delegate(op ‖ available)`,
    `last_balance_seen -= available`, `last_recompound_epoch = now`.
  - Errors: `rate_limited`, `insufficient_to_recompound`, `no_operators`,
    `queue_full`.
  - Events: `recompounded(amount ‖ operator)`.

- **`poke()`** — permissionless `sync_rewards()` only. No state mutation
  beyond that. Useful for keeping `total_pooled_solen` fresh during quiet
  periods.

### Slash oracle

- **`report_slash(operator[32] ‖ realized[16])`** — gated to
  `caller == slash_oracle`.
  - Pre: `realized < op_stake[operator]` (rejects non-loss reports).
  - Effects: `loss = prior - realized`. `op_stake[operator] = realized`,
    `total_pooled_solen -= loss` (saturating).
  - Errors: `unauthorized`, `invalid_args`, `not_a_loss`.
  - Events: `slash_reported(operator ‖ prior ‖ realized ‖ loss)`.

### Admin (owner-gated)

- **`set_operator(index[8] ‖ operator[32])`** — `index < MAX_OPERATORS`.
- **`remove_operator(index[8])`** — refuses if `op_stake[op_at_slot] > 0`.
- **`set_op_count(count[8])`** — `count ≤ MAX_OPERATORS`.
- **`set_op_cap_bps(bps[8])`** — `bps ≤ 10_000`.
- **`set_protocol_fee_bps(bps[8])`** — `bps ≤ 2000`.
- **`set_treasury(addr[32])`**, **`set_slash_oracle(addr[32])`**.
- **`pause()` / `unpause()`** — toggles `paused`. Emits `paused` / `unpaused`.

### Reads (no mutation)

- `exchange_rate()` → `(total_pooled[16] ‖ total_supply[16])` — caller does
  the divide at full precision.
- `pending_undelegate_op_of(operator[32])` → u128.
- `op_stake_of(operator[32])` → u128.
- `withdrawal_at(seq[8])` → `account[32] ‖ solen_owed[16] ‖ epoch[8]` or `b""`.
- `pending_withdrawals_of(account[32])` → u64 count.
- `owner`, `treasury`, `slash_oracle`, `paused`.

---

## 6. Math derivations

### 6.1 Exchange rate

```
mint  = solen_in * total_supply / total_pooled_solen      [normal case]
owed  = stsolen_burn * total_pooled_solen / total_supply  [normal case]
mint  = solen_in - 1000                                   [first deposit; bootstrap burn]
```

All u128. With Solen's 2 × 10¹⁷ max base units and `total_supply ≤ 2 × 10¹⁷`,
products are bounded by 4 × 10³⁴ — well under `u128::MAX ≈ 3.4 × 10³⁸`.
Mul-then-div ordering is intentional: prevents truncating the numerator
before division.

**Truncation bias:** depositors lose ≤ 1 base unit on mint; claimants lose
≤ 1 base unit on owed. Symmetric direction (truncates toward zero on both
sides), so the protocol doesn't systematically extract value from one side.

### 6.2 Bootstrap burn

First deposit (`total_supply == 0`) mints 1000 stSOLEN to `DEAD_ADDRESS` and
`(msg_value - 1000)` to caller. Total supply ends at `msg_value`, pool ends
at `msg_value`, exchange rate = 1.0. The dead-address position is permanent
and unredeemable.

**Why:** prevents the donate-and-deposit attack (Uniswap V2 `MINIMUM_LIQUIDITY`
mitigation). An attacker can't deflate `total_supply` to 1 to inflate the rate
because 1000 is permanently locked.

**Constraint:** `MIN_FIRST_DEPOSIT = 11_100` ensures `to_delegate > 0` after
reserving `MIN_FEE_RESERVE (10_000)` and burning 1000 — leaves a positive
mint for the depositor.

### 6.3 Reward absorption (`sync_rewards`)

```
inflow = current_balance - last_balance_seen - msg_value
fee    = inflow * protocol_fee_bps / 10_000
growth = inflow - fee
total_pooled_solen += growth
fee_mint = fee * total_supply / (total_pooled_solen_after_growth)
mint(treasury, fee_mint)
last_balance_seen = current_balance
```

Reward inflow is detected as the unaccounted balance delta excluding the
current op's `msg_value`. The fee is minted in stSOLEN to the treasury at
the **post-growth rate** — this biases a tiny (< 0.1 % at typical fee/reward
sizes) loss onto the treasury rather than diluting holders. Acceptable
asymmetry; deliberate per spec.

**Limitation:** Matured-undelegation inflows from the staking module also
arrive in account balance. `sync_rewards` would mis-classify them as
rewards if not accounted for. The contract handles this by:

1. `crank_undelegations` queues `STAKING_ADDRESS:withdraw` and
   pre-emptively bumps `last_balance_seen += matured`. So the *next*
   `sync_rewards` sees the post-pull balance equal to its expected
   `last_balance_seen`, no phantom inflow.
2. `claim_withdrawal` pre-emptively bumps `last_balance_seen -= solen_owed`
   for the same reason.

**Audit focus:** verify that every state path that adds or removes balance
post-WASM-return correspondingly adjusts `last_balance_seen`. See §11.

### 6.4 Withdrawal-queue allocation

`request_withdrawal` distributes the `solen_owed` pro-rata across operators:

```
total_op_stake = Σ op_stake[i]
per_op_share[i] = solen_owed * op_stake[i] / total_op_stake
residual = solen_owed - Σ per_op_share[i]
allocated to last non-empty operator: pue[last] += residual
```

**Audit concern:** what if `total_op_stake < solen_owed`? The contract
errors with `no_delegated_stake` if `total_op_stake == 0`, but doesn't check
the inequality otherwise. In practice this can happen if a slash dropped
operator stakes below the user's owed amount. The crank then fails to drain
the full owed (caps `amount = pending.min(stake)`), leaving residual `pue/`
that won't undelegate. The user's `solen_owed` is locked in `pending_withdrawal_solen`
but the matched undelegations don't fully cover it. **This is a known sharp
edge — see §7.**

### 6.5 Slashing accounting

Current behavior under Solen's slash model: slashes hit `validator.self_stake`
only, not delegations. Contract delegations are unaffected at the chain
level. `report_slash` is wired but should NOT be called against the current
slash semantics (would incorrectly decrement `total_pooled_solen` against
principal that's still safe).

If Solen later adopts delegation-affecting slashes, `report_slash` becomes
load-bearing:

```
loss = prior_op_stake - realized_op_stake
op_stake[op] = realized
total_pooled_solen -= loss   (saturating)
```

The slash-oracle bot watches `slashed` events from `STAKING_ADDRESS` and
can report when configured to do so (currently `ON_SLASH=alert` by default).

---

## 7. Known limitations & deferred items (out of scope for this audit)

### 7.1 Slash-oracle correctness under current Solen semantics
Per §6.5, `report_slash` decrements pool, but current Solen slashes don't
affect delegations. The bot ships with `ON_SLASH=alert` for that reason.
**Auditor: verify the gate and the math, but the design choice that
`report_slash` should not be enabled today is documented.**

### 7.2 Withdrawal queue under-allocation after slash
If a slash reduces `total_op_stake` below pending obligations, the crank
caps undelegate amounts at available stake, leaving residual `pue/`
amounts that won't drain. The user's withdrawal queue entry is at locked
rate; the buffer eventually short. v1 surfaces this via
`err:buffer_insufficient` on claim. **No automated recovery** —
operational team must intervene.

### 7.3 Operator migration
No `migrate_operator` admin path. Removing an operator with non-zero stake
requires waiting for natural drain via withdrawals. Deferred to v1.1.

### 7.4 No pause-state-aware claims
`claim_withdrawal` does NOT check `paused` — claims continue even when
deposits are halted. **Intentional**: pause is for halting *new* exposure,
not freezing user funds.

### 7.5 Matured-pull racing
If two cranks fire simultaneously, both walk `un_log` and both queue a
`STAKING_ADDRESS:withdraw`. The second walk reads the (committed) post-first-walk
state, so it sees no matured entries and does nothing. **Safe by sequencing**:
the executor processes UserOps serially per-block.

### 7.6 Treasury fee bias
Treasury receives fewer stSOLEN than it would at pre-growth rate
(documented in §6.3). Magnitude < 0.1 % at default 10 % fee. Acceptable.

### 7.7 `withdraw` system-call failure
If `STAKING_ADDRESS:withdraw` were to fail (e.g. corrupt staking state),
the queued call returns Err and the entire UserOp rolls back. Crank state
unwinds; `un_log_head` un-advances; `withdrawal_buffer` un-bumps;
`last_balance_seen` un-bumps. **Verify atomicity** — see §11 invariant
checks.

---

## 8. Solen VM dependencies

### 8.1 Arc-1 queued-call model
Cross-contract calls via `sdk::queue_call` execute *after* the queueing
contract's `call()` returns. **Failures propagate; whole UserOp rolls
back.** No re-entrancy. **No return values** — the contract cannot
introspect sub-call success.

This is documented at `crates/solen-vm/src/host.rs::HostContext.pending_calls`
and `crates/solen-execution/src/executor.rs:1162` (drain loop).

### 8.2 Executor patch (system-contract routing)
Pre-deploy patch lands at `crates/solen-execution/src/executor.rs::dispatch_contract_call`:
queued calls to system addresses are now routed through `execute_system_call`
with `sender = queueing_contract`. Without this, queued calls to
`STAKING_ADDRESS:delegate` etc. silently no-op. This is the load-bearing
plumbing — without it, stSOLEN couldn't drive the staking module from
queued calls.

**Auditor: verify the patch correctness in `executor.rs:1056-1082` (the
new `is_system_contract` branch in `dispatch_contract_call`).**

### 8.3 `sdk::self_balance()` host fn
Contract uses this to detect rewards. Implementation:
`crates/solen-vm/src/runtime.rs` (`get_self_balance` linker entry) +
`crates/solen-contract-sdk/src/lib.rs::sdk::self_balance`. Snapshotted at
frame start; doesn't reflect outflows queued during this frame.

### 8.4 `native_transfers` ordering
`sdk::transfer` (used in `claim_withdrawal`) is queued separately from
`sdk::queue_call`. The executor processes `native_transfers` *before*
`pending_calls` (`executor.rs:1108-1175`). This is why
`claim_withdrawal` does NOT also queue `STAKING_ADDRESS:withdraw` — the
transfer would fire before the matured pull, hitting "insufficient balance".
The contract requires `crank_undelegations` to have pre-filled the buffer.

### 8.5 Operation-level rollback
If any action in a multi-action UserOp fails, `executor.rs::execute_operation`
restores state via the snapshot taken pre-execution. **Caveat:** the
`max_fee` reservation taken at `execute_operation` line ~531 is NOT
refunded on rollback (only the success path refunds at line ~668). This is
existing executor behavior, documented in stsolen-lifecycle tests
(`queued_system_call_failure_rolls_back_op` asserts the bound).

---

## 9. Test inventory

All in `crates/solen-execution/tests/stsolen_lifecycle.rs`. Run:

```bash
cd ~/solen
cargo test -p solen-execution --test stsolen_lifecycle
```

**10 integration tests, all passing on commit at deploy time:**

| # | Test | Coverage |
|---|---|---|
| 1 | `first_deposit_mints_minus_bootstrap_burn_and_delegates` | Bootstrap path + delegate flow |
| 2 | `reward_inflow_grows_pool_and_skims_treasury_fee` | sync_rewards math, fee mint at post-growth rate |
| 3 | `recompound_redelegates_idle_rewards` | recompound flow + rate-limit |
| 4 | `request_crank_claim_full_withdrawal_cycle` | End-to-end withdrawal: request → crank → epoch advance → claim |
| 5 | `claim_before_eligibility_returns_error` | UNBONDING_EPOCHS gate |
| 6 | `admin_methods_reject_non_owner` | Admin auth surface (pause, set_treasury, set_protocol_fee_bps) |
| 7 | `pause_halts_deposits_but_not_transfers` | Pause semantics; SRC-20 transfers still allowed |
| 8 | `report_slash_oracle_auth_and_math` | Slash oracle gating + non-loss rejection + pool decrement |
| 9 | `exchange_rate_rises_after_reward` | Post-reward second deposit gets fewer stSOLEN |
| 10 | `claim_without_crank_fails_buffer_insufficient` | Two-step claim flow enforced |

**Adjacent suites (also touched):** `crates/solen-execution/src/executor.rs` lib tests (24, including `queued_call_routes_to_staking_system_contract` and `queued_system_call_failure_rolls_back_op` for the executor patch); `crates/solen-vm/src/lib.rs` lib tests (5).

---

## 10. Suggested focus areas for the auditor

In rough priority order:

1. **`last_balance_seen` accounting (§6.3, §11).** Every state path that
   moves SOLEN in/out of the contract account post-WASM-return must
   correspondingly adjust `last_balance_seen`. Any miss creates a
   phantom-reward or phantom-loss.

2. **Withdrawal-queue allocation under partial slash (§6.4, §7.2).** Trace
   what happens when `total_op_stake < pending_withdrawal_solen`. Is the
   `claim_shortfall` path actually reachable? Is it correctly emitted?

3. **`crank_undelegations` matured-pull / new-undelegate ordering.** Verify
   the queue order: `withdraw` first, then per-op `undelegate`s. Confirm
   this is correct with respect to the staking module's
   `MAX_UNDELEGATION_ENTRIES = 7` cap.

4. **Reentrancy via `transfer_from` callbacks?** SRC-20 `transfer` /
   `transfer_from` only update storage and emit events — no callbacks. But
   Solen's smart-account model allows `auth_methods` to call back into
   contracts. Verify that an SRC-20 transfer to a contract with a custom
   `auth_method` cannot trigger re-entrant deposit/withdrawal logic.
   *(Likely safe — SRC-20 transfer is purely internal storage; no cross-call.
   But worth confirming.)*

5. **Frontrunning the bootstrap burn.** If the deployer / bootstrap
   depositor's first-deposit tx is in the mempool, can a third party
   front-run with a smaller amount that satisfies `MIN_FIRST_DEPOSIT` and
   captures most of the pool? *(Unlikely to cause material loss but worth
   the trace.)*

6. **Operator removal race.** `remove_operator` refuses if `op_stake[op] > 0`,
   but doesn't atomically prevent a concurrent `request_withdrawal` from
   adding to `pue/{op}` and then crank decrementing `op_stake` to zero
   *just before* removal — leaving stranded `pue/` after removal. Trace
   carefully.

7. **Integer overflow corners.** All arithmetic is `saturating_*` or
   bounded by §6.1 analysis, but verify no path uses raw `+`/`*` on
   user-controlled input.

8. **The executor patch (§8.2).** Independent review of
   `dispatch_contract_call`'s new system-contract route. Does the patch
   preserve the depth cap? Does it respect the failure-rollback
   semantics? Is `caller` propagated correctly?

9. **`get_self_balance` host fn (§8.3).** Does it correctly reflect the
   contract's account balance at frame start, including msg_value?

10. **Method-level dispatcher.** `do_*` matches in `lib.rs` — verify every
    declared method has a real body (no stubbed `err:todo_*` slipped
    through).

---

## 11. Reproducibility

```bash
# Build the contract WASM (release):
cd ~/solen/examples/contracts/stsolen
cargo build --release --target wasm32-unknown-unknown
# → target/wasm32-unknown-unknown/release/solen_stsolen.wasm  (~29 KB)

# Run the integration suite:
cd ~/solen
cargo test -p solen-execution --test stsolen_lifecycle

# Build the deploy + deposit tools:
cargo build --release -p stsolen-deploy
cargo build --release -p stsolen-deposit

# Mainnet deployment artifact:
# Block:   151583
# Tx:      c026296cae7c69db25ef3f021834bddd24a197f788d1cf6bcc528bf4a23ac7e3
# Address: bee37513c713e55113115dda2ae41d1ddd67802d99610708ec289130c1c8edc5
```

The deployed bytecode is reproducible from the source at the deployment
commit. To verify: build the WASM at that commit, hash it, and compare to
the on-chain `code_hash` for the contract account.

---

## 12. Contact

For audit follow-up: dev@solenchain.io.

Test vectors and reproducible build artifacts are in this repo. Fixes
discovered during audit should be reproduced as integration tests in
`stsolen_lifecycle.rs` before merge.
