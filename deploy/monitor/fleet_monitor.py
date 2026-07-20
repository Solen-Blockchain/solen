#!/usr/bin/env python3
"""
Solen fleet monitor — polls every node over SSH (their RPC is localhost-only),
evaluates health rules, and sends transition-based Telegram alerts.

Designed to run on an RPC node via a systemd timer (`--once`). Stdlib only.

Secrets via env: TELEGRAM_BOT_TOKEN, TELEGRAM_CHAT_ID
Run modes:
  --once       one poll (used by the timer)
  --dry-run    print alerts instead of sending (no Telegram needed)
  --loop N     poll every N seconds (alternative to the timer)
"""
import concurrent.futures as cf
import json
import os
import subprocess
import sys
import time
import urllib.parse
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
CFG = json.load(open(os.path.join(HERE, "config.json")))
STATE_PATH = os.environ.get("SOLEN_MONITOR_STATE", "/var/lib/solen-monitor/state.json")
TG_TOKEN = os.environ.get("TELEGRAM_BOT_TOKEN", "")
TG_CHAT = os.environ.get("TELEGRAM_CHAT_ID", "")
T = CFG["thresholds"]
PREFIX = CFG.get("alert_prefix", "")

# Critical alerts re-nag at remind_secs_critical (the user wants to be bugged
# often while a node is stuck in partition recovery / the fleet is forked).
def is_critical(key):
    return (key.startswith("partition:") or key.startswith("down:")
            or key.startswith("mismatch:") or key.startswith("manualint:")
            or key in ("fleet_stall", "fork"))

# ── remote probes ─────────────────────────────────────────────────────────
RPC = CFG["rpc_port"]; P2P = CFG["p2p_port"]; SVC = CFG["service"]; DATA = CFG["data_dir"]
WIN = T["partition_window_secs"]

RPC_TIMEOUT = T.get("rpc_timeout_secs", 10)

def _rpc(method, params="[]"):
    # --max-time generously: a node mid snapshot-cache refresh (scan+compress on
    # a 100 MB+ store) can take several seconds to answer chainStatus, and a tight
    # deadline here was misreading those nodes as DOWN.
    return (f"""curl -s --max-time {RPC_TIMEOUT} -X POST 127.0.0.1:{RPC} -H 'content-type: application/json' """
            f"""-d '{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{params}}}'""")

COLLECT = f"""
H=$({_rpc("solen_chainStatus")});
echo "height=$(echo "$H" | grep -oE '\"height\":[0-9]+' | head -1 | cut -d: -f2)";
echo "peers=$(ss -tn state established 2>/dev/null | grep -c :{P2P})";
echo "part=$(journalctl -u {SVC} --since '{WIN} sec ago' --no-pager 2>/dev/null | grep -c 'partition detected')";
echo "mismatch=$(journalctl -u {SVC} --since '{WIN} sec ago' --no-pager 2>/dev/null | grep -c 'state root mismatch on finalization')";
echo "manualint=$(journalctl -u {SVC} --since '{WIN} sec ago' --no-pager 2>/dev/null | grep -c 'needs manual intervention')";
echo "mminfo=$(journalctl -u {SVC} --since '{WIN} sec ago' --no-pager 2>/dev/null | grep 'state root mismatch on finalization' | tail -1 | sed -E 's/.* height=/height=/')";
echo "disk=$(df --output=pcent {DATA} 2>/dev/null | tail -1 | tr -dc 0-9)";
echo "rss=$(ps -o rss= -C {SVC} 2>/dev/null | awk '{{s+=$1}} END{{print int(s/1024)}}')";
IF=$(ip route 2>/dev/null | awk '/default/{{print $5; exit}}');
echo "rx=$(cat /sys/class/net/$IF/statistics/rx_bytes 2>/dev/null)";
echo "tx=$(cat /sys/class/net/$IF/statistics/tx_bytes 2>/dev/null)";
echo "up=$(systemctl is-active {SVC} 2>/dev/null)";
"""

def ssh(host, remote):
    cmd = ["ssh", *CFG["ssh_opts"], f'{CFG["ssh_user"]}@{host}', remote]
    out = subprocess.run(cmd, capture_output=True, text=True, timeout=CFG.get("ssh_cmd_timeout", 30))
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip() or f"ssh rc={out.returncode}")
    return out.stdout

def collect(host):
    d = {"host": host, "ts": time.time()}
    try:
        for line in ssh(host, COLLECT).splitlines():
            if "=" in line:
                k, _, v = line.partition("=")
                d[k.strip()] = v.strip()
        d["down"] = not (d.get("height", "").isdigit())
    except Exception as e:
        d["down"] = True
        d["error"] = str(e)[:120]
    return d

def block_root(host, height):
    try:
        out = ssh(host, _rpc("solen_getBlock", f"[{height}]"))
        import re
        m = re.search(r'"state_root":"([0-9a-f]+)"', out)
        return m.group(1) if m else None
    except Exception:
        return None

def jailed_validators(host):
    """Return list of jailed validator ids, or None if unknown."""
    try:
        data = json.loads(ssh(host, _rpc("solen_getValidators")))
        vs = data.get("result")
        if not isinstance(vs, list):
            return None
        out = []
        for v in vs:
            if v.get("jailed") is True or v.get("is_active") is False or v.get("active") is False:
                out.append(str(v.get("id") or v.get("address") or "?")[:12])
        return out
    except Exception:
        return None

# ── alert evaluation ──────────────────────────────────────────────────────
def evaluate(nodes, prev):
    """Return dict of active alerts {key: human message}."""
    alerts = {}
    up = [n for n in nodes if not n["down"]]
    heights = [int(n["height"]) for n in up if n.get("height", "").isdigit()]
    tip = max(heights) if heights else None

    # bandwidth rate from previous counters
    bw_prev = prev.get("bw", {})
    for n in up:
        h = n["host"]
        try:
            tx = int(n["tx"]); rx = int(n["rx"])
            p = bw_prev.get(h)
            if p and n["ts"] > p["ts"]:
                dt = n["ts"] - p["ts"]
                mbps = max(tx - p["tx"], 0) / dt / 1e6 + max(rx - p["rx"], 0) / dt / 1e6
                n["mbps"] = round(mbps, 1)
        except (KeyError, ValueError, ZeroDivisionError):
            pass

    # Debounce DOWN by WALL-CLOCK, not poll count: a single slow/missed poll
    # (e.g. a node mid snapshot-cache refresh) must not page. Track when each
    # node was first seen unreachable and only alert once it's been continuously
    # down for >= down_min_secs. Time-based so it's immune to poll cadence /
    # overlapping runs (a poll-count debounce can be defeated by two polls
    # landing seconds apart). down_since persists across polls via state.
    now_ts = time.time()
    down_min = T.get("down_min_secs", 90)
    prev_since = prev.get("down_since", {})
    down_since = {}

    # per-node rules
    for n in nodes:
        h = n["host"]
        if n["down"]:
            first = prev_since.get(h, now_ts)  # carry forward, or start the clock now
            down_since[h] = first
            elapsed = int(now_ts - first)
            _dm = T.get("down_overrides", {}).get(h, down_min)
            if elapsed >= _dm:
                alerts[f"down:{h}"] = (f"🔴 {h} is DOWN / unreachable "
                                       f"({n.get('error','no RPC')}, {elapsed}s)")
            continue
        # reachable — clear the down clock (no down_since entry)
        if tip is not None and n.get("height", "").isdigit():
            behind = tip - int(n["height"])
            _bt = T.get("behind_overrides", {}).get(h, T["behind_blocks"])
            if behind > _bt:
                alerts[f"behind:{h}"] = f"🟠 {h} is {behind} blocks behind the fleet (tip {tip})"
        if n.get("peers", "").isdigit() and int(n["peers"]) < T["peers_min"]:
            alerts[f"peers:{h}"] = f"🟠 {h} has only {n['peers']} P2P peers"
        if n.get("part", "").isdigit() and int(n["part"]) >= T["partition_logs_per_window"]:
            alerts[f"partition:{h}"] = f"🔴 {h} in partition-recovery loop ({n['part']} hits/{WIN}s)"
        # state-root divergence: this node executed a block and got a different
        # state root than the proposer — the earliest, most specific signal of a
        # consensus fork (fires seconds after it happens, well before the 90s
        # fleet-stall trips). Names the block/proposer so triage skips straight
        # to the culprit. See runbooks/consensus-fork-recovery.md.
        if n.get("mismatch", "").isdigit() and int(n["mismatch"]) > 0:
            info = n.get("mminfo", "").strip()
            detail = f" — {info}" if info else ""
            alerts[f"mismatch:{h}"] = f"🔴 {h} STATE-ROOT DIVERGENCE — rejected a block on execution{detail}"
        # consensus gave up: node hit the manual-intervention latch (auto-resync
        # failed / sync disabled). A halted node will not self-heal without an
        # operator — page hard.
        if n.get("manualint", "").isdigit() and int(n["manualint"]) > 0:
            alerts[f"manualint:{h}"] = f"🔴 {h} CONSENSUS HALTED — auto-resync gave up, needs manual intervention"
        if "mbps" in n and n["mbps"] > T["bandwidth_mbps"]:
            alerts[f"bandwidth:{h}"] = f"🟠 {h} bandwidth {n['mbps']} MB/s (> {T['bandwidth_mbps']})"
        if n.get("disk", "").isdigit() and int(n["disk"]) >= T["disk_pct"]:
            alerts[f"disk:{h}"] = f"🟠 {h} disk {n['disk']}% full ({DATA})"
        # memory watchdog (warn tier): solen-node RSS climbing toward the
        # auto-restart threshold. Tracks a post-2026-07-17 leak that grows with
        # uptime; the guarded restarter below acts at mem_rss_mb_restart.
        if n.get("rss", "").isdigit() and int(n["rss"]) >= T.get("mem_rss_mb_warn", 7000):
            alerts[f"mem:{h}"] = (f"🟠 {h} solen-node RSS {n['rss']}MB "
                                  f"(watchdog restarts at {T.get('mem_rss_mb_restart', 8500)}MB)")

    # fleet-wide stall: tip hasn't advanced within fleet_stall_secs
    f_prev = prev.get("fleet")
    if tip is not None:
        if f_prev and tip <= f_prev["height"] and (time.time() - f_prev["ts"]) > T["fleet_stall_secs"]:
            alerts["fleet_stall"] = f"🔴 FLEET STALLED — tip stuck at {tip} for >{T['fleet_stall_secs']}s"

    # fork: state-root disagreement at a settled common height
    if len(heights) >= 2:
        common = min(heights) - 5
        if common > 0:
            roots = {}
            with cf.ThreadPoolExecutor(max_workers=16) as ex:
                futs = {ex.submit(block_root, n["host"], common): n["host"] for n in up}
                for fut in futs:
                    r = fut.result()
                    if r:
                        roots.setdefault(r, []).append(futs[fut])
            if len(roots) > 1:
                desc = "; ".join(f"{r[:10]}…→{','.join(hs)}" for r, hs in roots.items())
                alerts["fork"] = f"🔴 FORK at height {common}: {desc}"

    # jailed validators (query from any healthy node)
    if up:
        jl = jailed_validators(up[0]["host"])
        if jl:
            alerts["jailed"] = f"🔴 Jailed/inactive validators: {', '.join(jl)}"

    return alerts, tip, down_since

# ── telegram + state ──────────────────────────────────────────────────────
def send(msg, dry):
    if PREFIX:
        msg = f"{PREFIX} {msg}"
    if dry or not TG_TOKEN or not TG_CHAT:
        print(("[DRY] " if dry else "[no-telegram] ") + msg)
        return
    data = urllib.parse.urlencode({"chat_id": TG_CHAT, "text": msg, "disable_web_page_preview": "true"}).encode()
    try:
        urllib.request.urlopen(f"https://api.telegram.org/bot{TG_TOKEN}/sendMessage", data=data, timeout=10)
    except Exception as e:
        print(f"telegram send failed: {e}", file=sys.stderr)

def load_state():
    try:
        return json.load(open(STATE_PATH))
    except Exception:
        return {}

def save_state(s):
    os.makedirs(os.path.dirname(STATE_PATH), exist_ok=True)
    json.dump(s, open(STATE_PATH, "w"))

def maybe_restart_high_memory(nodes, state, dry):
    """Memory watchdog. If a node's solen-node RSS crosses mem_rss_mb_restart,
    restart the SINGLE highest one — but only when it's safe:
      * mem_auto_restart is enabled,
      * EVERY node is currently up (so at most one node is ever down at a time,
        keeping >=10/11 = comfortable quorum during the restart), and
      * the cooldown since the last auto-restart has elapsed (paces restarts,
        gives the last one time to rejoin).
    This is a stopgap for the uptime-correlated memory growth in the 2026-07-17
    binary until the leak is root-caused; a fresh node starts back at ~2.7GB.
    Returns the mem_restart state dict to persist (cooldown bookkeeping)."""
    mem = state.get("mem_restart", {})
    if not T.get("mem_auto_restart", False):
        return mem
    thr = T.get("mem_rss_mb_restart", 8500)
    over = sorted((n for n in nodes if not n["down"] and n.get("rss", "").isdigit()
                   and int(n["rss"]) >= thr),
                  key=lambda n: int(n["rss"]), reverse=True)
    if not over:
        return mem
    if any(n["down"] for n in nodes):      # someone already down — never 2 at once
        return mem
    now = time.time()
    if now - mem.get("ts", 0) < T.get("mem_restart_cooldown_secs", 1200):
        return mem
    t = over[0]; h = t["host"]; rss = int(t["rss"])
    msg = (f"🔧 memory watchdog: restarting {h} — solen-node RSS {rss}MB "
           f">= {thr}MB (quorum-safe, one node at a time)")
    if dry:
        print("[DRY] would " + msg)
        return mem
    try:
        ssh(h, f"systemctl restart {SVC}")
    except Exception as e:
        send(f"⚠️ memory watchdog FAILED to restart {h} (RSS {rss}MB): {str(e)[:100]}", dry)
        return mem
    send(msg, dry)
    return {"ts": now, "host": h, "rss": rss}

def poll(dry=False):
    state = load_state()
    with cf.ThreadPoolExecutor(max_workers=16) as ex:
        nodes = list(ex.map(collect, CFG["nodes"]))

    active, tip, down_since = evaluate(nodes, state)
    now = time.time()
    prev_alerts = state.get("alerts", {})

    # memory watchdog — guarded auto-restart of the highest-RSS node (stopgap for
    # the post-2026-07-17 memory growth). Runs before alert bookkeeping so the
    # restart message goes out this cycle.
    mem_restart = maybe_restart_high_memory(nodes, state, dry)

    # new + reminders
    new_state_alerts = {}
    for key, msg in active.items():
        if key in prev_alerts:
            since = prev_alerts[key]["since"]; last = prev_alerts[key]["last"]
            remind = T.get("remind_secs_critical", T["remind_secs"]) if is_critical(key) else T["remind_secs"]
            if now - last >= remind:
                send(f"⏰ STILL OPEN ({int((now-since)/60)}m): {msg}", dry); last = now
            new_state_alerts[key] = {"since": since, "last": last}
        else:
            send(msg, dry)
            new_state_alerts[key] = {"since": now, "last": now}
    # resolved
    for key in prev_alerts:
        if key not in active:
            send(f"✅ RESOLVED: {key}", dry)

    # persist bandwidth counters + fleet tip + alerts
    bw = {n["host"]: {"rx": int(n["rx"]), "tx": int(n["tx"]), "ts": n["ts"]}
          for n in nodes if not n["down"] and n.get("tx", "").isdigit() and n.get("rx", "").isdigit()}
    fleet = state.get("fleet")
    if tip is not None and (not fleet or tip > fleet["height"]):
        fleet = {"height": tip, "ts": now}
    save_state({"alerts": new_state_alerts, "bw": bw, "fleet": fleet,
                "down_since": down_since, "mem_restart": mem_restart})

    healthy = sum(1 for n in nodes if not n["down"])
    print(f"[{time.strftime('%H:%M:%S')}] {healthy}/{len(nodes)} up, tip={tip}, {len(active)} active alert(s)")

def main():
    args = sys.argv[1:]
    dry = "--dry-run" in args
    if "--loop" in args:
        interval = int(args[args.index("--loop") + 1])
        while True:
            try: poll(dry)
            except Exception as e: print(f"poll error: {e}", file=sys.stderr)
            time.sleep(interval)
    else:
        poll(dry)

if __name__ == "__main__":
    main()
