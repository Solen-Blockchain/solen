# Post-mortem: single-validator state-root fork at an epoch boundary (2026-07-16)

**Severity:** consensus fork (one validator diverged), no loss of funds, no
double-spend. Quorum remained on the canonical chain throughout.
**Root-cause class:** non-deterministic execution at the epoch-boundary
transition — one validator computed a different state root than the honest
majority for the same block.

## What happened

At an epoch boundary (near height 1,028,501) a single validator finalized a block
whose state root differed from the rest of the fleet's, forking itself off the
canonical chain. The remaining validators held quorum and continued to finalize,
so the network stayed live and safe; the diverged node could not re-join because
its local state no longer matched the canonical chain.

Notably, attestation-aware fork choice (v2) was already active and did not prevent
this — because it was not a *competing-block liveness* problem (which v2
addresses) but a *state-root divergence* problem: the two nodes had genuinely
different post-execution state, not two valid views of the same state. This
distinction is why it read as a new failure class.

## Root cause

The epoch-boundary transition (which recomputes validator-set/reward state)
executed non-deterministically across heterogeneous nodes: given the same inputs,
nodes could produce a byte-different state, hence a different state root. On a
BFT chain the state root is the consensus object, so any non-determinism in the
transition is a fork generator. The specific trigger lived in the boundary
transition path; the general defect was that parts of execution were not
guaranteed byte-identical across hosts/builds.

## Recovery

Standard divergence recovery, since quorum was healthy:

1. Stop the diverged validator (the "culprit" on the minority state).
2. Confirm the majority is finalizing the canonical chain.
3. Reset the diverged node's data directory and re-sync from a canonical snapshot
   (identity preserved via its validator seed, which is not part of the data
   directory).

The node rejoined on the canonical chain and resumed finalizing with quorum.

## Follow-ups this incident drove

- A **WASM determinism-hardening** activation (bounded linear memory +
  deterministic relaxed-SIMD), gated behind a height flag so it can be turned on
  fleet-wide in a coordinated flag-day without a mid-flight fork.
- A documented **divergence-recovery runbook** and monitor alerts for state-root
  disagreement and single-node stranding.
- A broader program to ensure every consensus-relevant value is a pure function
  of agreed state — which directly informed the fix for the 2026-07-28
  epoch-boundary halt (see that post-mortem).

## Lessons

1. **Fork choice fixes liveness, not determinism.** A chain can have perfect fork
   choice and still fork if execution isn't byte-identical across nodes.
2. **Epoch-boundary transitions are high-risk** because they run rarely, touch
   more state than normal blocks, and are exercised less by ordinary testing —
   they deserve dedicated differential/heterogeneous-host testing.
3. **Keep recovery boring:** a rehearsed stop-culprit → verify-quorum →
   snapshot-resync procedure turns a scary fork into routine ops when quorum is
   safe.
