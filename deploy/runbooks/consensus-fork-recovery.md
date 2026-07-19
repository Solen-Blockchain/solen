# Runbook: consensus fork / state-root divergence recovery

**Symptom:** `[Solen fleet] 🔴 FLEET STALLED — tip stuck at <H>` that does not clear,
and/or the newer `🔴 <node> STATE-ROOT DIVERGENCE …` / `🔴 <node> CONSENSUS HALTED …`
alerts. The chain stops producing blocks.

This runbook is written for **mainnet**. Data dir: `/opt/solen/data/mainnet`,
localhost RPC `127.0.0.1:9944`, P2P `30333`, systemd unit `solen-node`, SSH `root@validatorN`
(N=1..11) and `root@rpcN` (N=1..4). Adjust paths for testnet (`…/data/testnet`, P2P `40333`).

First seen: 2026-07-16, block 1028501 (see "Reference incident" below).

---

## 0. Confirm and characterize (read-only)

Poll every node's height + state root. A divergence looks like **one (or a minority)
node on a different height/state root than the rest**:

```bash
for i in $(seq 1 11); do
  ( R=$(ssh -o BatchMode=yes -o ConnectTimeout=10 root@validator$i \
      "curl -s --max-time 8 -X POST 127.0.0.1:9944 -H 'content-type: application/json' \
       -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"solen_chainStatus\",\"params\":[]}'")
    H=$(echo "$R"  | grep -oE '\"height\":[0-9]+' | head -1 | cut -d: -f2)
    SR=$(echo "$R" | grep -oE '\"state_root\":\"[0-9a-f]{12}' | head -1 | cut -d'\"' -f4)
    echo "validator$i h=$H root=$SR" ) &
done; wait
```

Then read the transition on a **stuck (majority)** node — look for the decisive line:

```bash
ssh root@validator2 "journalctl -u solen-node --since '90 min ago' --no-pager \
  | grep -E 'state root mismatch|manual intervention|different fork' | tail"
# → 'state root mismatch on finalization — reverted … height=<H> proposer=<P> theirs=<R>'
```

**Decide which side is canonical:** the side held by the **≥2/3 stake supermajority**
(normally the larger group). A block is only truly final with matching-state-root
attestations from 2/3+; a lone/minority node at a higher height did **not** have real
quorum and is the fork. Confirm all binaries match (`sha256sum /opt/solen/bin/solen-node`)
to rule out a version-skew cause.

## 1. Preserve evidence (on the diverging node, before you touch it)

```bash
ssh root@<forked-node> '
  mkdir -p /root/fork-<H>-evidence && cd /root/fork-<H>-evidence
  for m in chainStatus "getBlock\",\"params\":[<H>]"; do :; done   # or just:
  curl -s -X POST 127.0.0.1:9944 -H "content-type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"solen_getBlock\",\"params\":[<H>]}" > block-<H>.json
  journalctl -u solen-node --since "<T-5min>" --no-pager > consensus-fork.log
'
```

## 2. Isolate the fork source

Stop the diverging (minority) node so it stops broadcasting the poison block:

```bash
ssh root@<forked-node> "systemctl stop solen-node; systemctl is-active solen-node"  # → inactive
```

The fleet is now down to N−1 validators — still ≥2/3 of 11, enough to finalize.

## 3. Un-stick the honest majority

The majority latches into "manual intervention / sync disabled" and will **not**
self-heal until restarted. Rolling-restart them (batches keep it tidy; they aren't
producing anyway):

```bash
for i in 2 3 4 5 6; do ssh root@validator$i "systemctl restart solen-node" & done; wait
for i in 7 8 9 10 11; do ssh root@validator$i "systemctl restart solen-node" & done; wait
```

Wait ~60–90s for peers to reconnect and a round-change to elect a healthy proposer
for the stuck height, then confirm the tip advances (re-run the step-0 poll). You want
to see `block finalized with quorum height=<H+…>` climbing and all nodes on one root.

## 4. Resync the diverging node onto the canonical chain

Its local state is corrupt/divergent and must be discarded. **The validator key is in
the systemd `--validator-seed` flag, NOT the data dir**, so wiping the data dir is safe
(verify: `systemctl show solen-node -p ExecStart --value | grep -o validator-seed`).

```bash
ssh root@<forked-node> '
  systemctl stop solen-node
  rm -rf /opt/solen/data/mainnet          # <-- mainnet path; discards divergent state
  systemctl start solen-node              # empty data dir → auto snapshot-sync from bootstrap peers
'
```

Watch it pull a snapshot (`downloaded chunk … progress=…%`) then replay forward
(`requesting sync … our_height=… peer_height=…`) until it reaches the tip on the
**canonical** root and resumes attesting. No mismatch/fork lines should appear.

> ⚠️ `rm -rf` on a live mainnet data dir is irreversible and is gated by the Claude Code
> auto-mode guard — run it yourself / outside auto mode.

## 5. Verify full recovery

- All 11 `validatorN` + all 4 `rpcN` at the same tip with the **same state root**.
- Public endpoint agrees: `curl -s -X POST https://rpc.solenchain.io … solen_chainStatus`.
- Watch the **next epoch boundary** (height % 100 == 0) for recurrence — that is where
  the reference incident triggered.

---

## Reference incident — 2026-07-16, block 1028501

Root cause: **non-deterministic state execution at the epoch boundary**. At 1028500→1028501
(epoch 10284→10285, where checkpoint + epoch-seed/proposer-selection updates fire),
proposer **validator1** computed state root `6ddc66d0…`; the other 10 validators executed
the same block, computed `7b31bfb9…`-lineage, rejected it (`state root mismatch on
finalization`) and reverted. validator1 marched onto a private fork (1028501→1028502…);
the honest majority latched into "manual intervention" and halted instead of rotating
past the bad proposer → full chain stall (~1h). All 11 binaries were byte-identical
(`1a42a9f14355`, v0.1.0), ruling out version skew.

Recovery: stopped validator1 → rolling-restarted validator2–11 (they elected a new
proposer and resumed at a fresh, valid 1028501) → wiped + snapshot-resynced validator1.
Evidence archived at `root@validator1:/root/fork-1028501-evidence/`.

### Open engineering follow-ups (prevent recurrence)
1. **Liveness:** auto round-change past a proposer whose block the ≥2/3 majority rejects,
   instead of the global "manual intervention" halt. Highest-leverage fix — chain
   self-heals even if determinism bugs recur.
2. **Determinism:** find + fix the non-determinism in epoch-boundary execution (audit
   `HashMap`/`HashSet` iteration into the state root, serialization order, time/RNG/float);
   add a full-block execution determinism test.
3. **Optimistic finalization:** validator1 logged `finalized with quorum` for a block no
   peer accepted — fix quorum accounting to require matching-state-root attestations.
4. **Monitoring:** `state root mismatch` + `manual intervention` alerts added to
   fleet_monitor.py (2026-07-16) — fire in seconds and name the culprit proposer.
