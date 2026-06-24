# Devnet partition drill

A self-contained harness that reproduces the consensus failure modes behind the
mainnet incidents (2026-06-08 deadlock, 06-23 8-way fork, 06-24 fork cascade)
and asserts the fixes hold: **the fleet never forks and always self-heals with
no manual intervention.**

## Run

```bash
tools/devnet/partition-drill.sh          # release binary (realistic timing)
tools/devnet/partition-drill.sh --debug  # debug binary (faster build)
```

Exit code 0 = all checks passed. Logs land in `/tmp/solen-drill/n*.log`.

## What it spins up

An **isolated** 4-validator devnet on localhost (`--network devnet`, so the
remote resync URLs are empty and it never touches real testnet/mainnet). Genesis
is `drill-genesis.json` (equal stake, 1s blocks, known seeds `01`–`04`). Nodes
mesh via mDNS + explicit bootstrap.

4 validators is the BFT-meaningful minimum: 3/4 stake = quorum, 2/4 = no quorum.

## Scenarios

| # | Scenario | Asserts |
|---|----------|---------|
| A | 15 random kill/restart cycles (proposer churn) | **No competing-block fork** — every live node agrees on the state root at each settled common height. This is the core check for fix #2 (deterministic backup proposer + slashing determinism). |
| B | Kill 2/4 (quorum lost) → restart | Chain **halts** under the minority (strict quorum refuses to finalize), then **self-heals** and advances once quorum returns — the deterministic partition prober, no manual step. |
| C | Kill 1/4 (quorum survives) → restart | The 3/4 majority keeps finalizing; the straggler **rejoins and catches up**. |

The invariant checked throughout is **agreement**: at a settled common height,
all live validators must share the same block state root. A single mismatch is a
fork and fails the drill.

## Why this matters / what it can't yet cover

Run this **before any consensus-affecting redeploy**. The 06-24 incident was
caused by shipping the strict `has_quorum` without the competing-block fix; this
drill is exactly what would have caught it.

Limitation: fix #1's *resync recovery tiers* (rollback journal → checkpoint →
snapshot → remote) can't be fully exercised on an isolated devnet, because they
verify candidate state against a canonical seed RPC and devnet has none. The
drill validates that fix #2 **prevents** the strands those tiers recover from,
plus the prober self-heal. To exercise the resync tiers directly, run against a
seeded environment (a `--resync-url` flag pointing at a canonical node would let
this happen on devnet — a small follow-up).
