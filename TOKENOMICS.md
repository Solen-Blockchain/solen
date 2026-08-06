# Solen Tokenomics (canonical)

> **This document is the single source of truth for SOLEN token allocation.** It
> is reconciled against the deployed mainnet genesis (`genesis-mainnet.json` +
> the mainnet branch of `crates/solen-node/src/genesis_config.rs`) and the
> on-chain state. Where earlier materials (older tokenomics tables, investor
> decks) disagree with this document, **this document is correct**; those are
> superseded. Everything below is independently verifiable on-chain (see
> "Verify on-chain").

SOLEN is the native token of the Solen network — used for staking, fees,
governance, and settlement.

## Supply

| Parameter | Value |
|-----------|-------|
| Symbol | SOLEN |
| Decimals | 8 (1 SOLEN = 100,000,000 base units) |
| Total supply (genesis) | **2,000,000,000 SOLEN** (`__total_supply__` = 2×10¹⁷ base units) |
| Genesis emission | Fixed at genesis; validator rewards are paid **from the pre-funded staking pool**, not minted (no new supply until the pool is exhausted) |
| Deflation | 50% of every transaction fee is burned |

Note on "max supply": the 2B is the genesis supply. It is **not** a hard cap
enforced in code as immutable — governance could later enable inflation once the
staking pool depletes (see Staking Rewards). Until then, supply only *decreases*
(fee burn).

## Genesis allocation (mainnet, chain_id = 1)

Every bucket below is a real genesis balance at a specific address. `addr(n)`
denotes the system address `0xff…ff<n>` (32 bytes, all `0xff` except the last
byte). System addresses are the same on every node.

| Bucket | SOLEN | % | On-chain location | Status at genesis |
|--------|------:|--:|-------------------|-------------------|
| **Foundation Treasury** | 589,000,000 | 29.45% | `TREASURY_ADDRESS` = `addr(0x04)` | Liquid, governance-controlled. Bundles 400M foundation + **89M validator reserve** + **100M investor reserve** (see reconciliation). |
| **Staking rewards pool** | 500,000,000 | 25.00% | `STAKING_POOL_ADDRESS` = `addr(0x10)` | Locked to reward emission; pays validators/delegators over time. |
| **Team & founders** | 300,000,000 | 15.00% | `TEAM_POOL_ADDRESS` = `addr(0x23)` | Locked; relocated into the vesting vault (`addr(0x06)`) via the `MigrateTeamPoolToVesting` governance proposal **before** the team cliff. Claims are paid from the vault (never minted). |
| **Ecosystem fund** | 300,000,000 | 15.00% | `738a9064…dd13ff57` | Grants, integrations, incentives. |
| **Community** | 200,000,000 | 10.00% | `ad4d2e42…db117d25` | Distribution / airdrops. |
| **Liquidity & market-making** | 100,000,000 | 5.00% | `f01e9941…feb89123` | Available at launch. |
| **Genesis validator self-stake** | 11,000,000 | 0.55% | 11 validators × 1,000,000, **staked** | 1M self-stake each; subject to the genesis validator lock. |
| **Total** | **2,000,000,000** | **100%** | | |

`total_supply` is computed at genesis as `sum(account balances) + total_staked`
and stored in `__total_supply__`; the table above sums to exactly 2×10¹⁷ base
units.

### Reconciliation vs. prior tables (what changed and why)

Earlier tokenomics tables and decks listed eight round buckets summing to 2B.
The deployed genesis differs in three material ways — corrected here:

1. **Genesis validators self-staked 11M, not 100M.** There are 11 genesis
   validators at 1M self-stake each. The remaining ~89M of the old "genesis
   validators" bucket is held as a **validator reserve inside the Treasury**
   (part of the 589M), not distributed to validators.
2. **There is no separate "Early Investors" genesis account on mainnet.** The
   `INVESTOR_POOL` account is only created on non-mainnet. The 100M investor
   reserve is held **inside the Treasury** (part of the 589M) and would be
   allocated via future investor vesting schedules funded from Treasury.
3. **Treasury is 589M at genesis** (400M foundation + 89M validator reserve +
   100M investor reserve), not 400M.

Buckets that match prior docs exactly: Staking 500M, Team 300M, Ecosystem 300M,
Community 200M, Liquidity 100M.

## Circulating supply

Circulating supply is **computed on-chain**, not a fixed "launch float." The
`chainStatus` RPC defines it as:

```
circulating = total_supply
            − Σ(balances of system/fund addresses: treasury, staking pool,
                ecosystem, community, liquidity, team pool, vesting, staking,
                governance, bridge, intent, investor pool)
            − total_staked
```

At genesis, essentially all supply sits in fund accounts or is staked, so
circulating starts near zero and grows only as tokens are *distributed out* of
fund accounts (community distribution, liquidity deployment, ecosystem grants,
vested claims) or *unstaked*. Any "circulating supply" figure must be sourced
from `solen_chainStatus` (`total_circulation`) — do not quote a planned float as
if it were circulating.

## Vesting

- **Team (300M):** held in `TEAM_POOL_ADDRESS` at genesis with the team-member
  accounts at balance 0. The pool is migrated into the vesting vault
  (`VESTING_ADDRESS`) via governance before the cliff; claims are paid from the
  vault. Intended schedule: 1-year cliff, then linear over the remaining term.
- **Investors:** `investor_vesting` is **empty** at genesis. Any investor
  allocation would be added post-genesis via `add_vesting`, funded from Treasury
  at add-time (not pre-minted). No investor tokens exist on-chain today beyond
  the Treasury reserve.
- The genesis allocation is immutable chain identity (pinned by `--genesis-hash`
  / chain_id = 1) and is never edited retroactively; all post-genesis schedules
  are Treasury-funded.

## Staking rewards

Rewards are paid from the pre-funded 500M pool (`STAKING_POOL_ADDRESS`) — they
are **not** newly minted, so they do not increase supply until the pool is
exhausted.

| Parameter | Value (mainnet) |
|-----------|-----------------|
| Reward pool | 500,000,000 SOLEN |
| Block time | **6 s** (`block_time_ms = 6000`) |
| Epoch length | 100 blocks ≈ **10 minutes** |
| Reward per epoch (on-chain default) | 317 SOLEN, **governance-adjustable** (`__config_epoch_reward__`) |
| Implied emission at default rate | ~52,560 epochs/yr × 317 ≈ **16.7M SOLEN/yr** → pool lasts ~30 years at the current default |
| Payout | Each epoch, split across active validators by total stake (self + delegated) |

> Reward rate, block time, and epoch reward are all governance parameters, so
> the emission curve is a policy choice, not a fixed schedule. The pool balance
> and the current `__config_epoch_reward__` are the ground truth.

Reward split within a validator:
```
validator_reward = epoch_reward × (validator_total_stake / network_total_stake)
delegator_reward = validator_reward × (delegator_stake / validator_total_stake)
```

Staking parameters (from `solen-system-contracts::staking`): minimum self-stake
500,000 SOLEN; minimum active validators 20; unbonding period 7 epochs; slashing
10% (double-sign) / 1% (downtime after threshold); genesis validator lock
`GENESIS_LOCK_EPOCHS = 157,680` epochs (note: at 6 s blocks this is ~3 years of
wall-clock, longer than the "~1 year" figure in older docs, which assumed 2 s
blocks — flagged for governance review).

## Fees

| Parameter | Value |
|-----------|-------|
| Base fee per gas | 1 base unit (governance-adjustable) |
| Burn | **50% of fees burned** (permanent supply reduction) |
| Treasury | 50% of fees to Treasury |
| Gas | transfer 100 · contract call 500 + VM cost · deploy 1,000 |

Fees can be sponsored (paymasters / gas abstraction), so users need not hold
SOLEN to transact. As usage grows, fee burn can offset or exceed reward
emission, making the token net-deflationary.

## Governance

Staked-token voting. Key parameters (authoritative values live in
`solen-system-contracts::governance` and on-chain config):

| Parameter | Value |
|-----------|-------|
| Quorum | 30% of staked supply |
| Pass threshold | 66.67% supermajority |
| Voting period | Governance-set; the raw genesis value is **clamped** by `effective_voting_period()` to a safe bound so governance can never be frozen |
| Timelock | Applied before execution |

Governance can modify fee/burn rates, block time, epoch rewards, staking
parameters, rollup registration, and emergency pause/resume. A dormant
`EMERGENCY_FASTTRACK` gate (default off, `u64::MAX`) can — only after a
coordinated flag-day activation — let quorum + supermajority pass emergency
pause/resume without the normal voting window/timelock; its activation policy is
documented separately and it is **off** today.

## Verify on-chain

- **Total & circulating supply:** `solen_chainStatus` → `total_allocation`,
  `total_staked`, `total_circulation`.
- **Fund balances:** `solen_getAccount` on each address above (e.g. Treasury
  `addr(0x04)`, staking pool `addr(0x10)`, team pool `addr(0x23)`).
- **Validators & stake:** `solen_getValidators`.
- **Genesis:** `genesis-mainnet.json` (pinned by `--genesis-hash`).

_Last reconciled: 2026-08 against genesis-mainnet.json and mainnet on-chain state._
