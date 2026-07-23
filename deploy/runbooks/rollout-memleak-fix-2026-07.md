# Rollout runbook: memory-leak fix (Kad Client mode + indexer caps) — 2026-07

Canary-first, verify-gated rollout of the fix for the post-2026-07-17 anonymous-
heap leak (fleet mem-watchdog restarts, esp. validator6; root cause + dhat profile
in memory project-solen-memory-leak). Ships on top of the canonicity-fix binary.
**Nothing activates** — PQ stays dormant (no `--pq-auth-height`).

## Deploy point
- **DEPLOY_COMMIT** = `fa3e82a` (`main`). Binary delta over the deployed canonicity
  binary = this one code commit (+ `3f7a203` dhat instrumentation, which is
  feature-gated OFF → no default-build impact).
- **NEW_SHA** = `7db2c35ab167d835c2371f8c73e7bfbf7d7430415d735bed905d8b5546011a1f` (short `7db2c35ab167`). Built 2026-07-20.
- **OLD_SHA** = `ed42882ae4bfdc0f397035638579d969812e490932b155a6f520f6704b6baeae` (short `ed42882ae4bf`, the canonicity-fix binary). Confirm fleet-wide in §0.

Distribute this exact `target/release/solen-node`; per-node `sha256sum` verifies.

## What's shipping / why safe incrementally (NO flag-day)
- **Kademlia `Mode::Server` → `Mode::Client`** — stops serving inbound DHT query
  substreams (whose ~497KB buffers leaked ~226MB). Peer connectivity is unchanged:
  every peer is in the explicit bootstrap list (+ mdns + identify); Solen uses no
  DHT records. 3-node devnet verified: connects + finalizes in Client mode.
- **Indexer `blocks`/`events` in-memory caps** (20k / 200k, drain-oldest) — bounds
  the ~65MB indexer accumulation; both are scan-queried, so eviction is safe; full
  history stays in the chain/RocksDB.
- **Networking-discovery + indexer only** — no change to block execution, state
  roots, or block validity, so old/new interoperate with no fork. Roll node-by-node.
- The canonicity fix (already deployed) makes restarts wedge-safe.
- Pre-roll: `cargo test --workspace` green; p2p/indexer + indexer-cap tests pass.

---

## 0. Preconditions
- [ ] Full `cargo test --workspace` green on `fa3e82a`.
- [ ] `ssh root@validator1 "sha256sum /opt/solen/bin/solen-node"` = OLD_SHA (`ed42882ae4bf…`), matches fleet-wide.
- [ ] Fleet healthy: all 15 on one state_root, advancing.
- [ ] Note each node's current RSS (baseline) for the post-deploy slope check: `ssh root@<h> "ps -o rss= -C solen-node | awk '{s+=\$1} END{print int(s/1024)\"MB\"}'"`.
- [ ] Low-traffic window; avoid starting within ~10 blocks of an epoch boundary.

## 1. Stage the new binary INACTIVE on every node
```bash
cd ~/solen
for h in validator{1..11} rpc{1..4}; do
  scp target/release/solen-node root@$h:/opt/solen/bin/solen-node.memfix-2026-07
  ssh root@$h "sha256sum /opt/solen/bin/solen-node.memfix-2026-07"   # MUST equal 7db2c35ab167…
done
```
Abort if any sha differs. No systemd unit change.

## 2. Canary + LEAK-GONE verification (rpc4 — already has a before-profile)
Swap rpc4 (non-validator → zero quorum risk):
```bash
ssh root@rpc4 '
  systemctl stop solen-node
  cp /opt/solen/bin/solen-node /opt/solen/bin/solen-node.pre-memfix
  cp /opt/solen/bin/solen-node.memfix-2026-07 /opt/solen/bin/solen-node
  systemctl start solen-node'
```
**Verify gate:**
- Node active, catches up, finalizes/serves RPC, agrees on state_root, NO `not confirmed canonical` loop.
- **Leak-gone check (the point of this roll)** — pick ONE:
  - **(preferred, definitive, ~30min)** dhat re-profile: stage the `--features dhat-heap` build of DEPLOY_COMMIT, run rpc4 manually under it ~30min (see project-solen-memory-leak / the earlier run), SIGINT, and confirm `libp2p_kad::...upgrade_inbound` bytes-at-exit is ~0 (was ~226MB). Then restore the normal binary.
  - **(operational, ~2–3h)** RSS slope: watch rpc4 RSS over 2–3h — it should stay roughly FLAT (was climbing ~450MB/h). Compare to a still-old node's continued climb.
- If the leak is NOT gone → rollback rpc4 (§7) and investigate (may need Kad fully disabled rather than Client mode).

## 3. Fleet rollout — one node at a time
After the canary confirms leak-gone + no regression. Roll one at a time (mixed-
safe, so pace is for cleanliness, not safety): validator2..11 → validator1 last →
rpc2/3/1 (rpc4 already done). Per node:
```bash
ssh root@<node> '
  systemctl stop solen-node
  cp /opt/solen/bin/solen-node /opt/solen/bin/solen-node.pre-memfix
  cp /opt/solen/bin/solen-node.memfix-2026-07 /opt/solen/bin/solen-node
  systemctl start solen-node'
```
**Verify gate after each:** on NEW_SHA, active + finalizing with quorum, state_root
agrees with the fleet, no checkpoint loop. (Same mechanics as the canonicity roll.)

## 4. Post-deploy
- [ ] All 15 on NEW_SHA, one state_root, advancing.
- [ ] **RSS stays bounded** — over the next 24–48h, fleet RSS should plateau (no
  climb toward the 8.5GB watchdog line), and the watchdog should stop auto-restarting
  nodes. Watch validator6 especially (was the fastest leaker). **NOTE (2026-07-23):
  the code fix alone did NOT plateau — residual glibc arena retention kept the crawl
  going; see §5 for the jemalloc supplement that actually flattened the fleet.**
- [ ] Once RSS is confirmed flat for ~48h, **relax the mem watchdog**: on rpc1
  `/opt/solen-monitor/config.json`, restore `mem_rss_mb_warn` (was bumped 7000→8000
  as interim relief) and consider raising `mem_rss_mb_restart` / setting
  `mem_auto_restart:false` — the stopgap is no longer needed. Keep a modest warn as
  a safety net.
- [ ] Clean up after ~24h stable: remove `solen-node.memfix-2026-07`; keep `solen-node.pre-memfix` until confident.

## 5. jemalloc supplement — the clincher (deployed 2026-07-23)
The code fix (Kad Client + indexer caps) removed the two *Rust* leaks but the fleet
still crept to 2–6.5GB and tripped the watchdog. A local devnet repro under dhat
proved the residual was NOT a Rust code path: +285MB RSS vs only ~14.6MB live in the
Rust allocator. Root cause = **glibc `malloc` arena retention** (freed RocksDB/C++
allocations held in per-thread arenas, never returned to the OS) under bursty
serve/resync — anonymous heap, invisible to dhat (which only hooks the Rust global
allocator). Fix = swap the allocator to **jemalloc**, which releases freed pages
back to the OS.

**Mechanism — LD_PRELOAD via a systemd drop-in (config-only, no rebuild, mixed-safe):**
```bash
for h in validator{1..11} rpc{1..4}; do
  ssh root@$h '
    apt-get install -y libjemalloc2
    install -d /etc/systemd/system/solen-node.service.d
    printf "[Service]\nEnvironment=LD_PRELOAD=/usr/lib/x86_64-linux-gnu/libjemalloc.so.2\n" \
      > /etc/systemd/system/solen-node.service.d/jemalloc.conf
    systemctl daemon-reload'
  ssh root@$h 'systemctl restart solen-node'    # STANDALONE restart — do not fold into the compound cmd
done
```
Roll one node at a time. The restart also clears that node's accumulated arena bloat.

**Verify per node:** `PID=$(pgrep -x solen-node); grep -c jemalloc /proc/$PID/maps`
must be non-zero (expect `5`), node active + finalizing with quorum, state_root
agrees with the fleet.

**Result (2026-07-23):** all 15 loaded (`jemalloc=5`), unanimous root, fleet RSS
dropped from the 2–6.5GB spread to **~0.6–1.2GB** and stayed flat. v3 pre-roll test:
45min dead-flat ~1.05GB while glibc peers sat pinned at 4.5/5.9GB — proves it's the
allocator, not the restart. Memory saga resolved.

**Rollback:** `rm /etc/systemd/system/solen-node.service.d/jemalloc.conf && systemctl daemon-reload && systemctl restart solen-node` (reverts to glibc; behaviour otherwise identical).

---

## 7. Rollback
Single node: `cp /opt/solen/bin/solen-node.pre-memfix /opt/solen/bin/solen-node && systemctl restart solen-node`.
Full: revert every node to `solen-node.pre-memfix` (safe — networking/indexer only, no root change).
If Client mode ever proves insufficient (leak persists in the dhat re-profile),
the escalation is to disable the Kademlia behaviour entirely (bigger change, needs
its own test that the bootstrap-list mesh stays connected).
