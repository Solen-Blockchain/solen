# Rollout runbook: fail-open canonicity fix (2026-07)

Canary-first, verify-gated rollout of the recovery-path fix for the 2026-07-19
validator8 restart-wedge (root cause: memory project-solen-checkpoint-gate-wedge;
commit `0aa5c9f`). Ships on top of the PQ Phase-1 binary. **Nothing activates**
— PQ stays dormant; no `--pq-auth-height` flag.

Fleet: `root@validator1..11` + `root@rpc1..4`. systemd unit `solen-node`, binary
`/opt/solen/bin/solen-node`, data `/opt/solen/data/mainnet`, localhost RPC 9944,
P2P 30333. Same mechanism as `rollout-pq-phase1-dormant-2026-07.md`.

## Deploy point
- **DEPLOY_COMMIT** = `0aa5c9f` (`main`). Binary delta over the deployed Phase-1
  binary = this ONE recovery-path commit (the intervening commits are runbook
  docs, no binary impact).
- **NEW_SHA** = `ed42882ae4bfdc0f397035638579d969812e490932b155a6f520f6704b6baeae` (short `ed42882ae4bf`). Built 2026-07-19 from DEPLOY_COMMIT.
- **OLD_SHA** = `3fc2cf239b329d6097a7b7f2f508fe94e520fc75e0641ff3e9d323954ca04fd6` (short `3fc2cf239b32`, the PQ Phase-1 binary, deployed 2026-07-19). Confirm it matches fleet-wide in §0.

Release builds aren't bit-reproducible across machines — **distribute this exact
`target/release/solen-node` file**; the per-node `sha256sum` verifies the transfer.

## What's shipping / why safe incrementally (NO flag-day)
Replaces the fail-CLOSED `checkpoint_is_canonical` bool with a three-way
`Canonicity {Canonical, Forked, Inconclusive}` probe that retries across resync
sources with backoff; the tiered-recovery tip guard now regresses ONLY on a
definitive `Forked` (an Inconclusive probe fails OPEN to the forward remote
snapshot instead of destructively walking backward and looping).

- **Recovery-path only.** No change to block execution, state roots, or block
  validity — old and new binaries compute identical blocks, so a mixed old/new
  window cannot fork. Roll node-by-node with the verify gate.
- **Each node is protected the moment it restarts** (it comes up on the new
  fail-open binary), so this rollout is *lower* wedge-risk than Phase 1 was.
- Pre-roll gates green (2026-07-19): `cargo build --workspace`, `cargo test -p
  solen-node` (8 new canonicity tests incl. the all-sources-silent → Inconclusive
  wedge trigger). Do a full `cargo test --workspace` before rolling.

## Lessons from the Phase-1 incident (bake into this roll)
- Pick a **low-RPC-load window** — do NOT run during other fleet churn (the
  wedge was amplified by transient `rpc.solenchain.io` load from 15 concurrent
  restarts). One node at a time keeps snapshot/RPC pressure minimal.
- Between every node, confirm the just-restarted node **caught up + is
  finalizing and did NOT log the checkpoint loop** (§B6) before the next.
- Optional hardening: add extra `--resync-url` entries (several rpc nodes) to the
  ExecStart so the retrying probe has independent sources. Not required.

---

## 0. Preconditions
- [ ] Full `cargo test --workspace` green on `0aa5c9f`.
- [ ] `ssh root@validator1 "sha256sum /opt/solen/bin/solen-node"` = OLD_SHA (`3fc2cf239b32…`); matches fleet-wide (Appendix A of the Phase-1 runbook).
- [ ] Fleet healthy: all 15 on one state_root, advancing.
- [ ] Fresh snapshot on the rpc nodes (recovery authority).
- [ ] Low-traffic window; avoid starting within ~10 blocks of an epoch boundary (height % 100 == 0).

## 1. Stage the new binary INACTIVE on every node
```bash
cd ~/solen
for h in validator{1..11} rpc{1..4}; do
  scp target/release/solen-node root@$h:/opt/solen/bin/solen-node.canonfix-2026-07
  ssh root@$h "sha256sum /opt/solen/bin/solen-node.canonfix-2026-07"   # MUST equal NEW_SHA (ed42882ae4bf…)
done
```
Abort if any sha differs. No systemd unit change.

## 2. Canary — one validator (validator6)
```bash
ssh root@validator6 '
  systemctl stop solen-node
  cp /opt/solen/bin/solen-node /opt/solen/bin/solen-node.pre-canonfix   # rollback copy
  cp /opt/solen/bin/solen-node.canonfix-2026-07 /opt/solen/bin/solen-node
  sha256sum /opt/solen/bin/solen-node
  systemctl start solen-node
'
```
**Verify gate (§B)** — watch ~3–5 min. Especially §B6: the restart must catch up
and finalize WITHOUT a `checkpoint not confirmed canonical` loop. If red →
rollback the canary (§7) and stop.

## 3. Validator rollout — one at a time, ≥10 up
Quorum = 8 of 11. Roll **one validator at a time** (max 1 down → 10 up) to keep
RPC/snapshot pressure minimal — the load conditions that amplified the wedge.
Order (validator1 LAST as the recovery anchor):
```
validator2, validator3, validator4, validator5, validator7, validator8,
validator9, validator10, validator11, then validator1
```
Per node — swap, then pass §B (esp. §B6) before the next:
```bash
ssh root@<node> '
  systemctl stop solen-node
  cp /opt/solen/bin/solen-node /opt/solen/bin/solen-node.pre-canonfix
  cp /opt/solen/bin/solen-node.canonfix-2026-07 /opt/solen/bin/solen-node
  systemctl start solen-node
'
```
`validator8` (the wedge victim) and `validator9` (slow disk) deserve an extra
look — but with the fix a slow-RPC restart should simply fail open and catch up.

## 4. RPC nodes (rpc2, rpc3, rpc4, then rpc1 last). Same swap; verify each
returns `solen_chainStatus` at the tip on the canonical root after restart.

## 5. Post-deploy
- [ ] All 15 on NEW_SHA, one state_root, advancing.
- [ ] Zero `checkpoint not confirmed canonical` loops / `needs manual` / `mismatch` alerts over the following hours.
- [ ] Clean up after ~24h stable: remove `solen-node.canonfix-2026-07`; KEEP `solen-node.pre-canonfix` until confident.

This fix is a prerequisite for the PQ Phase-3 flag-day (which restarts every
validator — the exact wedge trigger). Land + soak this first.

---

## 7. Rollback
Single node: `cp /opt/solen/bin/solen-node.pre-canonfix /opt/solen/bin/solen-node && systemctl restart solen-node`.
Full: revert every node to `solen-node.pre-canonfix` (safe — recovery-path only, no root change).
If a node ever wedges in the checkpoint loop (should not, given the fix): the
clean data-dir reset that recovered validator8 — stop → `mv /opt/solen/data/mainnet`
aside → restart (fresh snapshot resync; identity is the `--validator-seed` flag,
not in the data dir).

## Appendix B — verify gate (all must pass after each node)
1. **Participation**: `journalctl -u solen-node --since '90 sec ago' | grep -c 'finalized with quorum'` > 0.
2. **Agreement / no fork**: at a common settled height (tip−5), upgraded AND still-old nodes report the SAME state_root.
3. **Advancing**: fleet tip increases across two polls ~block-time apart.
4. **No anomalies**: no `state root mismatch on finalization`, no `needs manual intervention`, no `partition detected`.
5. **Quorum margin**: ≥10 validators `active`.
6. **NO WEDGE (the point of this fix)**: `journalctl -u solen-node --since '3 min ago' | grep -c 'checkpoint not confirmed canonical'` stays low and does NOT climb across polls; the node reaches the tip and finalizes within ~2 min of restart. A sustained/among-tiers `not confirmed canonical` loop = red gate → investigate (and confirm the node is actually on NEW_SHA).
