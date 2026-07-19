# Rollout runbook: 2026-07 state-root / liveness fixes

Canary-first, verify-gated rollout of the three fixes for the 2026-07-16 halt.
Fleet: `root@validator1..11` + `root@rpc1..4`. systemd unit `solen-node`,
binary `/opt/solen/bin/solen-node`, data `/opt/solen/data/mainnet`, localhost
RPC `9944`, P2P `30333`. Validator identity is the `--validator-seed` startup
flag (NOT the data dir).

## What's shipping (all in ~/solen, currently uncommitted)
1. **Liveness** — `engine.rs`: finalize-mismatch no longer latches `needs_resync`; rejects the bad block, lets the backup/v2 recover. Gated on `fc_v2_active` (mainnet v2 active since height 688050).
2. **Determinism (intent ordering)** — `intents/pool.rs`: `pending_intents()` sorted by id.
3. **Root cause (produce guard)** — `engine.rs`: `restore_store_to_finalized_tip()` at the top of `produce_block` undoes a leaked eager-commit before producing.

## Why this roll is SAFE to do incrementally (no flag-day / activation gate)
Unlike the 2026-06-25 `is_backup` roll that forked mainnet, **none of these
change block hashes or state roots**:
- The liveness fix changes only the *recovery action* on a mismatch, not any block's computed root. Gated on an already-active height, so old and new nodes agree on when it applies.
- The intent-ordering fix changes only *production* order; verifiers replay the block's baked ops regardless, so mixed old/new produce interoperates.
- The produce guard is a **no-op unless the store already drifted**; it never alters a correctly-produced block.

Therefore a mixed old/new binary window cannot safety-fork. Roll node-by-node
with a verify gate; NO simultaneous flag-day required. (Still: batched, gated,
canary-first — never a quick batched restart.)

Validated pre-roll: full `cargo test --workspace` green; `partition-drill`
(v2 off) Scenario A no-fork across 15 churn cycles + `fork-choice-v2-drill`
(v2 on) survived 2-down with no fork; produce-guard never misfired.

---

## 0. Preconditions

### 0a. Commit the fixes (granular, matching repo convention; direct to `main`)
Three modified files: `crates/solen-consensus/src/engine.rs` (the two consensus
fixes), `crates/solen-intents/src/pool.rs` (intent ordering),
`crates/solen-indexer/src/stsolen_apy.rs` (doctest fix).
```bash
cd ~/solen

# 1) the consensus fixes (liveness + produce guard)
git add crates/solen-consensus/src/engine.rs
git commit -m "consensus: fix state-root divergence + resync-halt (2026-07-16 mainnet halt)

Root cause: the produce path (execute_block_journaled) commits eagerly, so a
superseded produce attempt leaks writes into the store; at an epoch boundary
that double-applies the epoch reward and the next block diverges. produce_block
now calls restore_store_to_finalized_tip() first — a no-op unless the store has
drifted from the finalized tip.

Liveness: on a finalize state-root mismatch, reject the block and let the backup
proposer / v2 vote-change converge, instead of latching needs_resync (which
halted the whole honest majority at once). Gated on fc_v2_active; a new
v2_invalid set stops the bad block being re-accepted or re-elected as leader.

Neither change alters block hashes or state roots. Tests:
bad_root_block_rejected_without_resync_then_backup_finalizes,
produce_restores_store_after_leaked_eager_commit."

# 2) deterministic intent ordering
git add crates/solen-intents/src/pool.rs
git commit -m "intents: sort pending_intents by id for deterministic block production

HashMap iteration order is per-process random; the proposer injects fulfill ops
in that order, breaking the deterministic re-proposal invariant. Sort by id.
Test: pending_intents_are_deterministically_ordered."

# 3) trivial doctest fix (unrelated cleanup surfaced by cargo test --workspace)
git add crates/solen-indexer/src/stsolen_apy.rs
git commit -m "indexer: fence stSOLEN APY math block as text (fix doctest)"
```

### 0b. Tag the deploy point (annotated) and push
```bash
git tag -a mainnet-stateroot-fix-2026-07-17 \
  -m "State-root divergence + resync-halt fixes (2026-07-16 incident). cargo test --workspace + devnet drills green."
git push origin main
git push origin mainnet-stateroot-fix-2026-07-17
git rev-parse --short HEAD   # record = DEPLOY_COMMIT
```

### 0c. Build + record shas
- [x] `cargo build --release -p solen-node` (from `DEPLOY_COMMIT` = `96f4efb`, tag `mainnet-stateroot-fix-2026-07-17`) → **NEW_SHA = `a38ff4e126b67987347683f4cb9b7405a54d6bb7b0a2043fb93973e7641d0f35`** (short `a38ff4e126b6`). Built 2026-07-17.
  - Stage THIS exact binary (`target/release/solen-node`) to the fleet in §1; the per-node `sha256sum` check confirms the transfer matches `a38ff4e126b6…` (release builds aren't bit-reproducible across machines, so distribute the file — don't rebuild per node).
- [ ] Record the **current** deployed sha: `ssh root@validator1 "sha256sum /opt/solen/bin/solen-node"` = **OLD_SHA** (should match across the fleet).
- [ ] Fleet healthy now: run the step-0 poll (Appendix A) — all 15 on one root, advancing.
- [ ] Fresh snapshot exists on the rpc nodes (the canonical authority for recovery).
- [ ] Pick a low-traffic window. Avoid starting within ~10 blocks of an epoch boundary (height % 100 == 0) so the first upgraded nodes settle before one.

## 1. Stage the new binary INACTIVE on every node
Copy to a staging path; do NOT swap yet.
```bash
for h in validator{1..11} rpc{1..4}; do
  scp target/release/solen-node root@$h:/opt/solen/bin/solen-node.new-2026-07
  ssh root@$h "sha256sum /opt/solen/bin/solen-node.new-2026-07"   # MUST equal NEW_SHA
done
```
Abort if any sha differs.

## 2. Canary — one validator (pick a non-critical mid-fleet node, e.g. validator6)
```bash
ssh root@validator6 '
  systemctl stop solen-node
  cp /opt/solen/bin/solen-node /opt/solen/bin/solen-node.old-pre-fixes   # rollback copy
  cp /opt/solen/bin/solen-node.new-2026-07 /opt/solen/bin/solen-node
  sha256sum /opt/solen/bin/solen-node
  systemctl start solen-node
'
```
**Verify gate (Appendix B)** — must ALL hold before proceeding, watch for ~3–5 min:
- canary is `active`, catching up then finalizing at the tip;
- canary's state_root == the fleet's at a common settled height (tip-5);
- canary logs `block finalized with quorum` (it's participating, not just following);
- no `mismatch:` / `manualint:` / `partition` alerts from the monitor;
- fleet tip still advancing ~1 block / block-time.

If the gate fails → **rollback the canary** (§8) and stop.

## 3. Batched validator rollout — pairs, ≥9 up at all times
Quorum is 2/3 of 11 = **8 nodes**. Upgrade in **pairs** (≤2 down → ≥9 up = 82%
margin). After each pair, pass the verify gate before the next.

Order (validator1 LAST as the old-binary recovery anchor):
```
pair 1: validator2, validator3
pair 2: validator4, validator5
pair 3: validator7, validator8      # 6 already canaried
pair 4: validator9, validator10
single: validator11
last:   validator1
```
For each node in a pair (run the pair concurrently, then gate):
```bash
ssh root@<node> '
  systemctl stop solen-node
  cp /opt/solen/bin/solen-node /opt/solen/bin/solen-node.old-pre-fixes
  cp /opt/solen/bin/solen-node.new-2026-07 /opt/solen/bin/solen-node
  systemctl start solen-node
'
```
**Verify gate after every pair** (Appendix B). Do not proceed on a red gate.

Note the **cross-binary check**: at each gate, confirm upgraded nodes and
still-old nodes share the **same state_root at a common settled height** — this
is the live proof that old/new interoperate with no fork.

## 4. validator1 last
Only after 2–11 are green. Same steps. validator1 stayed on the old binary
throughout so it (with the rpc nodes) could anchor a recovery if anything went
wrong mid-roll.

## 5. RPC nodes last (rpc1..4) — non-validators, zero quorum impact
Upgrade rpc2, rpc3, rpc4 first (rpc1 last — it's the recovery authority source).
Same swap. Verify each returns `solen_chainStatus` at the tip on the canonical
root after restart.

## 6. Post-deploy watch (critical — this is a boundary-triggered bug)
- [ ] All 15 on **NEW_SHA**, one state_root, advancing.
- [ ] **Watch the next epoch boundary** (next height % 100 == 0) cross cleanly — proposer produces, `checkpoint FINALIZED (2/3+ quorum)`, no divergence. This is the exact condition that triggered the incident; the fix must be observed working through at least one boundary.
- [ ] Zero `mismatch:` / `manualint:` alerts over the following few hours (the new fleet_monitor.py alerts).
- [ ] Clean up staging + backups only after ~24h stable: remove `solen-node.new-2026-07`; KEEP `solen-node.old-pre-fixes` until you're confident.

---

## 7. Abort / rollback
**Single node misbehaving** (canary or one validator): revert just it.
```bash
ssh root@<node> 'systemctl stop solen-node
  cp /opt/solen/bin/solen-node.old-pre-fixes /opt/solen/bin/solen-node
  systemctl start solen-node'
```

**Fleet wedged / a same-height competing-block or attestation-split latch**
(should not happen with these fixes, but the universal recovery): restarts alone
NEVER clear it — go straight to the **rpc1-authority data-dir copy** (see
`consensus-fork-recovery.md` §; and [[project_solen_partition_deadlock]]):
1. Stop the wedged validators.
2. `tar` rpc1's `/opt/solen/data/mainnet` (exclude `checkpoints/`, `rocks-checkpoints/`, `IDENTITY`) → restore onto each wedged validator → `rm IDENTITY` → restart canary-first.
3. Chain resumes on the canonical rpc1 state; nodes reform quorum.

**Full rollback to the old binary**: revert every node to `solen-node.old-pre-fixes`
(validators in pairs, verify gate). Safe because the fixes don't change roots, so
new→old is as interoperable as old→new.

---

## Appendix A — fleet health poll (read-only)
```bash
for i in $(seq 1 11); do
  ( R=$(ssh -o ConnectTimeout=8 root@validator$i "curl -s --max-time 8 -X POST 127.0.0.1:9944 -H 'content-type: application/json' -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"solen_chainStatus\",\"params\":[]}'")
    H=$(echo "$R"  | grep -oE '\"height\":[0-9]+' | head -1 | cut -d: -f2)
    SR=$(echo "$R" | grep -oE '\"state_root\":\"[0-9a-f]{12}' | head -1 | cut -d'\"' -f4)
    V=$(ssh -o ConnectTimeout=8 root@validator$i "sha256sum /opt/solen/bin/solen-node | cut -c1-12")
    echo "validator$i h=$H root=$SR sha=$V" ) &
done; wait
```

## Appendix B — verify gate (must ALL pass after each step)
1. **Liveness/participation**: each upgraded node logs `block finalized with quorum` within ~2 block-times of restart:
   `ssh root@<node> "journalctl -u solen-node --since '90 sec ago' | grep -c 'finalized with quorum'"` → > 0.
2. **Agreement / no fork**: at a common settled height (tip−5), upgraded AND still-old nodes report the **same** state_root (Appendix A columns match).
3. **Advancing**: fleet tip increases across two successive polls ~block-time apart.
4. **No anomalies**: `journalctl` on each upgraded node shows no `state root mismatch on finalization`, no `needs manual intervention`, no `partition detected` loop.
5. **Quorum margin held**: ≥9 validators `active` throughout.

A single failed check = red gate → stop, diagnose, roll back the last step.
