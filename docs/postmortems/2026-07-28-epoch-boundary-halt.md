# Post-mortem: epoch-boundary halt from a non-canonical proposer seed (2026-07-28)

**Severity:** chain halt (liveness), no loss of funds, no safety violation.
**Duration of halt:** the chain stopped finalizing at an epoch boundary until a
one-line consensus fix was deployed fleet-wide.
**Root cause:** the epoch proposer-selection seed was derived from the block
*hash* (which includes non-canonical header fields) instead of the agreed
*state root*, so honest nodes that agreed on state could still disagree on the
next epoch's proposer schedule.

We publish this in full because "it never breaks" is not a credible claim for a
young, independently-built consensus engine — "when it breaks, we find the exact
cause and fix it deterministically" is.

## What happened

At an epoch boundary (height 1,209,600), the chain stopped finalizing. Every
validator agreed on the state root of the last finalized block, yet each began
proposing its *own* next block and none attested the others — the signature of a
disagreement about **who the proposer is**, not about the state.

## Root cause

Solen selects each epoch's proposer schedule using a per-epoch seed. That seed
was computed as:

```rust
// crates/solen-consensus/src/engine.rs (before the fix)
let seed = blake3(block_hash(&last_block.header));
```

`block_hash()` hashes the entire header, which includes `proposer` and
`timestamp_ms`. Those fields are **not canonical across state-equivalent blocks**.
Solen's slashing logic deliberately treats two blocks with the same
`parent_hash`, `transactions_root`, and `state_root` but a different
`proposer`/`timestamp_ms` as *the same logical block* (a normal re-proposal after
a restart, not equivocation). So two honest nodes can finalize a
state-equivalent boundary block that hashes differently.

The proposer seed lived in in-memory consensus state, **outside the state root**.
The result: nodes that agreed perfectly on `state_root` could hold boundary
blocks with different `block_hash`, compute different seeds, and therefore derive
**different proposer schedules** for the next epoch. Each node concluded it was
the rightful proposer, proposed its own block, and nobody reached quorum → halt.

This was not caught earlier because within an epoch the seed is fixed and shared;
the divergence only manifests at the boundary where the new seed is derived, and
only when boundary blocks are state-equivalent-but-hash-different (which
restart/backup timing makes common).

## The fix

Derive the seed from an **agreed, canonical on-chain value** — the state root —
which is identical on every honest node by definition of consensus:

```rust
// after the fix
let seed = blake3(&last_block.header.state_root);
```

We verified byte-for-byte that `blake3(state_root)` equals the previous
computation at the tip on the running (pre-fix) fleet before shipping, so the
change is value-preserving in the normal case and only removes the divergence
path. It is consensus-layer only and does not alter any state root.

**Recovery mechanics.** Because the fix is deterministic, once every node ran the
new binary and restarted, they re-derived a single shared seed and immediately
converged — the chain resumed and then crossed the *next* epoch boundary
(1,209,700) with a unanimous state root across all nodes, confirming the fix
end-to-end rather than just un-sticking the immediate halt.

## Follow-up hardening

The state-root seed is deterministic but still influenced by the boundary block's
proposer (via the transactions that determine the state root). To dilute that, we
implemented a **grind-resistant epoch-randomness accumulator** that mixes an
agreed per-block contribution into a value committed to state and snapshots it as
the seed at each boundary — so no single proposer controls the schedule, and the
seed is restart-safe (loaded from state, never recomputed from volatile memory).
It ships **dormant** behind a height-gated flag and is validated by a
staggered-restart determinism drill (`tools/devnet/epoch-randomness-drill.sh`)
before any coordinated activation. A VRF is the eventual end state for full
unbiasability.

## Lessons

1. **Any value that influences consensus must be a pure function of agreed
   state.** Deriving consensus-relevant randomness from block *identity* (hash,
   proposer, timestamp) rather than block *state* is a latent fork generator.
2. **State-equivalent ≠ hash-equivalent.** Once the protocol intentionally
   accepts re-proposed, state-equivalent blocks, nothing downstream may depend on
   their hash.
3. **Verify equivalence before shipping a "same value" fix**, and prove a fix by
   crossing the *next* instance of the failure condition, not just clearing the
   current one.
4. **Determinism drills belong in CI-adjacent tooling**: the staggered-restart
   drill reproduces exactly the timing that exposed this class.
