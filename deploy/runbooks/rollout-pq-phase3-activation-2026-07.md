# Rollout runbook: PQ Phase 3 — post-quantum auth activation flag-day

Activates post-quantum (ML-DSA-65 / Hybrid) account auth on Solen mainnet by
setting `--pq-auth-height H` fleet-wide. This is the CONSENSUS-AFFECTING flag-day
(Phase 3 of the PQ activation plan; memory project-solen-pq-activation). The PQ
verify code is ALREADY deployed dormant — this only sets the activation height.

- **H = 1_110_000** (epoch 11,100). At height ≥ H every node honors `MlDsa`/`Hybrid`
  auth; below H they return `None` for it (dormant), regardless of the flag.
- Reference: current height 1_066_777 @ 6s blocks on 2026-07-19 → H ≈ 2026-07-22
  (~72h). Pick the exact flag-day window once soak is done; H is fixed once
  restarts begin.

Fleet: `root@validator1..11` + `root@rpc1..4`. systemd unit `solen-node`, binary
`/opt/solen/bin/solen-node` (currently **ed42882ae4bf**, the canonicity-fix
binary), localhost RPC 9944. `--pq-auth-height` is a startup flag set in the
unit's `ExecStart` (same pattern as the existing `--fork-choice-v2-height 688050`).

## The one hard invariant
**Every one of the 15 nodes must carry the SAME H and reach it before the chain
height passes H.** A node still at u64::MAX (dormant) at height H rejects the
PQ-authorized ops the others accept → it forks off alone. Restart ORDER does not
matter: setting `--pq-auth-height H` is BEHAVIOR-NEUTRAL below H (`height >= H`
is false, identical to dormant), so the restart window carries zero fork risk —
the change only bites at H, by which time all 15 must be converted. The gate is
therefore: **100% of nodes report `chainStatus.config.pq_auth_height == H`,
verified and re-verified, well before H.**

## Why lower-risk than a normal flag-day
- No binary change; PQ verify code already fleet-wide and dormant.
- Restarting with `--pq-auth-height H` is a no-op below H → the restart itself
  cannot fork. Only the height-gated flip at H matters, and it is a deterministic
  integer comparison every node computes identically.
- The canonicity fix (ed42882ae4bf, deployed 2026-07-19) removes the restart
  resync-wedge that hit validator8 on the Phase-1 flag-day — restarts are safe.
- Non-disruptive: PQ is opt-in per account via SetAuth. 100% classical Ed25519
  accounts are untouched; zero forced migration.

---

## 0. Preconditions
- [ ] Canonicity fix (ed42882ae4bf) soaked ≥24–48h with zero `not confirmed canonical` loops fleet-wide.
- [ ] Full `cargo test --workspace` green on the deployed commit; PQ Phase-0 drill (`tools/devnet/pq-auth-drill.sh`) green.
- [ ] Fleet healthy: all 15 on ed42882ae4bf, one state_root, advancing; `pq_auth_height` currently `18446744073709551615` everywhere (Appendix C of the Phase-1 runbook).
- [ ] **Canary account ready:** a throwaway mainnet HD account funded with a few SOLEN (fees), imported into a solen-cli keystore, to exercise the hybrid upgrade + hybrid transfer AFTER H. (Also confirm the wallet has a spare recovery-phrase account for the UI check.)
- [ ] H (= 1_110_000) is comfortably in the future (≥ several hours of restart+verify margin). Do the §1 flag-day restart 24–48h before H, then monitor.

## 1. Set the flag on every node (well before H) — one at a time
Behavior-neutral below H, so this is a routine gated restart on the canonicity-fix
binary. Per node (see Appendix A for the exact idempotent edit):
```bash
ssh root@<node> '
  sed -i "s|--fork-choice-v2-height 688050|--fork-choice-v2-height 688050 --pq-auth-height 1110000|" /etc/systemd/system/solen-node.service
  grep -c -- "--pq-auth-height 1110000" /etc/systemd/system/solen-node.service   # MUST print 1 (idempotent)
  systemctl daemon-reload
  systemctl restart solen-node
'
```
**Verify gate after EACH node** (all must hold before the next):
1. `chainStatus.config.pq_auth_height == "1110000"` on the node (Appendix B).
2. Node is `active`, catches up, finalizes with quorum, NO `not confirmed canonical` loop.
3. State_root agrees with the fleet at a common settled height (no fork — expected, since below H this is a no-op).
Order: canary validator6 first (full watch), then validator2..11 one at a time,
validator1 last, then rpc2/3/4, rpc1 last. Same mechanics as
`rollout-canonicity-failopen-2026-07.md`.

## 2. Convergence gate (THE gate) — before H
Re-run the fleet `pq_auth_height` sweep (Appendix B) and confirm **all 15 report
`1110000`**. Any node showing `18446744073709551615` (or a different value) MUST
be fixed before H, or it forks at H. Re-check again a few hours later and once
more shortly before H — a node that crash-restarts picks up H from the unit, so
this should stay stable, but verify.

## 3. Activation at H
- [ ] Watch the chain cross **H = 1_110_000**. Block H is a normal (epoch) block
  carrying NO PQ ops — the capability just turns on. Confirm all 15 cross H on
  one state_root, still advancing (Appendix A of Phase-1 runbook).
- [ ] **Canary the PQ path** a few blocks AFTER H (not in block H), with the
  throwaway funded account (Appendix C):
  - `solen --network mainnet key quantum-upgrade <canary> --hybrid` → poll
    `solen_getAccount` until its `auth_methods` contains `Hybrid`.
  - `solen --network mainnet transfer <canary> <dest> <small>` → a HYBRID-signed
    op; confirm it lands (recipient balance rises).
  - Confirm all 15 validators AGREE on the state_root at that height (no fork).
- [ ] **Wallet end-to-end:** the wallet Security tab (against rpc.solenchain.io)
  now shows PQ "active" and ENABLES "Upgrade to quantum-safe (hybrid)". Optionally
  run one phrase-preserving upgrade from the wallet and confirm it lands.

## 4. Post-activation monitoring
- [ ] Zero `state root mismatch` / `needs manual` / `partition` over the following hours; watch the next epoch boundary after H cross cleanly.
- [ ] Announce PQ availability once stable. Update memory: PQ ACTIVE at H.

---

## Rollback
**Point of no return = the FIRST PQ-authorized op included after H.** Before that,
activation is fully reversible with no state divergence:
```bash
ssh root@<node> '
  sed -i "s| --pq-auth-height 1110000||" /etc/systemd/system/solen-node.service
  systemctl daemon-reload && systemctl restart solen-node'   # back to dormant (u64::MAX)
```
Revert all 15 (verify each reports `18446744073709551615`). Safe because nothing
used PQ yet. AFTER the first post-H PQ op is finalized, rollback would orphan it —
that op is the commit point; do NOT roll back past it.

If a node wedges on restart (should not — canonicity fix deployed): the clean
data-dir reset (stop → `mv /opt/solen/data/mainnet` aside → restart for fresh
snapshot resync; identity is the `--validator-seed` flag).

---

## Appendix A — exact idempotent unit edit
The `ExecStart` today ends with `... --fork-choice-v2-height 688050`. Append the
flag right after it (the `sed` is idempotent — running twice won't duplicate,
because the second run's pattern `--fork-choice-v2-height 688050` without the
already-appended suffix won't match a line that already has it; verify with the
`grep -c` = 1 check). To be safe, ALWAYS run the `grep -c -- "--pq-auth-height
1110000"` check = exactly 1 after editing, and `systemctl cat solen-node | grep
ExecStart` to eyeball the final command before restart.

## Appendix B — pq_auth_height sweep
```bash
for h in validator{1..11} rpc{1..4}; do
  V=$(ssh -o ConnectTimeout=8 root@$h "curl -s --max-time 12 -X POST 127.0.0.1:9944 -H 'content-type: application/json' -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"solen_chainStatus\",\"params\":[]}'" | grep -o '\"pq_auth_height\":\"[0-9]*\"')
  echo "$h $V"
done
# Before H, after §1: EVERY node must read "pq_auth_height":"1110000".
```

## Appendix C — canary account (solen-cli, isolated keystore)
1 SOLEN = 100_000_000 base units. Fees are ~0.01 SOLEN (`max_fee` 1_000_000 base,
`base_fee_per_gas` 1), so ~5 SOLEN covers the SetAuth + a hybrid transfer + the
1-SOLEN test amount with wide margin. `--network mainnet` targets
`https://rpc.solenchain.io`, chain_id 1.

### C.1 — prep + fund (do any time before H; §1 should be converged first)
```bash
cargo build --release -p solen-cli          # ensure the CLI has `quantum-upgrade`
SOLEN=~/solen/target/release/solen
export CANARY_HOME=/tmp/pq-canary            # isolated keystore — never touches ~/.solen

# 1) Generate a fresh THROWAWAY seed yourself (never share/reuse; small funds only):
SEED=$(openssl rand -hex 32); echo "canary seed (save until done): $SEED"

# 2) Import + read the address:
HOME=$CANARY_HOME $SOLEN --network mainnet key import canary "$SEED"
HOME=$CANARY_HOME $SOLEN --network mainnet account canary     # note the ID (base58) = CANARY_ADDR

# 3) Fund ~5 SOLEN to CANARY_ADDR — from the wallet (send 5 SOLEN to CANARY_ADDR),
#    or via CLI from a funded key:
#    $SOLEN --network mainnet transfer <your-funded-key> <CANARY_ADDR> 5

# 4) Verify:
HOME=$CANARY_HOME $SOLEN --network mainnet balance canary     # ~5 SOLEN
```

### C.2 — exercise the PQ path (AFTER H, a few blocks past 1_110_000)
```bash
HOME=$CANARY_HOME $SOLEN --network mainnet key quantum-upgrade canary --hybrid
HOME=$CANARY_HOME $SOLEN --network mainnet account canary      # auth_methods should show Hybrid
# hybrid-signed transfer (send 1 SOLEN back to your funding address as <dest>):
HOME=$CANARY_HOME $SOLEN --network mainnet transfer canary <dest> 1   # confirm it lands
```
Note: the cli's `quantum-upgrade` mints a NEW hybrid seed (account_id preserved,
auth ed-key differs from id). The WALLET path is phrase-preserving (auth ed-key
== id). Both are valid on-chain; the node only checks signatures verify.

### C.3 — wallet end-to-end (optional, at H)
Have a spare **recovery-phrase** wallet account with a small balance (~1 SOLEN for
fees). After H the Security tab shows PQ "active" and enables "Upgrade to
quantum-safe (hybrid)"; run it once and confirm the SetAuth lands (address +
phrase unchanged).
