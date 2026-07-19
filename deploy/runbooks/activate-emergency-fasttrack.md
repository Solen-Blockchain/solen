# Runbook: activate the emergency governance fast-track

Turns the dormant emergency fast-track ON so `EmergencyPause`/`EmergencyResume`
proposals finalize the instant they hit quorum (30%) + supermajority (66.67%) and
execute with **no** timelock. Normal proposals are unaffected. Implemented
2026-07-19, dormant by default — see [[project_solen_emergency_fasttrack]].

**This is a CONSENSUS-AFFECTING change** — it alters *when* a proposal executes,
which is in the state root. A node running the fast-track will execute an emergency
proposal at an epoch a non-fast-track node rejects → fork. So it MUST activate
fleet-wide at a single agreed epoch via a coordinated, canary-first roll (the same
discipline as the state-root fixes and `fork_choice_v2`). Do NOT enable it ad hoc.

Fleet: `root@validator1..11` + `root@rpc1..4`. systemd unit `solen-node`, binary
`/opt/solen/bin/solen-node`.

---

## 0. Decide two things
- **Activation epoch** `ACT` — a FUTURE epoch far enough out that all 15 nodes are
  provably on the new binary before it. Epoch = 100 blocks ≈ 10 min, so pick at
  least a day of headroom (e.g., current epoch + ~200 ≈ +33h). Confirm current
  epoch: `curl -s -X POST https://rpc.solenchain.io -d '{"jsonrpc":"2.0","id":1,"method":"solen_chainStatus","params":[]}' | grep -oE '"height":[0-9]+'` → `epoch = height/100`.
- **Emergency threshold** — currently 66.67% (same as normal). Since the fast-track
  skips the deliberation window, consider requiring a *higher* bar. If so, edit the
  emergency branch in `governance.rs::finalize` to use a higher constant than
  `PASS_THRESHOLD_BPS` for emergencies (one-line change), and re-run tests, BEFORE
  building.

## 1. Set the gate + build
```bash
cd ~/solen
# set the activation epoch (replace <ACT>):
sed -i 's/pub const EMERGENCY_FASTTRACK_ACTIVATION_EPOCH: u64 = u64::MAX;/pub const EMERGENCY_FASTTRACK_ACTIVATION_EPOCH: u64 = <ACT>;/' \
  crates/solen-system-contracts/src/governance.rs
grep EMERGENCY_FASTTRACK_ACTIVATION_EPOCH crates/solen-system-contracts/src/governance.rs   # verify

cargo test -p solen-system-contracts governance     # all green (incl. fast-track tests)
cargo build --release -p solen-node
sha256sum target/release/solen-node                 # = NEW_SHA
git add -A && git commit -m "governance: activate emergency fast-track at epoch <ACT>"   # you commit
```

## 2. Roll canary-first to all 15 — BEFORE epoch `ACT`
Use the exact procedure in `rollout-stateroot-fixes-2026-07.md` — it's the proven
playbook. In short:
1. **Stage** NEW_SHA to all 15 as `solen-node.new-fasttrack`; verify sha on each.
2. **Canary** one mid-fleet validator → verify gate (finalizing with quorum; same
   state_root as old-binary peers at a settled height; no mismatch/partition).
3. **Batched pairs** (≥9 up), validator1 last, then rpc2/3/4, rpc1 last. Verify gate
   after each. Keep a rollback copy (`solen-node.old-pre-fasttrack`) on every node.
4. **Confirm all 15 on NEW_SHA with time to spare before epoch `ACT`.** If any node
   isn't upgraded by `ACT`, it will fork when an emergency proposal fast-tracks —
   so if you can't finish in time, roll back and pick a later `ACT`.

Because the feature is dormant until `ACT`, this roll is behaviourally a no-op —
old and new binaries agree on every proposal (all still slow) right up to `ACT`.
That makes the mixed-binary window safe, exactly like the fork-choice-v2 arming.

## 3. Activation (automatic, no restart)
At epoch `ACT` the fast-track switches on fleet-wide with no action needed (it's a
pure code gate on `epoch >= ACT`). Verify after: create a throwaway `EmergencyResume`
(no-op if not paused) or, on devnet, test that an emergency proposal finalizes +
executes immediately on reaching quorum. Watch for NO fork (all nodes one root).

## 4. Using it in a real emergency (post-activation)
```bash
# propose the pause (any governance key)
solen --network mainnet propose ... EmergencyPause "reason"      # returns id N
# vote enough stake to clear 30% quorum + 66.67% yes
solen --network mainnet vote <yourkey> N --yes --weight <stake>
# the MOMENT quorum+threshold are met — no waiting:
solen --network mainnet finalize-proposal <yourkey> N            # -> Passed
solen --network mainnet execute-proposal  <yourkey> N            # -> is_paused=true, immediately
# resume the same way with EmergencyResume once safe.
```
(Always `--network mainnet` so signing uses chain_id 1 — a missing flag signs with
devnet's 1337 and fails "signature verification failed".)

## Rollback
Before `ACT`: revert nodes to `solen-node.old-pre-fasttrack` (or just rebuild with
the const back to `u64::MAX`) and roll — dormant either way. After `ACT`, the gate
is live; to disable, ship a build with the const back to `u64::MAX` (or a far-future
epoch) via the same canary roll.
