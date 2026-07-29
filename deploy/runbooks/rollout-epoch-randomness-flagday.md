# Rollout runbook: epoch-randomness accumulator (variant B) flag-day

Activates the grind-resistant epoch-randomness seed (commit `7b1752a`). The epoch
proposer seed moves from `blake3(state_root of the boundary block)` — the shipped
halt fix `c20fc0b`, still proposer-influenceable — to a value **accumulated in
state** from every block's parent `state_root`, snapshotted at each boundary.

**Why:** the boundary proposer can grind the current seed via its txs → state_root.
Variant B dilutes that to ~1/EPOCH_LENGTH (one block's contribution) and, by living
in state, is deterministic + restart-safe. Full unbiasability still wants a VRF —
this is the big cheap step, not the finish.

## The one property that makes this a real flag-day
At/after H the executor **writes the accumulator to state every block**, so it
**changes `state_root`**. This is a STATE-AFFECTING activation (like `fee-fix`),
not a mere proposer-selection tweak:
- **All 15 nodes must carry the SAME H and reach it before the chain does.** A node
  with a different/unset H computes a different state at H → divergence → the exact
  epoch-boundary halt class `c20fc0b` fixed, re-triggered by misconfiguration.
- **Point of no return = H.** Below H: unset the flag → byte-identical → clean
  rollback. Once blocks at/after H finalize, the accumulator is baked into canonical
  state; rollback then diverges from finalized history.

Correctness under adversarial restart timing is already PROVEN by
`tools/devnet/epoch-randomness-drill.sh` (9/9). The residual risk is entirely
operational discipline (the convergence gate), not the code.

---

## Deploy point
- **DEPLOY_COMMIT** = `6d3dc18` (`main`). Binary = current live code + dormant
  variant B; behaviorally identical with the flag unset.
- **NEW_SHA** = `28fdaa8bd05b504d…` (record full sha at build; `cargo test --workspace` green).
- **OLD_SHA (live)** = `5ddb47bede26b5bf…` = `c20fc0b` (epoch-seed fix; NO variant B —
  its `chainStatus.config` lacks `epoch_randomness_height`).

Distribute this exact `target/release/solen-node`; per-node `sha256sum` verifies.

---

## P0 — Pre-flight
- [ ] `git status` clean at `6d3dc18`; build `-p solen-node`; record NEW_SHA.
- [ ] `cargo test --workspace` green (incl. `solen-system-contracts::epoch_randomness`).
- [ ] **Re-run the determinism drill on the exact build:** `tools/devnet/epoch-randomness-drill.sh`
      → expect `9 passed, 0 failed` (Gate B = staggered restarts straddling a boundary,
      unanimous state_root). This is the flag-day gate; do not proceed without it.

## P1 — Deploy dormant (rolling, NOT a flag-day)
Dormant variant B computes IDENTICAL state roots and the SAME legacy seed as the live
`5ddb47`, so mixed old/new is safe — a routine quorum-protected rolling deploy.
```bash
cd ~/solen
for h in validator{1..11} rpc{1..4}; do
  scp target/release/solen-node root@$h:/opt/solen/bin/solen-node.varB
  ssh root@$h "sha256sum /opt/solen/bin/solen-node.varB"   # MUST equal NEW_SHA
done
```
Then per node (canary an rpc node first → soak → validators, `validator1` last, rpc last):
```bash
ssh root@<node> 'systemctl stop solen-node
  cp /opt/solen/bin/solen-node /opt/solen/bin/solen-node.pre-varB   # rollback
  cp /opt/solen/bin/solen-node.varB /opt/solen/bin/solen-node'      # cp fails ETXTBSY while running — stop first
ssh root@<node> 'systemctl start solen-node'                        # STANDALONE restart (the compound-ssh gremlin)
```
- Jemalloc drop-in stays (LD_PRELOAD unaffected by the binary swap).
- **Gate:** all 15 on NEW_SHA; `chainStatus.config.epoch_randomness_height` now
  **appears** = `18446744073709551615` fleet-wide (the field showing up at all is the
  convergence signal — the live binary omits it); unanimous root; advancing.
- **Rollback:** revert any node to `solen-node.pre-varB` + restart — safe (dormant-equivalent).

## P2 — Pick H + soak
Clean future epoch boundary + soak margin. 6s/block ≈ 14,400 blocks/day.
- **Recommended H = boundary + 1** (e.g. `1_250_001` = boundary 1,250,000 + 1): lets P1
  deploy + soak, AND the `+1` means the accumulator builds over a full epoch
  (H → H+99) before its seed is first *used* at the next boundary (H+99) → a
  well-mixed first seed instead of a single-block one.
- Adjustable until P3 restarts begin. Confirm it is comfortably ahead of live height.

## P3 — Flag-day activation
Append the flag to each node's systemd ExecStart (drop-in), daemon-reload, restart.
Behavior-neutral below H → restart order does not matter, zero divergence window
below H — but do it methodically with standalone restarts.
```bash
# per node, drop-in that redefines ExecStart with the flag appended (see the
# epoch-seed / pq-phase3 rollouts for the ExecStart-capture-and-append pattern)
--epoch-randomness-height <H>
```
- **HARD GATE:** all 15 report `epoch_randomness_height=<H>` via `chainStatus`
  **well before** the chain reaches H. Never let the chain approach H with any node
  unset — that is the misconfig that halts it.
- **At H:** executor starts accumulating (state changes, all nodes together); at the
  first boundary ≥ H consensus switches to the accumulator seed. Same code + same H →
  identical switch on every node.
- **Verify across the first post-H boundary:** unanimous `state_root`, chain crosses
  cleanly, consistent "epoch seed updated" logs. Then soak across several boundaries.

## Rollback
- **Before the chain reaches H:** unset `--epoch-randomness-height` on all nodes +
  restart → back to the legacy seed, byte-identical. Clean.
- **After H finalizes:** the accumulator is in canonical state → coordinated-only,
  effectively not rollback-able (same discipline as `fee_fix_height`). Treat H as a
  hard commit.
- Binary rollback (to before variant B code) is separate and only needed if the
  dormant binary itself misbehaves in P1 — use `solen-node.pre-varB`.

## Notes
- Not urgent: variant B sits dormant indefinitely at zero cost. Run on a calm day
  with a soak window.
- Do NOT bundle with any other consensus activation — keep it bisectable.
- After a full soak post-activation, clean up staging (`solen-node.varB`,
  `solen-node.pre-varB`).
