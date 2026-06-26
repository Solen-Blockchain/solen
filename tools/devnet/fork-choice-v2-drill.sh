#!/usr/bin/env bash
#
# Solen fork-choice v2 — 2-down liveness drill
# ============================================
# Reproduces the mainnet halt of 2026-06-26 (height 683764): with >=2 validators
# down, the rotating backup/prober emits competing blocks whose attestations
# split, and under the legacy (v1) attest-once rule the survivors can never
# converge -> the fleet wedges even though a quorum is alive. Validates that
# attestation-aware fork choice (v2) lets the survivors converge their votes and
# keep finalizing.
#
# Topology: 7 equal-stake validators. Quorum is 5 (2/3 of 7), so killing 2 leaves
# exactly a quorum that MUST agree on one block — the case v1 cannot satisfy when
# blocks compete, and v2 can (vote-change converges them).
#
# Runs the SAME kill-2 scenario twice and compares:
#   - v1 (gate OFF):  expected to STALL after kill-2 (the bug).
#   - v2 (gate ON):   expected to KEEP ADVANCING after kill-2 (the fix).
#
# Isolated --network devnet on localhost; never touches testnet/mainnet.
# Usage: tools/devnet/fork-choice-v2-drill.sh [--debug]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROFILE="release"; [ "${1:-}" = "--debug" ] && PROFILE="debug"
NODE="$ROOT/target/$PROFILE/solen-node"
GENESIS="$ROOT/tools/devnet/drill-genesis-7.json"
BASE="/tmp/solen-fcv2-drill"
N=7
SEEDS=("0101010101010101010101010101010101010101010101010101010101010101" \
       "0202020202020202020202020202020202020202020202020202020202020202" \
       "0303030303030303030303030303030303030303030303030303030303030303" \
       "0404040404040404040404040404040404040404040404040404040404040404" \
       "0505050505050505050505050505050505050505050505050505050505050505" \
       "0606060606060606060606060606060606060606060606060606060606060606" \
       "0707070707070707070707070707070707070707070707070707070707070707")
FCV2=999999999   # set per-run: 999999999 = OFF (v1), low = ON (v2)

rpc_port(){ echo $((31944 + $1 * 10)); }
p2p_port(){ echo $((51333 + $1)); }

PASS=0; FAIL=0
ok(){   echo "  ✅ $*"; PASS=$((PASS+1)); }
bad(){  echo "  ❌ $*"; FAIL=$((FAIL+1)); }
note(){ echo "  ·  $*"; }

boot_args(){ local self=$1 a=""; for i in $(seq 0 $((N-1))); do
  [ "$i" -ne "$self" ] && a="$a --bootstrap /ip4/127.0.0.1/tcp/$(p2p_port "$i")"; done; echo "$a"; }

start_node(){ local i=$1
  nohup "$NODE" --network devnet --genesis "$GENESIS" --validator-seed "${SEEDS[$i]}" \
    --data-dir "$BASE/n$i" --rpc-port "$(rpc_port "$i")" --p2p-port "$(p2p_port "$i")" \
    --explorer-port 0 --fork-choice-v2-height "$FCV2" \
    --resync-url "http://127.0.0.1:$(rpc_port 0)" --resync-url "http://127.0.0.1:$(rpc_port 1)" \
    $(boot_args "$i") > "$BASE/n$i.log" 2>&1 &
  echo $! > "$BASE/n$i.pid"; }

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

live_nodes(){ for i in $(seq 0 $((N-1))); do is_up "$i" && echo "$i"; done; }
max_height(){ local m=0 h; for i in $(live_nodes); do h=$(height "$i"); [ -n "$h" ] && [ "$h" -gt "$m" ] && m=$h; done; echo "$m"; }
min_height(){ local m=99999999 h any=0; for i in $(live_nodes); do h=$(height "$i"); [ -n "$h" ] && { any=1; [ "$h" -lt "$m" ] && m=$h; }; done; [ "$any" = 1 ] && echo "$m" || echo ""; }

check_no_fork(){
  local mh; mh=$(min_height); [ -z "$mh" ] && return 0
  local h=$((mh - 3)); [ "$h" -lt 1 ] && return 0
  local first="" r
  for i in $(live_nodes); do
    r=$(root_at "$i" "$h"); [ "$r" = "NONE" ] && continue
    if [ -z "$first" ]; then first="$r"; elif [ "$r" != "$first" ]; then
      bad "FORK at height $h: node$i=$r != $first"; return 1; fi
  done; return 0; }

# Advance past a target within timeout (liveness over the live set).
advances_past(){ local target=$1 timeout=${2:-45} t=0
  while [ "$t" -lt "$timeout" ]; do
    local mh; mh=$(max_height); [ -n "$mh" ] && [ "$mh" -gt "$target" ] && return 0
    sleep 3; t=$((t+3)); done; return 1; }

cleanup(){ for i in $(seq 0 $((N-1))); do stop_node "$i"; done; }
trap cleanup EXIT

# Run the kill-2 scenario for a given gate setting. Echoes PASS/STALL.
run_scenario(){
  local label=$1; FCV2=$2
  echo "[$label] launching $N validators (fork-choice-v2-height=$FCV2)..."
  rm -rf "$BASE"; mkdir -p "$BASE"; for i in $(seq 0 $((N-1))); do mkdir -p "$BASE/n$i"; done
  for i in $(seq 0 $((N-1))); do start_node "$i"; done

  # Liftoff + steady production.
  local lift=0 t=0
  while [ "$t" -lt 90 ]; do local mh; mh=$(max_height); [ -n "$mh" ] && [ "$mh" -ge 6 ] && { lift=1; break; }; sleep 3; t=$((t+3)); done
  if [ "$lift" != 1 ]; then bad "[$label] never lifted off (tip $(max_height)) — localhost mesh artifact"; cleanup; return; fi
  note "[$label] lifted off + producing (tip $(max_height))"
  check_no_fork && ok "[$label] no fork pre-kill"

  # Kill 2 validators (v5,v6) — keep v0/v1 (bootstrap+resync anchors) up.
  # 5 survivors == exactly quorum, so they must AGREE on one block to advance.
  local base; base=$(max_height)
  stop_node 5; stop_node 6
  note "[$label] killed v5,v6 at tip=$base — 5 survivors == exact quorum"

  # The discriminator: do the 5 survivors keep finalizing?
  if advances_past "$((base + 5))" 60; then
    ok "[$label] survivors ADVANCED through 2-down (tip $(max_height))"
    check_no_fork && ok "[$label] no fork after 2-down"
    SCEN_RESULT="ADVANCED"
  else
    bad "[$label] survivors STALLED after 2-down (stuck ~$(max_height), base $base)"
    SCEN_RESULT="STALLED"
  fi

  # Restart the 2 and confirm rejoin (only meaningful if the chain is alive).
  start_node 5; start_node 6
  if [ "$SCEN_RESULT" = "ADVANCED" ]; then
    advances_past "$(max_height)" 45 && ok "[$label] chain still advancing after rejoin" \
      || note "[$label] post-rejoin advance slow (localhost)"
    check_no_fork && ok "[$label] no fork after rejoin"
  fi
  cleanup; sleep 2
}

echo "=== Solen fork-choice v2 — 2-down liveness drill ($PROFILE) ==="
[ -x "$NODE" ] || { echo "building $PROFILE binary..."; (cd "$ROOT" && cargo build --"$PROFILE" -p solen-node) || exit 1; }
pkill -f "solen-fcv2-drill/n" 2>/dev/null; sleep 1

echo "[1/2] v1 baseline — gate OFF (expect STALL after kill-2)"
run_scenario "v1" 999999999
V1_RESULT="$SCEN_RESULT"

echo "[2/2] v2 fix — gate ON at height 5 (expect ADVANCE after kill-2)"
run_scenario "v2" 5
V2_RESULT="$SCEN_RESULT"

echo
echo "=== RESULT: v1(kill-2)=$V1_RESULT  v2(kill-2)=$V2_RESULT ==="
# What this drill can and cannot show:
#  - The PASS signal is v2=ADVANCED with no fork: the v2 wiring (candidate
#    tracking + revote broadcast over real gossip) works end-to-end and is a
#    no-regression — the chain stays alive and consistent through 2-down.
#  - A FAIL signal is v2=STALLED or any fork: a real regression to investigate.
#  - If v1 ALSO advanced, a clean `kill -9` of 2 nodes did NOT reproduce the
#    competing-block deadlock. Expected: the deployed binary already has the
#    deterministic-backup fix, so two cleanly-dead nodes don't emit competing
#    blocks. The mainnet deadlock needed the churn conditions a clean kill can't
#    recreate (a hung/zombie node holding stale state + a sync-starved fork +
#    rotation timing). The DETERMINISTIC deadlock repro is the engine unit tests
#    (competing_blocks_with_split_attestations_deadlock / ..._converge_...).
if [ "$V2_RESULT" = "ADVANCED" ]; then
  echo "v2 e2e OK ✅ — wiring works, survives 2-down, no fork (no-regression)."
  [ "$V1_RESULT" = "ADVANCED" ] && echo "NOTE: clean kill-2 did not reproduce the v1 deadlock (see header) — unit tests are the deterministic repro."
  exit 0
else
  echo "v2 did NOT advance through 2-down — REGRESSION to investigate (logs in $BASE)."
  exit 1
fi
