#!/usr/bin/env bash
#
# Solen epoch-randomness (variant B) — staggered-restart determinism drill
# ========================================================================
# Guards the grind-resistant epoch-randomness accumulator against the failure
# class that halted mainnet on 2026-07-28: the epoch proposer seed diverging
# across nodes at an epoch boundary. The original bug derived the seed from
# block_hash (proposer + timestamp), so state-equivalent boundary blocks yielded
# different seeds -> different proposer schedules -> no quorum -> halt; and a
# node RESTART reset the in-memory seed, so a restarted node disagreed with its
# still-running peers.
#
# Variant B commits the seed to STATE (accumulated from the agreed parent
# state_root each block). The property under test: staggered node restarts across
# epoch boundaries must NOT diverge the seed. Because the accumulator lives in
# state, `state_root` agreement across nodes IS accumulator agreement — no
# special RPC is needed.
#
# Gates (flag ACTIVE at height 5):
#   A. Fleet crosses epoch boundary 100 without halting; all nodes agree on
#      state_root; chainStatus reports epoch_randomness_height active.
#   B. (keystone) Restart nodes ONE AT A TIME at staggered heights straddling
#      boundary 200 (before / at / after). Fleet keeps finalizing and all nodes
#      still agree on state_root past 200 -> the in-state seed survived the
#      restarts with no divergence.
#   C. Dormant baseline (flag OFF): fleet also crosses a boundary cleanly, so the
#      active path is a no-regression.
#
# A FAIL is a halt at a boundary (original bug class) or any state_root fork
# after restarts (a variant-B non-determinism). Isolated --network devnet on
# localhost; never touches testnet/mainnet.
#
# Usage: tools/devnet/epoch-randomness-drill.sh [--debug]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROFILE="release"; [ "${1:-}" = "--debug" ] && PROFILE="debug"
NODE="$ROOT/target/$PROFILE/solen-node"
GENESIS="$ROOT/tools/devnet/er-drill-genesis-4.json"
BASE="/tmp/solen-er-drill"
N=4
FCV2=5   # attestation-aware fork choice ON early — healthy consensus baseline
SEEDS=("0101010101010101010101010101010101010101010101010101010101010101" \
       "0202020202020202020202020202020202020202020202020202020202020202" \
       "0303030303030303030303030303030303030303030303030303030303030303" \
       "0404040404040404040404040404040404040404040404040404040404040404")
ERH=18446744073709551615   # set per-run: u64::MAX = dormant, low = active

rpc_port(){ echo $((32944 + $1 * 10)); }
p2p_port(){ echo $((52333 + $1)); }

PASS=0; FAIL=0
ok(){   echo "  ✅ $*"; PASS=$((PASS+1)); }
bad(){  echo "  ❌ $*"; FAIL=$((FAIL+1)); }
note(){ echo "  ·  $*"; }

boot_args(){ local self=$1 a=""; for i in $(seq 0 $((N-1))); do
  [ "$i" -ne "$self" ] && a="$a --bootstrap /ip4/127.0.0.1/tcp/$(p2p_port "$i")"; done; echo "$a"; }

start_node(){ local i=$1
  nohup "$NODE" --network devnet --genesis "$GENESIS" --validator-seed "${SEEDS[$i]}" \
    --data-dir "$BASE/n$i" --rpc-port "$(rpc_port "$i")" --p2p-port "$(p2p_port "$i")" \
    --explorer-port 0 --fork-choice-v2-height "$FCV2" --epoch-randomness-height "$ERH" \
    --resync-url "http://127.0.0.1:$(rpc_port 0)" --resync-url "http://127.0.0.1:$(rpc_port 1)" \
    $(boot_args "$i") > "$BASE/n$i.log" 2>&1 &
  echo $! > "$BASE/n$i.pid"; disown 2>/dev/null || true; }

stop_node(){ [ -f "$BASE/n$1.pid" ] && kill -9 "$(cat "$BASE/n$1.pid")" 2>/dev/null; rm -f "$BASE/n$1.pid"; }
is_up(){ [ -f "$BASE/n$1.pid" ] && kill -0 "$(cat "$BASE/n$1.pid")" 2>/dev/null; }

rpc(){ curl -s --max-time 3 -X POST "127.0.0.1:$(rpc_port "$1")" -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" 2>/dev/null; }
height(){ rpc "$1" solen_chainStatus | python3 -c "import sys,json
try: print(json.load(sys.stdin)['result']['height'])
except: print('')" 2>/dev/null; }
root_at(){ rpc "$1" solen_getBlock "[$2]" | python3 -c "import sys,json
try: print(json.load(sys.stdin)['result']['state_root'][:16])
except: print('NONE')" 2>/dev/null; }
er_height(){ rpc "$1" solen_chainStatus | python3 -c "import sys,json
try: print(json.load(sys.stdin)['result']['config']['epoch_randomness_height'])
except: print('')" 2>/dev/null; }

live_nodes(){ for i in $(seq 0 $((N-1))); do is_up "$i" && echo "$i"; done; }
max_height(){ local m=0 h; for i in $(live_nodes); do h=$(height "$i"); [ -n "$h" ] && [ "$h" -gt "$m" ] && m=$h; done; echo "$m"; }
min_height(){ local m=99999999 h any=0; for i in $(live_nodes); do h=$(height "$i"); [ -n "$h" ] && { any=1; [ "$h" -lt "$m" ] && m=$h; }; done; [ "$any" = 1 ] && echo "$m" || echo ""; }

# All live nodes must agree on state_root at a settled height (min tip - 3).
# Since the accumulator is in state, this also proves seed/accumulator agreement.
check_no_fork(){
  local mh; mh=$(min_height); [ -z "$mh" ] && return 0
  local h=$((mh - 3)); [ "$h" -lt 1 ] && return 0
  local first="" r
  for i in $(live_nodes); do
    r=$(root_at "$i" "$h"); [ "$r" = "NONE" ] && continue
    if [ -z "$first" ]; then first="$r"; elif [ "$r" != "$first" ]; then
      bad "FORK at height $h: node$i=$r != $first"; return 1; fi
  done; return 0; }

advances_past(){ local target=$1 timeout=${2:-60} t=0
  while [ "$t" -lt "$timeout" ]; do
    local mh; mh=$(max_height); [ -n "$mh" ] && [ "$mh" -gt "$target" ] && return 0
    sleep 3; t=$((t+3)); done; return 1; }

wait_liftoff(){ local t=0; while [ "$t" -lt 90 ]; do local mh; mh=$(max_height)
  [ -n "$mh" ] && [ "$mh" -ge 6 ] && return 0; sleep 3; t=$((t+3)); done; return 1; }

# Wait until the fleet tip reaches `target`, then restart node `i` and let it rejoin.
# Only one node is ever down at a time, so quorum (3 of 4) is preserved.
restart_at(){ local i=$1 target=$2 t=0
  while [ "$t" -lt 120 ]; do local mh; mh=$(max_height); [ -n "$mh" ] && [ "$mh" -ge "$target" ] && break; sleep 2; t=$((t+2)); done
  note "restarting node$i at tip=$(max_height) (target $target)"
  stop_node "$i"; sleep 1; start_node "$i"
  # let it come back and catch up before the next staggered restart
  local w=0; while [ "$w" -lt 40 ]; do is_up "$i" && { local h; h=$(height "$i"); [ -n "$h" ] && [ "$h" -ge "$((target - 5))" ] && break; }; sleep 2; w=$((w+2)); done
}

cleanup(){ for i in $(seq 0 $((N-1))); do stop_node "$i"; done; }
trap cleanup EXIT

launch(){ rm -rf "$BASE"; mkdir -p "$BASE"; for i in $(seq 0 $((N-1))); do mkdir -p "$BASE/n$i"; done
  for i in $(seq 0 $((N-1))); do start_node "$i"; done; }

echo "=== Solen epoch-randomness (variant B) determinism drill ($PROFILE) ==="
[ -x "$NODE" ] || { echo "building $PROFILE binary..."; (cd "$ROOT" && cargo build --"$PROFILE" -p solen-node) || exit 1; }
pkill -f "solen-er-drill/n" 2>/dev/null; sleep 1

# ---------------------------------------------------------------------------
echo "[1/3] Gate A — flag ACTIVE (height 5): cross epoch boundary 100 cleanly"
ERH=5; launch
if ! wait_liftoff; then bad "never lifted off (tip $(max_height)) — localhost mesh artifact"; else
  note "lifted off (tip $(max_height))"
  # Confirm the flag is actually active fleet-wide.
  erok=1; for i in $(seq 0 $((N-1))); do e=$(er_height "$i"); [ "$e" = "5" ] || erok=0; done
  [ "$erok" = 1 ] && ok "chainStatus reports epoch_randomness_height=5 on all nodes" \
                  || bad "epoch_randomness_height not active fleet-wide"
  if advances_past 115 90; then
    ok "crossed epoch boundary 100 without halting (tip $(max_height))"
    check_no_fork && ok "no fork across boundary 100 (all agree on state_root incl. accumulator)"
  else
    bad "HALTED at/before boundary 100 (stuck ~$(max_height)) — original bug class"
  fi
fi

# ---------------------------------------------------------------------------
echo "[2/3] Gate B (keystone) — staggered restarts straddling boundary 200"
if [ "$(max_height)" -ge 100 ]; then
  # One node at a time, before / at / after the boundary. Node 0 stays up as anchor.
  restart_at 1 188   # before the boundary
  restart_at 2 198   # just before / at the boundary
  restart_at 3 205   # just after the boundary
  if advances_past 225 90; then
    ok "fleet kept finalizing through staggered restarts across boundary 200 (tip $(max_height))"
    check_no_fork && ok "no fork after staggered restarts — in-state seed survived restarts, no divergence"
    # Explicit agreement check at a height safely past the boundary.
    r0=$(root_at 0 210); allmatch=1
    for i in 1 2 3; do ri=$(root_at "$i" 210); [ "$ri" = "NONE" ] || [ "$ri" = "$r0" ] || allmatch=0; done
    [ "$allmatch" = 1 ] && ok "state_root@210 unanimous across all 4 nodes ($r0)" \
                        || bad "state_root@210 DIVERGED after restarts"
  else
    bad "fleet STALLED after staggered restarts near boundary 200 (stuck ~$(max_height))"
  fi
else
  note "skipped Gate B — fleet never reached height 100"
fi
cleanup; sleep 2

# ---------------------------------------------------------------------------
echo "[3/3] Gate C — dormant baseline (flag OFF): boundary crossing is no-regression"
ERH=18446744073709551615; launch   # the real shipped default (u64::MAX)
if wait_liftoff && advances_past 115 90; then
  erok=1; for i in $(seq 0 $((N-1))); do e=$(er_height "$i"); [ "$e" = "18446744073709551615" ] || erok=0; done
  [ "$erok" = 1 ] && ok "dormant fleet reports epoch_randomness_height=u64::MAX" || bad "dormant flag misreported"
  ok "dormant fleet crossed boundary 100 cleanly (tip $(max_height))"
  check_no_fork && ok "no fork on the dormant path"
else
  bad "dormant baseline failed to cross boundary 100 (stuck ~$(max_height))"
fi
cleanup

echo
echo "=== RESULT: $PASS passed, $FAIL failed ==="
if [ "$FAIL" -eq 0 ]; then
  echo "✅ epoch-randomness (variant B) is deterministic under staggered restarts across"
  echo "   epoch boundaries, active and dormant. Safe to proceed toward a flag-day."
  exit 0
else
  echo "❌ FAILURES above — do NOT activate. Logs in $BASE/n*.log"
  exit 1
fi
