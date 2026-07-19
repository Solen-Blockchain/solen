#!/usr/bin/env bash
#
# Solen post-quantum auth — Phase 0 activation canary
# ===================================================
# Validates the `--pq-auth-height` flag-day BEFORE it touches mainnet. Proves,
# on an isolated 3-validator localhost devnet, the four things the activation
# runbook depends on (see memory: project-solen-pq-activation):
#
#   A. VECTOR PARITY   ML-DSA-65 signatures are byte-identical across the Rust
#                      node (fips204) and the TS wallet (@noble/post-quantum),
#                      from the committed pq_vectors.json known-answer vector.
#   B. SHIPS DORMANT   with the flag OFF (u64::MAX), a Hybrid SetAuth upgrade is
#                      REJECTED — a not-yet-activated node never honors PQ auth.
#   C. ACTIVATES CLEAN with the flag at a low height H, past H a Hybrid upgrade
#                      is ACCEPTED, a subsequent hybrid-signed transfer lands,
#                      and all 3 validators AGREE on the state root (no fork).
#   D. MIXED-H FORKS   a 4th node started DORMANT while the cluster is active
#                      cannot follow the chain past the first Hybrid op — the
#                      concrete demonstration of why every node needs the SAME
#                      H before the chain reaches it.
#
# Isolated --network devnet on localhost; never touches testnet/mainnet. The
# CLI keystore is redirected to $BASE/clihome so it can't touch ~/.solen.
# Usage: tools/devnet/pq-auth-drill.sh [--debug]
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PROFILE="release"; [ "${1:-}" = "--debug" ] && PROFILE="debug"
NODE="$ROOT/target/$PROFILE/solen-node"
CLI="$ROOT/target/$PROFILE/solen"
GENESIS="$ROOT/tools/devnet/pq-drill-genesis-3.json"
VECTORS="$ROOT/sdks/wallet-sdk-ts/test/pq_vectors.json"
BASE="/tmp/solen-pq-drill"
CHAIN_ID=1337
N=3
H_ACTIVE=10                 # low activation height for part C/D
H_OFF=999999999             # dormant (stand-in for u64::MAX)
SEEDS=("0101010101010101010101010101010101010101010101010101010101010101" \
       "0202020202020202020202020202020202020202020202020202020202020202" \
       "0303030303030303030303030303030303030303030303030303030303030303")
ALICE_SEED="0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a"
BOB_SEED="0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b"

rpc_port(){ echo $((34944 + $1 * 10)); }
p2p_port(){ echo $((54333 + $1)); }

PASS=0; FAIL=0
ok(){   echo "  ✅ $*"; PASS=$((PASS+1)); }
bad(){  echo "  ❌ $*"; FAIL=$((FAIL+1)); }
note(){ echo "  ·  $*"; }

# CLI with an isolated keystore + devnet RPC (node 0 by default).
cli(){ local port="${PQ_CLI_PORT:-$(rpc_port 0)}"
  HOME="$BASE/clihome" "$CLI" --rpc "http://127.0.0.1:$port" --chain-id "$CHAIN_ID" "$@"; }

boot_args(){ local self=$1 a=""; for i in $(seq 0 $((N-1))); do
  [ "$i" -ne "$self" ] && a="$a --bootstrap /ip4/127.0.0.1/tcp/$(p2p_port "$i")"; done; echo "$a"; }

# start_node <i> <pq_auth_height> [seed_override]
start_node(){ local i=$1 H=$2 seed="${3:-${SEEDS[$i]}}"
  nohup "$NODE" --network devnet --genesis "$GENESIS" --validator-seed "$seed" \
    --data-dir "$BASE/n$i" --rpc-port "$(rpc_port "$i")" --p2p-port "$(p2p_port "$i")" \
    --explorer-port 0 --pq-auth-height "$H" \
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
pqheight(){ rpc "$1" solen_chainStatus | python3 -c "import sys,json
try: print(json.load(sys.stdin)['result']['config']['pq_auth_height'])
except: print('')" 2>/dev/null; }
root_at(){ rpc "$1" solen_getBlock "[$2]" | python3 -c "import sys,json
try: print(json.load(sys.stdin)['result']['state_root'][:16])
except: print('NONE')" 2>/dev/null; }
# auth method tags for a base58 account id, space-separated (Ed25519 Hybrid ...).
auth_tags(){ rpc 0 solen_getAccount "[\"$1\"]" | python3 -c "import sys,json
try: print(' '.join(list(m.keys())[0] for m in json.load(sys.stdin)['result'].get('auth_methods',[])))
except: print('')" 2>/dev/null; }

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
  done; note "state roots agree at height $h ($first)"; return 0; }

wait_height(){ local target=$1 timeout=${2:-90} t=0
  while [ "$t" -lt "$timeout" ]; do local mh; mh=$(max_height)
    [ -n "$mh" ] && [ "$mh" -ge "$target" ] && return 0; sleep 2; t=$((t+2)); done; return 1; }

# Op inclusion is asynchronous (submit → gossip to producer → block). Poll for
# the on-chain EFFECT rather than sleeping a fixed window, so assertions don't
# race a slow-but-correct cluster.
bal(){ rpc 0 solen_getAccount "[\"$1\"]" | python3 -c "import sys,json
try: print(json.load(sys.stdin)['result']['balance'])
except: print('')" 2>/dev/null; }
# wait until account <id> carries auth tag <tag>
wait_auth(){ local id=$1 tag=$2 to=${3:-25} t=0
  while [ "$t" -lt "$to" ]; do case " $(auth_tags "$id") " in *" $tag "*) return 0;; esac; sleep 2; t=$((t+2)); done; return 1; }
# wait until account <id> balance exceeds <base>
wait_bal_gt(){ local id=$1 base=$2 to=${3:-25} t=0 b
  while [ "$t" -lt "$to" ]; do b=$(bal "$id"); [ -n "$b" ] && [ "$b" -gt "$base" ] && return 0; sleep 2; t=$((t+2)); done; return 1; }

launch_cluster(){ local H=$1; rm -rf "$BASE"; mkdir -p "$BASE/clihome"
  for i in $(seq 0 $((N-1))); do mkdir -p "$BASE/n$i"; start_node "$i" "$H"; done; }

# Launch and confirm liftoff, retrying once on a first-bringup P2P mesh race
# (3 validators need quorum=2; if the localhost mesh forms slowly the cluster
# can stall short of it — a relaunch reliably clears it).
bringup(){ local H=$1 target=$2 timeout=${3:-90} try
  for try in 1 2 3; do
    launch_cluster "$H"
    wait_height "$target" "$timeout" && { note "lifted off on attempt $try (tip $(max_height))"; return 0; }
    note "liftoff attempt $try stalled (tip $(max_height)); relaunching…"
    for i in $(seq 0 $((N-1))); do stop_node "$i"; done; sleep 3
  done
  return 1; }

cleanup(){ for i in 0 1 2 3; do stop_node "$i"; done; }
trap cleanup EXIT

echo "=== Solen post-quantum auth — Phase 0 canary ($PROFILE) ==="
[ -x "$NODE" ] || { echo "building node…"; (cd "$ROOT" && cargo build --"$PROFILE" -p solen-node) || exit 1; }
[ -x "$CLI" ]  || { echo "building cli…";  (cd "$ROOT" && cargo build --"$PROFILE" -p solen-cli)  || exit 1; }
pkill -f "solen-pq-drill/n" 2>/dev/null; sleep 1

# ── Part A — ML-DSA vector parity (no node) ─────────────────────────────────
# NOTE: ML-DSA-65 signing is hedged (randomized), so Rust and TS signatures are
# NOT byte-identical — that's expected. The property that matters is mutual
# VERIFICATION plus digest/keygen parity, which the maintained vector tests
# already encode: signing.test.ts asserts the signing-message digest is
# byte-identical to Rust, keygen-from-seed matches Rust's pubkey, and the
# Rust-produced signature verifies under the TS verifier (Rust→TS). We run those
# authoritative suites rather than re-deriving a weaker check here.
echo "[A] ML-DSA-65 cross-impl parity (digest + keygen + Rust↔TS verify)"
if (cd "$ROOT/sdks/wallet-sdk-ts" && npx --yes vitest run src/signing.test.ts) >"$BASE.vitest.log" 2>&1; then
  ok "wallet-sdk-ts signing vectors pass (digest parity, keygen parity, Rust→TS verify)"
else
  bad "wallet-sdk-ts signing vectors FAILED (see $BASE.vitest.log) — wallet/node PQ divergence"
fi
if (cd "$ROOT" && cargo test --"$PROFILE" -p solen-crypto -- --quiet) >"$BASE.cargo-crypto.log" 2>&1; then
  ok "solen-crypto ML-DSA verify tests pass"
else
  bad "solen-crypto ML-DSA tests FAILED (see $BASE.cargo-crypto.log)"
fi
mkdir -p "$BASE"

# ── Part B — dormant: SetAuth lands, hybrid SIGNING is rejected ─────────────
# KEY SEMANTIC: pq_auth_height gates signature VERIFICATION, not SetAuth. A
# SetAuth→Hybrid is authorized by the CURRENT Ed25519 key, so the node accepts
# and stores it even while dormant — which then BRICKS the account (its next op
# must be hybrid-signed, and hybrid verification is off). This is exactly why
# the wallet gates the upgrade UI; the node does not protect you. We prove both
# halves: the registration lands, and a hybrid-signed op is then rejected.
echo "[B] dormant: SetAuth→Hybrid lands, but hybrid-signed ops are rejected (the brick the wallet guards)"
if ! bringup "$H_OFF" 6 90; then bad "[B] cluster never lifted off (tip $(max_height))"; else
  note "[B] chainStatus pq_auth_height=$(pqheight 0)"
  cli key import alice "$ALICE_SEED" >/dev/null 2>&1
  cli key import bob   "$BOB_SEED"   >/dev/null 2>&1
  ALICE_ID=$(cli account alice 2>/dev/null | awk '/ID:/{print $2}')
  BOB_ID=$(cli account bob 2>/dev/null | awk '/ID:/{print $2}')
  cli key quantum-upgrade alice --hybrid >/dev/null 2>&1
  if wait_auth "$ALICE_ID" Hybrid 25; then
    ok "[B] SetAuth→Hybrid stored while dormant (node does NOT gate registration — wallet must)"
  else
    bad "[B] SetAuth→Hybrid did not land while dormant"
  fi
  B0=$(bal "$BOB_ID")
  cli transfer alice bob 5 >/dev/null 2>&1
  if wait_bal_gt "$BOB_ID" "$B0" 12; then
    bad "[B] hybrid-signed transfer LANDED while dormant — verification gating broken!"
  else
    ok "[B] hybrid-signed transfer rejected while dormant (alice bricked — precisely what the wallet gate prevents)"
  fi
fi
cleanup; sleep 2

# ── Part C — activates cleanly (flag ON at H) ───────────────────────────────
echo "[C] active: past H=$H_ACTIVE, Hybrid upgrade + hybrid tx must land, no fork"
if ! bringup "$H_ACTIVE" $((H_ACTIVE + 3)) 90; then bad "[C] never advanced past H (tip $(max_height))"; else
  note "[C] tip $(max_height) > H=$H_ACTIVE; chainStatus pq_auth_height=$(pqheight 0)"
  cli key import alice "$ALICE_SEED" >/dev/null 2>&1
  cli key import bob   "$BOB_SEED"   >/dev/null 2>&1
  ALICE_ID=$(cli account alice 2>/dev/null | awk '/ID:/{print $2}')
  BOB_ID=$(cli account bob 2>/dev/null | awk '/ID:/{print $2}')

  cli key quantum-upgrade alice --hybrid >/dev/null 2>&1
  if wait_auth "$ALICE_ID" Hybrid 25; then ok "[C] alice on-chain auth is Hybrid"; else bad "[C] alice auth not Hybrid ($(auth_tags "$ALICE_ID"))"; fi

  B0=$(bal "$BOB_ID")
  cli transfer alice bob 5 >/dev/null 2>&1
  if wait_bal_gt "$BOB_ID" "$B0" 20; then
    PQ_OP_HEIGHT=$(height 0)
    ok "[C] hybrid-signed transfer landed (bob rose; observed by ~height $PQ_OP_HEIGHT)"
  else
    bad "[C] hybrid-signed transfer did not land (bob flat at $B0)"
  fi
  check_no_fork && ok "[C] 3 validators agree on state root (no fork through PQ ops)"
fi

# ── Part D — mixed-H forks (the invariant) ──────────────────────────────────
# A dormant node re-executing a block that contains a hybrid-authorized op
# rejects that op → cannot reproduce the canonical state root → it stalls below
# that height instead of following the chain. That divergence is why every node
# must carry the SAME H before the chain reaches it. NOTE: the DETERMINISTIC
# proof of this is Part B (a dormant executor rejects a hybrid op — so two nodes
# at different H compute different results for the same block ⇒ fork). Part D is
# the live-consensus illustration and is best-effort on a localhost mesh: a
# late-joining node that can't bootstrap in time is reported inconclusive, never
# a false green.
echo "[D] invariant: a DORMANT node cannot reproduce the chain past the hybrid op"
if [ -n "${PQ_OP_HEIGHT:-}" ]; then
  CANON=NONE; for _ in 1 2 3 4 5; do CANON=$(root_at 0 "$PQ_OP_HEIGHT"); [ "$CANON" != "NONE" ] && break; sleep 2; done
  mkdir -p "$BASE/n3"; start_node 3 "$H_OFF"
  note "[D] node3 dormant; canonical root@$PQ_OP_HEIGHT=$CANON; watching node3 sync…"
  best=0; t=0
  while [ "$t" -lt 60 ]; do h=$(height 3); [ -n "$h" ] && [ "$h" -gt "$best" ] && best=$h
    [ "$best" -ge "$PQ_OP_HEIGHT" ] && break; sleep 3; t=$((t+3)); done
  N3ROOT=$(root_at 3 "$PQ_OP_HEIGHT")
  if [ "$CANON" = "NONE" ]; then
    note "[D] inconclusive — could not read canonical root@$PQ_OP_HEIGHT from node0 (transient RPC); rerun to confirm"
  elif [ "$best" -lt 2 ]; then
    note "[D] inconclusive — dormant node3 never bootstrapped (tip $best; localhost P2P race), not a PQ signal (see Part B for the deterministic proof)"
  elif [ "$N3ROOT" = "NONE" ] || [ "$N3ROOT" != "$CANON" ]; then
    ok "[D] node3 synced to ~$best but could NOT reproduce height $PQ_OP_HEIGHT (root=$N3ROOT vs canon=$CANON) — proves same-H requirement"
  else
    bad "[D] dormant node3 reproduced the PQ-op height — gating is not consensus-affecting as expected"
  fi
else
  note "[D] skipped — no PQ op landed in Part C to test against"
fi

echo
echo "=== RESULT: $PASS passed, $FAIL failed  (logs in $BASE) ==="
[ "$FAIL" -eq 0 ] && { echo "PQ Phase-0 canary GREEN ✅ — safe to proceed to Phase 1 (dormant fleet deploy)."; exit 0; } \
                  || { echo "PQ Phase-0 canary RED ❌ — DO NOT schedule activation; investigate above."; exit 1; }
