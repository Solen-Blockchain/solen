# Solen Tokenomics

The native token of the Solen network. Used for staking, fees, governance, and settlement.

## Supply

| Parameter | Value |
|-----------|-------|
| **Total initial supply** | 2,000,000,000 (2B) |
| **Token symbol** | SOLEN |
| **Decimals** | 8 (1 SOLEN = 100,000,000 base units) |
| **Inflation** | None at launch. Validator rewards come from the staking allocation. Governance can vote to enable inflation if the staking pool is depleted. |

## Initial Distribution

| Allocation | Tokens | % | Vesting |
|-----------|--------|---|---------|
| **Staking & Validator Rewards** | 500,000,000 | 25% | Released over 10 years via epoch rewards |
| **Foundation Treasury** | 400,000,000 | 20% | Governed by on-chain governance; disbursed via grants |
| **Team & Founders** | 300,000,000 | 15% | 1-year cliff, 3-year linear vest (4 years total) |
| **Ecosystem Fund** | 300,000,000 | 15% | For dApp incentives, developer grants, partnerships |
| **Community & Airdrops** | 200,000,000 | 10% | Distributed at and after launch |
| **Early Investors** | 100,000,000 | 5% | 6-month cliff, 2-year linear vest |
| **Genesis Validators** | 100,000,000 | 5% | 1-year validator lock |
| **Liquidity & Market Making** | 100,000,000 | 5% | Available at launch |
| **Total** | **2,000,000,000** | **100%** | |

### Vesting Schedule

```
Year 0 (launch)
├── Circulating: Community (200M) + Liquidity (100M) + Genesis Validators (100M) = 400M (20%)
├── Genesis validator tokens locked for 1 year (staked, earning rewards)
├── Staking rewards begin accruing
└── Team, investors locked

Year 0.5
├── Investor cliff ends → linear unlock begins
└── Estimated circulating: ~450M

Year 1
├── Genesis validator lock expires (can unstake)
├── Team cliff ends → linear unlock begins
├── Staking rewards: ~50M released
└── Estimated circulating: ~550-650M

Year 2
├── Investors ~50% unlocked
├── Team ~25% unlocked
├── Staking rewards: ~100M cumulative
└── Estimated circulating: ~700-900M

Year 2.5
├── Investors fully vested
└── Estimated circulating: ~800-1.0B

Year 4
├── Team fully vested
└── Estimated circulating: ~1.0-1.2B

Year 10
└── Staking pool fully distributed → governance decides on inflation
```

## Staking

### Validators

Validators run nodes, propose blocks, and participate in consensus.

| Parameter | Value |
|-----------|-------|
| Minimum self-stake | 500,000 SOLEN |
| Minimum active validators | 20 |
| Genesis validator lock | 1 year (~157,680 epochs) |
| Slashing (double sign) | 10% of stake |
| Slashing (downtime) | 1% after 50 missed blocks |
| Unbonding period | 7 epochs |

Genesis validators cannot unstake for 1 year after network launch. This ensures network stability during the bootstrap phase. After the lock expires, they can deregister like any other validator, subject to the minimum validator count.

The network enforces a minimum of 20 active validators at all times. Any deregistration that would drop below this threshold is rejected.

Validators earn rewards from two sources:
1. **Epoch rewards** — distributed from the staking allocation at each epoch boundary, proportional to total stake (self-stake + delegations)
2. **Transaction fees** — the treasury share of fees (currently 50%) is governed by on-chain governance and can be directed to validators

### Delegators

Any token holder can delegate to a validator without running infrastructure.

| Parameter | Value |
|-----------|-------|
| Minimum delegation | No minimum |
| Reward share | Proportional to stake relative to validator's total |
| Unbonding period | 7 epochs (same as validators) |
| Slashing risk | Delegators share slashing risk with their chosen validator |

Delegators choose which validator to trust. If a validator is slashed, delegated tokens are slashed proportionally.

### Reward Calculation

Rewards come from the staking allocation (500M SOLEN over 10 years).

| Parameter | Value |
|-----------|-------|
| Total staking pool | 500,000,000 SOLEN |
| Distribution period | 10 years |
| Annual rewards | ~50,000,000 SOLEN (year 1, declining over time) |
| Epoch length | 100 blocks (~200 seconds at 2s block time) |
| Rewards per epoch | ~317 SOLEN |
| Payout frequency | Every epoch (~3.3 minutes) |

Each epoch, the reward pool is distributed across all active validators proportional to their total stake:

```
validator_reward = epoch_reward_pool × (validator_total_stake / network_total_stake)
```

Within a validator, rewards are split between the validator and its delegators proportional to their contribution:

```
delegator_reward = validator_reward × (delegator_stake / validator_total_stake)
```

### Example Earnings

Assuming 4 validators with equal stake and 50M SOLEN annual rewards:

| Scenario | Annual per validator | Monthly | Daily |
|----------|---------------------|---------|-------|
| 4 validators, no delegators | 12,500,000 SOLEN | ~1,041,667 | ~34,247 |
| 10 validators, no delegators | 5,000,000 SOLEN | ~416,667 | ~13,699 |
| 50 validators, no delegators | 1,000,000 SOLEN | ~83,333 | ~2,740 |
| 100 validators, no delegators | 500,000 SOLEN | ~41,667 | ~1,370 |

A validator with more delegated stake earns a larger share. Delegators earn proportionally but the validator can set a commission rate (future feature).

### Reward Schedule

Rewards decrease over time to extend the staking pool:

| Year | Annual Rewards | % of Pool |
|------|---------------|-----------|
| 1 | 50,000,000 | 10% |
| 2 | 50,000,000 | 10% |
| 3 | 50,000,000 | 10% |
| 4-5 | 45,000,000/yr | 9% |
| 6-7 | 40,000,000/yr | 8% |
| 8-10 | 30,000,000/yr | 6% |
| **Total** | **500,000,000** | **100%** |

After year 10, governance votes on whether to enable inflation (recommended: 1-2% annually) to continue funding validators.

### Target Staking Ratio

The network targets 40-60% of circulating supply staked. If staking participation falls below 40%, governance may increase epoch rewards to incentivize staking. If it exceeds 60%, rewards may decrease.

## Fee Model

| Parameter | Value |
|-----------|-------|
| Base fee per gas | 1 (adjustable via governance) |
| Burn rate | 50% of fees burned permanently |
| Treasury rate | 50% of fees to treasury |
| Gas (transfer) | 100 |
| Gas (contract call) | 500 + VM execution cost |
| Gas (deploy) | 1,000 |

### Fee Flow

```
User pays fee
├── 50% burned (removed from supply permanently)
└── 50% to treasury (governed by on-chain governance)
```

The burn creates deflationary pressure that partially offsets staking rewards, especially as network usage grows. At high transaction volumes, the burn rate can exceed reward issuance, making the token net-deflationary.

### Gas Abstraction

Users don't need to hold native tokens to transact. Paymasters can sponsor fees on behalf of users, enabling:
- dApps that pay for their users' transactions
- Fee payment in approved alternative assets
- Session-based spending policies

## Governance

Token holders participate in governance by voting with their staked tokens.

| Parameter | Value |
|-----------|-------|
| Quorum | 30% of staked supply must participate |
| Pass threshold | 66.67% supermajority |
| Voting period | 14 epochs |
| Timelock | 3 epochs after passing before execution |

Governance can modify:
- Base fee per gas
- Burn rate
- Block time
- Epoch rewards
- Staking parameters (minimum stake, unbonding period)
- Rollup registration
- Emergency pause/resume

Governance **cannot** modify:
- Total supply cap (requires hard fork)
- Vesting schedules for already-allocated tokens
- Consensus mechanism (requires hard fork)

## Rollup Economics

Rollups pay L1 fees for batch publication and proof verification. These fees follow the same burn/treasury split as regular transactions.

Rollup sequencers may charge additional fees on L2 which are independent of L1 tokenomics. Cross-domain bridge operations lock tokens in bridge vaults on L1 and mint equivalent representations on L2.

## Summary

The Solen token has a fixed initial supply of 2B with no inflation at launch. The fee burn creates deflationary pressure as network usage grows. Staking rewards come from a pre-allocated pool distributed over 10 years, after which governance decides whether to enable modest inflation to continue funding validators. The economic design prioritizes long-term sustainability over short-term incentive complexity.
