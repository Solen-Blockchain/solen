# Rollout runbook: PQ Phase 1 — dormant fleet deploy (2026-07)

Canary-first, verify-gated rollout of the current `main` binary to the whole
fleet **with post-quantum auth still DORMANT**. This is Phase 1 of the PQ
activation plan (memory: project-solen-pq-activation) — a routine, behavior-
neutral binary refresh. It does **NOT** activate anything. The flag-day that
turns PQ on (`--pq-auth-height H`) is Phase 3, a separate coordinated step.

Fleet: `root@validator1..11` + `root@rpc1..4`. systemd unit `solen-node`,
binary `/opt/solen/bin/solen-node`, data `/opt/solen/data/mainnet`, localhost
RPC `9944`, P2P `30333`. Validator identity is the `--validator-seed` startup
flag. Same mechanism as `rollout-stateroot-fixes-2026-07.md`.

## Deploy point
- **DEPLOY_COMMIT** = `5ac8ee4` (`main`), delta over the deployed binary is 3 commits:
  - `d12cd91` governance emergency-action fast-track (**dormant**, gated)
  - `321da16` rpc: expose `pq_auth_height` in `chainStatus.config`
  - `5ac8ee4` tools/devnet PQ canary drill (no binary impact)
- **NEW_SHA** = `3fc2cf239b329d6097a7b7f2f508fe94e520fc75e0641ff3e9d323954ca04fd6` (short `3fc2cf239b32`). Built 2026-07-19 from DEPLOY_COMMIT.
- **OLD_SHA** = `a38ff4e126b67987347683f4cb9b7405a54d6bb7b0a2043fb93973e7641d0f35` (short `a38ff4e126b6`, deployed 2026-07-17). Confirm it still matches fleet-wide in §0.

Release builds are not bit-reproducible across machines — **distribute this exact
`target/release/solen-node` file**; the per-node `sha256sum` verifies the transfer.

## Why this roll is SAFE incrementally (NO flag-day / no activation gate)
Like the state-root roll, **nothing here changes block hashes or state roots**:
- **PQ verify code is already deployed** (in `a38ff4e126b6`) and dormant — this
  roll does NOT add it. It only adds the `chainStatus.config.pq_auth_height`
  **display field** (RPC output, non-consensus, additive serde) so wallets can
  detect activation. With no `--pq-auth-height` flag, `pq_auth_height` stays
  `u64::MAX` and `verify_auth` returns `None` for MlDsa/Hybrid — identical to the
  old binary.
- **Emergency fast-track is dormant**: `emergency_fasttrack_active(epoch)` is
  `epoch >= u64::MAX` = always `false`, so `finalize()`/`execute()` collapse to
  the exact pre-existing branch (`if !emergency && …` → `if …`). The normal
  governance path is byte-identical (test: `fasttrack_does_not_affect_normal_proposals`).
- No account on mainnet uses MlDsa/Hybrid auth (PQ dormant), so the PQ code path
  is never exercised by any real op.

Therefore a mixed old/new window cannot safety-fork. Roll node-by-node with the
verify gate; **do NOT pass `--pq-auth-height` anywhere** (that is Phase 3).

Pre-roll validation (2026-07-19): delta crate tests green
(`solen-system-contracts`/`solen-execution`/`solen-rpc`); PQ Phase-0 canary
(`tools/devnet/pq-auth-drill.sh`) green (A/B/C deterministic, D illustrative).
**Do before rolling:** full `cargo test --workspace` green on DEPLOY_COMMIT.

---

## 0. Preconditions
- [ ] Full `cargo test --workspace` green on `5ac8ee4`.
- [ ] Record current deployed sha: `ssh root@validator1 "sha256sum /opt/solen/bin/solen-node"` = **OLD_SHA** (`a38ff4e126b6…`); confirm it matches across the fleet (Appendix A).
- [ ] Fleet healthy now: Appendix A — all 15 on one state_root, advancing.
- [ ] Fresh snapshot exists on the rpc nodes (recovery authority).
- [ ] Low-traffic window; avoid starting within ~10 blocks of an epoch boundary (height % 100 == 0).

## 1. Stage the new binary INACTIVE on every node
```bash
cd ~/solen
for h in validator{1..11} rpc{1..4}; do
  scp target/release/solen-node root@$h:/opt/solen/bin/solen-node.pq-phase1-2026-07
  ssh root@$h "sha256sum /opt/solen/bin/solen-node.pq-phase1-2026-07"   # MUST equal NEW_SHA (3fc2cf239b32…)
done
```
Abort if any sha differs. **No systemd unit change** — the ExecStart line stays
exactly as-is (validator-seed + bootstrap only; no `--pq-auth-height`).

## 2. Canary — one validator (validator6)
```bash
ssh root@validator6 '
  systemctl stop solen-node
  cp /opt/solen/bin/solen-node /opt/solen/bin/solen-node.old-pre-pq       # rollback copy
  cp /opt/solen/bin/solen-node.pq-phase1-2026-07 /opt/solen/bin/solen-node
  sha256sum /opt/solen/bin/solen-node                                      # == NEW_SHA
  systemctl start solen-node
'
```
**Verify gate (Appendix B)** — watch ~3–5 min; all must hold, including the new
**§B6 pq_auth_height check**. If red → rollback the canary (§7) and stop.

## 3. Batched validator rollout — pairs, ≥9 up at all times
Quorum = 2/3 of 11 = **8**. Upgrade in **pairs** (≤2 down → ≥9 up). Gate after each pair.
Order (validator1 LAST as the old-binary recovery anchor):
```
pair 1: validator2, validator3
pair 2: validator4, validator5
pair 3: validator7, validator8      # 6 already canaried
pair 4: validator9, validator10
single: validator11
last:   validator1
```
Per node:
```bash
ssh root@<node> '
  systemctl stop solen-node
  cp /opt/solen/bin/solen-node /opt/solen/bin/solen-node.old-pre-pq
  cp /opt/solen/bin/solen-node.pq-phase1-2026-07 /opt/solen/bin/solen-node
  systemctl start solen-node
'
```
**Verify gate after every pair.** Cross-binary check: upgraded and still-old
nodes share the **same state_root at a common settled height** — live proof of
old/new interop with no fork.

Note: `validator9` is the chronic slow-disk straggler (memory:
reference_validator9_host_issue) — expect slower catch-up; gate on agreement, not speed.

## 4. validator1 last
Only after 2–11 are green. Same steps. validator1 + rpc nodes anchor recovery.

## 5. RPC nodes last (rpc1..4) — non-validators, zero quorum impact
Upgrade rpc2, rpc3, rpc4 first (rpc1 last — recovery-authority source). Same swap.
Verify each returns `solen_chainStatus` at the tip on the canonical root AND now
reports `config.pq_auth_height` (§B6).

## 6. Post-deploy watch
- [ ] All 15 on **NEW_SHA**, one state_root, advancing.
- [ ] `chainStatus.config.pq_auth_height == "18446744073709551615"` on every node (Appendix C) — the field is live fleet-wide AND still dormant.
- [ ] **End-to-end**: the wallet Security tab (against `https://rpc.solenchain.io`) shows "Post-quantum auth: not yet activated" and the upgrade button disabled. This is the whole point of Phase 1 — the wallet can now detect activation state.
- [ ] Watch the next epoch boundary (height % 100 == 0) cross cleanly.
- [ ] Zero `mismatch:` / `manualint:` / `partition` alerts (fleet_monitor.py) over the next few hours.
- [ ] Clean up staging/backups only after ~24h stable: remove `solen-node.pq-phase1-2026-07`; KEEP `solen-node.old-pre-pq` until confident.

**Phase 1 done ≠ PQ active.** Accounts still cannot use PQ auth (and the wallet
won't offer the upgrade) until Phase 3 sets `--pq-auth-height H` fleet-wide.

---

## 7. Abort / rollback
**Single node**: revert just it.
```bash
ssh root@<node> 'systemctl stop solen-node
  cp /opt/solen/bin/solen-node.old-pre-pq /opt/solen/bin/solen-node
  systemctl start solen-node'
```
**Full rollback**: revert every node to `solen-node.old-pre-pq` (validators in
pairs, verify gate). Safe — new→old is as interoperable as old→new (no root change).

**Fleet wedged** (should not happen; universal recovery): restarts alone never
clear a same-height competing-block latch — go to the **rpc1-authority data-dir
copy** (see `rollout-stateroot-fixes-2026-07.md` §7 / consensus-fork-recovery /
memory: project_solen_partition_deadlock).

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
1. **Participation**: `journalctl -u solen-node --since '90 sec ago' | grep -c 'finalized with quorum'` > 0 on each upgraded node.
2. **Agreement / no fork**: at a common settled height (tip−5), upgraded AND still-old nodes report the **same** state_root (Appendix A columns match).
3. **Advancing**: fleet tip increases across two polls ~block-time apart.
4. **No anomalies**: no `state root mismatch on finalization`, no `needs manual intervention`, no `partition detected` loop.
5. **Quorum margin**: ≥9 validators `active` throughout.
6. **PQ dormant + exposed (NEW)**: on each upgraded node,
   `curl -s --max-time 8 -X POST 127.0.0.1:9944 -H 'content-type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"solen_chainStatus","params":[]}' | grep -o '"pq_auth_height":"[0-9]*"'`
   → `"pq_auth_height":"18446744073709551615"` (field present = new binary; value = u64::MAX = still dormant). Still-old nodes omit the field — expected.

A single failed check = red gate → stop, diagnose, roll back the last step.

## Appendix C — fleet pq_auth_height sweep (post-deploy)
```bash
for h in validator{1..11} rpc{1..4}; do
  V=$(ssh -o ConnectTimeout=8 root@$h "curl -s --max-time 8 -X POST 127.0.0.1:9944 -H 'content-type: application/json' -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"solen_chainStatus\",\"params\":[]}'" | grep -o '"pq_auth_height":"[0-9]*"')
  echo "$h $V"
done
# Expect every node: "pq_auth_height":"18446744073709551615"
```
