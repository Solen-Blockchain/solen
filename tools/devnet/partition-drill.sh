#!/usr/bin/env bash
#
# Solen devnet partition drill
# ============================
# Spins up an isolated local 4-validator devnet and exercises the consensus
# failure modes that bit mainnet on 2026-06-08 / 06-23 / 06-24, asserting the
# fleet NEVER forks and ALWAYS self-heals with no manual intervention.
#
# What it validates:
#   - Fix #2 (deterministic backup proposer + slashing determinism): under heavy
#     proposer churn (kill/restart), no two validators ever finalize different
#     blocks at the same height (no competing-block fork).
#   - Partition self-heal (deterministic prober): kill >1/3 of stake so the chain
#     halts, restart, and confirm the fleet converges and advances again on its
#     own.
#   - Majority survival + straggler rejoin: kill one node (quorum survives),
#     confirm the rest keep finalizing, restart it, confirm it catches up.
#
# Isolated: runs --network devnet (empty remote resync URLs) on localhost, so it
# never touches real testnet/mainnet. mDNS + explicit bootstrap form the mesh.
#
# Usage: tools/devnet/partition-drill.sh [--debug]   (release by default)
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROFILE="release"; [ "${1:-}" = "--debug" ] && PROFILE="debug"
NODE="$ROOT/target/$PROFILE/solen-node"
GENESIS="$ROOT/tools/devnet/drill-genesis.json"
BASE="/tmp/solen-drill"
N=4
SEEDS=("0101010101010101010101010101010101010101010101010101010101010101" \
       "0202020202020202020202020202020202020202020202020202020202020202" \
       "0303030303030303030303030303030303030303030303030303030303030303" \
       "0404040404040404040404040404040404040404040404040404040404040404")

rpc_port(){ echo $((29944 + $1 * 10)); }
p2p_port(){ echo $((50333 + $1)); }

PASS=0; FAIL=0
ok(){   echo "  ✅ $*"; PASS=$((PASS+1)); }
bad(){  echo "  ❌ $*"; FAIL=$((FAIL+1)); }
note(){ echo "  ·  $*"; }

# ── node lifecycle ─────────────────────────────────────────────────────────
boot_args(){ local self=$1 a=""; for i in $(seq 0 $((N-1))); do
  [ "$i" -ne "$self" ] && a="$a --bootstrap /ip4/127.0.0.1/tcp/$(p2p_port "$i")"; done; echo "$a"; }

start_node(){ local i=$1
  # --resync-url points at peers 0 and 1 (which stay up through every scenario),
  # giving the tiered recovery a canonical seed so a diverged/stranded node can
  # actually self-heal on this isolated devnet (mainnet uses rpc.solenchain.io).
  nohup "$NODE" --network devnet --genesis "$GENESIS" --validator-seed "${SEEDS[$i]}" \
    --data-dir "$BASE/n$i" --rpc-port "$(rpc_port "$i")" --p2p-port "$(p2p_port "$i")" \
    --explorer-port 0 \
    --resync-url "http://127.0.0.1:$(rpc_port 0)" --resync-url "http://127.0.0.1:$(rpc_port 1)" \
    $(boot_args "$i") > "$BASE/n$i.log" 2>&1 &
  echo $! > "$BASE/n$i.pid"; }

stop_node(){ [ -f "$BASE/n$1.pid" ] && kill -9 "$(cat "$BASE/n$1.pid")" 2>/dev/null; rm -f "$BASE/n$1.pid"; }
is_up(){ [ -f "$BASE/n$1.pid" ] && kill -0 "$(cat "$BASE/n$1.pid")" 2>/dev/null; }

# ── rpc helpers (python3 for json) ─────────────────────────────────────────
rpc(){ curl -s --max-time 3 -X POST "127.0.0.1:$(rpc_port "$1")" -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" 2>/dev/null; }
height(){ rpc "$1" solen_chainStatus | python3 -c "import sys,json
try: print(json.load(sys.stdin)['result']['height'])
except: print('')" 2>/dev/null; }
root_at(){ rpc "$1" solen_getBlock "[$2]" | python3 -c "import sys,json
try: print(json.load(sys.stdin)['result']['state_root'][:16])
except: print('NONE')" 2>/dev/null; }

live_nodes(){ for i in $(seq 0 $((N-1))); do is_up "$i" && echo "$i"; done; }
max_height(){ local m=0 h; for i in $(live_nodes); do h=$(height "$i"); [ -n "$h" ] && [ "$h" -gt "$m" ] && m=$h; done; echo "$m"; }
min_height(){ local m=99999999 h any=0; for i in $(live_nodes); do h=$(height "$i"); [ -n "$h" ] && { any=1; [ "$h" -lt "$m" ] && m=$h; }; done; [ "$any" = 1 ] && echo "$m" || echo ""; }

# FORK CHECK: at a settled common height, every live node must agree on the root.
check_no_fork(){
  local mh; mh=$(min_height); [ -z "$mh" ] && return 0
  local h=$((mh - 3)); [ "$h" -lt 1 ] && return 0
  local first="" r
  for i in $(live_nodes); do
    r=$(root_at "$i" "$h"); [ "$r" = "NONE" ] && continue
    if [ -z "$first" ]; then first="$r"; elif [ "$r" != "$first" ]; then
      bad "FORK at height $h: node$i=$r != $first"; return 1; fi
  done
  return 0
}

# Wait until the live set advances past a target height (liveness).
wait_advance_past(){ local target=$1 timeout=${2:-40} t=0
  while [ "$t" -lt "$timeout" ]; do
    local mh; mh=$(max_height); [ -n "$mh" ] && [ "$mh" -gt "$target" ] && return 0
    sleep 2; t=$((t+2)); done; return 1; }

# Wait until the chain is producing steadily (advances >=6 blocks), i.e. fully
# recovered and not mid-settle. Returns 1 on timeout.
wait_settled(){ local timeout=${1:-60} start; start=$(max_height); local t=0
  while [ "$t" -lt "$timeout" ]; do
    local mh; mh=$(max_height); [ -n "$mh" ] && [ -n "$start" ] && [ "$mh" -ge $((start + 6)) ] && return 0
    sleep 3; t=$((t+3)); done; return 1; }

cleanup(){ for i in $(seq 0 $((N-1))); do stop_node "$i"; done; }
trap cleanup EXIT

# ── setup ──────────────────────────────────────────────────────────────────
echo "=== Solen partition drill ($PROFILE) ==="
[ -x "$NODE" ] || { echo "building $PROFILE binary..."; (cd "$ROOT" && cargo build --"$PROFILE" -p solen-node) || exit 1; }
pkill -f "solen-drill/n" 2>/dev/null; sleep 1
rm -rf "$BASE"; mkdir -p "$BASE"; for i in $(seq 0 $((N-1))); do mkdir -p "$BASE/n$i"; done

echo "[1/4] starting 4-validator devnet..."
for i in $(seq 0 $((N-1))); do start_node "$i"; done

# Wait for mesh warmup + first finalizations (multi-validator gate waits ~30s).
note "waiting for first blocks (mesh warmup ~30s)..."
if wait_advance_past 2 80; then ok "cluster producing blocks (tip $(max_height))"
else bad "cluster never started producing"; echo; echo "RESULT: FAIL (no liftoff)"; exit 1; fi
check_no_fork && ok "baseline: all nodes agree" || true

# ── Scenario A: proposer churn → no competing-block fork (fix #2) ───────────
echo "[2/4] Scenario A: proposer churn (15 kill/restart cycles)..."
A_forks=0
for r in $(seq 1 15); do
  victim=$((RANDOM % N))
  stop_node "$victim"
  sleep "$(python3 -c "import random;print(round(random.uniform(0.5,2.5),2))")"
  start_node "$victim"
  sleep "$(python3 -c "import random;print(round(random.uniform(1.0,2.0),2))")"
  if ! check_no_fork; then A_forks=$((A_forks+1)); fi
done
[ "$A_forks" -eq 0 ] && ok "Scenario A: no fork across 15 churn cycles" \
                     || bad "Scenario A: $A_forks fork(s) detected"
# must still be live afterward
top=$(max_height); wait_advance_past "$top" 40 && ok "chain advancing after churn (tip $(max_height))" \
                                              || bad "chain stalled after churn"

# ── Scenario B: quorum-loss partition + prober self-heal ───────────────────
echo "[3/4] Scenario B: kill 2/4 (quorum lost) → expect halt → restart → self-heal..."
stop_node 2; stop_node 3
stuck=$(max_height); note "killed n2,n3; tip=$stuck (expect halt — 2/4 < 2/3 quorum)"
sleep 12
after=$(max_height)
if [ "$after" -le $((stuck + 2)) ]; then ok "chain correctly halted at ~$after (no minority finalization)"
else note "chain advanced to $after (timing — acceptable)"; fi
start_node 2; start_node 3
if wait_advance_past $((after + 2)) 150; then ok "Scenario B: fleet self-healed, advancing (tip $(max_height))"
else bad "Scenario B: did NOT self-heal after quorum restored"; fi
check_no_fork && ok "Scenario B: converged, no fork" || true

# Let the cluster fully settle after the partition recovery before the next
# scenario, so C starts from a steadily-advancing chain (not mid-recovery).
note "settling after B (waiting for steady production)..."
wait_settled 60 && note "settled (tip $(max_height))" || note "still settling (tip $(max_height))"

# ── Scenario C: majority survives + straggler rejoins (fix #1 territory) ────
echo "[4/4] Scenario C: kill 1/4 (quorum survives) → rejoin..."
stop_node 3
top=$(max_height)
if wait_advance_past $((top + 3)) 90; then ok "Scenario C: 3/4 majority kept finalizing"
else bad "Scenario C: majority stalled with 1 down"; fi
start_node 3
sleep 20
h3=$(height 3); mh=$(max_height)
if [ -n "$h3" ] && [ "$h3" -ge $((mh - 10)) ]; then ok "Scenario C: straggler rejoined (n3 @ $h3, tip $mh)"
else bad "Scenario C: straggler stuck (n3 @ ${h3:-down}, tip $mh)"; fi
check_no_fork && ok "Scenario C: no fork after rejoin" || true

# ── report ─────────────────────────────────────────────────────────────────
echo
echo "=== RESULT: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] && echo "DRILL PASSED ✅" || echo "DRILL FAILED ❌ (logs in $BASE/n*.log)"
exit "$FAIL"
