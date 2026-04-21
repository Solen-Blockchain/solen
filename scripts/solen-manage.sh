#!/usr/bin/env bash
#
# Solen Node Management Script
# Helps validators set up, run, and manage their Solen nodes.
#
set -euo pipefail

# ── Configuration ─────────────────────────────────────────────

SOLEN_DIR="${SOLEN_DIR:-$(cd "$(dirname "$0")/.." && pwd)}"
DATA_DIR="${SOLEN_DATA_DIR:-/opt/solen/data}"
BINARY="${SOLEN_DIR}/target/release/solen-node"
CLI="${SOLEN_DIR}/target/release/solen"
SERVICE_NAME="solen-node"
DEFAULT_NETWORK="mainnet"
NETWORK="${SOLEN_NETWORK:-$DEFAULT_NETWORK}"

# Network defaults
case "$NETWORK" in
  mainnet)  RPC_URL="https://rpc.solenchain.io";          CHAIN_ID=1;    RPC_PORT=9944;  P2P_PORT=30333 ;;
  testnet)  RPC_URL="https://testnet-rpc.solenchain.io";  CHAIN_ID=9000; RPC_PORT=19944; P2P_PORT=40333 ;;
  devnet)   RPC_URL="http://127.0.0.1:29944";             CHAIN_ID=1337; RPC_PORT=29944; P2P_PORT=50333 ;;
esac

# ── Colors ────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

header() { echo -e "\n${BOLD}${CYAN}=== $1 ===${NC}\n"; }
info()   { echo -e "${GREEN}[+]${NC} $1"; }
warn()   { echo -e "${YELLOW}[!]${NC} $1"; }
error()  { echo -e "${RED}[x]${NC} $1"; }
ask()    { echo -en "${BLUE}[?]${NC} $1: "; }

# ── Helpers ───────────────────────────────────────────────────

check_binary() {
  if [ ! -f "$BINARY" ]; then
    error "Binary not found at $BINARY"
    echo "  Run 'Build from source' first."
    return 1
  fi
}

check_cli() {
  if [ ! -f "$CLI" ]; then
    error "CLI not found at $CLI"
    echo "  Run 'Build from source' first."
    return 1
  fi
}

rpc_call() {
  local method="$1"
  local params="${2:-[]}"
  curl -s -X POST "$RPC_URL" \
    -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"$method\",\"params\":$params,\"id\":1}" 2>/dev/null
}

is_running() {
  systemctl is-active --quiet "$SERVICE_NAME" 2>/dev/null
}

# ════��═════════════════════════════════════════════════════════
# SETUP & INSTALLATION
# ══════════════════════════════════════════════════════════════

install_dependencies() {
  header "Install Dependencies"

  if command -v rustc &>/dev/null; then
    info "Rust already installed: $(rustc --version)"
  else
    warn "Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    info "Rust installed: $(rustc --version)"
  fi

  if command -v apt-get &>/dev/null; then
    info "Installing system packages..."
    sudo apt-get update -qq
    sudo apt-get install -y -qq build-essential pkg-config libssl-dev clang
    info "System packages installed."
  elif command -v dnf &>/dev/null; then
    sudo dnf install -y gcc make openssl-devel clang
  else
    warn "Unknown package manager. Install build-essential, libssl-dev, clang manually."
  fi

  info "Dependencies ready."
}

build_from_source() {
  header "Build from Source"
  cd "$SOLEN_DIR"

  if ! command -v cargo &>/dev/null; then
    error "Cargo not found. Run 'Install dependencies' first."
    return 1
  fi

  info "Building solen-node and solen CLI (release mode)..."
  cargo build --release -p solen-node -p solen-cli

  info "Build complete."
  echo "  Node:  $BINARY"
  echo "  CLI:   $CLI"
  ls -lh "$BINARY" "$CLI" 2>/dev/null | awk '{print "  " $NF ": " $5}'
}

init_genesis() {
  header "Initialize Genesis Config"
  check_binary || return

  ask "Data directory [$DATA_DIR]"
  read -r input
  [ -n "$input" ] && DATA_DIR="$input"

  mkdir -p "$DATA_DIR"

  if [ -f "$DATA_DIR/genesis.json" ]; then
    warn "Genesis already exists at $DATA_DIR/genesis.json"
    ask "Overwrite? (y/N)"
    read -r yn
    [ "$yn" != "y" ] && return
  fi

  "$BINARY" --network "$NETWORK" --data-dir "$DATA_DIR" --init-genesis
  info "Genesis written to $DATA_DIR/genesis.json"
}

setup_systemd() {
  header "Configure systemd Service"
  check_binary || return

  ask "Validator seed hex (leave empty for non-validator)"
  read -r seed

  ask "Data directory [$DATA_DIR]"
  read -r input
  [ -n "$input" ] && DATA_DIR="$input"

  ask "Genesis file path [$DATA_DIR/genesis.json]"
  read -r genesis
  [ -z "$genesis" ] && genesis="$DATA_DIR/genesis.json"

  ask "Bootstrap peer multiaddr (comma-separated, or empty)"
  read -r bootstrap

  # Build ExecStart command
  EXEC="$BINARY --network $NETWORK --data-dir $DATA_DIR --genesis $genesis"
  [ -n "$seed" ] && EXEC="$EXEC --validator-seed $seed"

  IFS=',' read -ra PEERS <<< "$bootstrap"
  for peer in "${PEERS[@]}"; do
    peer=$(echo "$peer" | xargs)
    [ -n "$peer" ] && EXEC="$EXEC --bootstrap $peer"
  done

  cat > /tmp/solen-node.service << EOF
[Unit]
Description=Solen Node ($NETWORK)
After=network.target

[Service]
Type=simple
User=$(whoami)
ExecStart=$EXEC
Restart=always
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF

  sudo mv /tmp/solen-node.service /etc/systemd/system/solen-node.service
  sudo systemctl daemon-reload
  info "Service installed at /etc/systemd/system/solen-node.service"
  echo ""
  echo "  Start:   sudo systemctl start solen-node"
  echo "  Enable:  sudo systemctl enable solen-node"
  echo "  Logs:    journalctl -u solen-node -f"
}

# ═══════════════════════════════��══════════════════════════════
# NODE OPERATIONS
# ═════════════════════���═════════════════════════���══════════════

start_node() {
  header "Start Node"
  if is_running; then
    warn "Node is already running."
  else
    sudo systemctl start "$SERVICE_NAME"
    sleep 2
    if is_running; then
      info "Node started."
    else
      error "Node failed to start. Check logs:"
      echo "  journalctl -u $SERVICE_NAME -n 20"
    fi
  fi
}

stop_node() {
  header "Stop Node"
  if is_running; then
    sudo systemctl stop "$SERVICE_NAME"
    info "Node stopped."
  else
    warn "Node is not running."
  fi
}

restart_node() {
  header "Restart Node"
  sudo systemctl restart "$SERVICE_NAME"
  sleep 2
  if is_running; then
    info "Node restarted."
  else
    error "Node failed to restart. Check logs."
  fi
}

view_logs() {
  header "Live Logs (Ctrl+C to exit)"
  journalctl -u "$SERVICE_NAME" -f --no-hostname -o cat
}

node_health() {
  header "Node Health Check"

  if is_running; then
    info "Service: ${GREEN}running${NC}"
  else
    error "Service: ${RED}stopped${NC}"
  fi

  local result
  result=$(rpc_call "solen_chainStatus")

  if echo "$result" | python3 -c "import sys,json; json.load(sys.stdin)['result']" &>/dev/null; then
    local height epoch pending state_root
    height=$(echo "$result" | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['height'])")
    pending=$(echo "$result" | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['pending_ops'])")
    state_root=$(echo "$result" | python3 -c "import sys,json; r=json.load(sys.stdin)['result']; print(r.get('state_root','?')[:16])")

    echo ""
    echo "  Height:     $height"
    echo "  Epoch:      $((height / 100))"
    echo "  Mempool:    $pending pending"
    echo "  State root: ${state_root}..."
    echo ""

    # Check if synced by comparing with 2 seconds ago
    sleep 3
    local result2 height2
    result2=$(rpc_call "solen_chainStatus")
    height2=$(echo "$result2" | python3 -c "import sys,json; print(json.load(sys.stdin)['result']['height'])" 2>/dev/null || echo "$height")
    local diff=$((height2 - height))
    if [ "$diff" -gt 0 ]; then
      info "Producing blocks: +$diff in 3s (~$(echo "scale=1; 3.0/$diff" | bc)s/block)"
    else
      warn "No new blocks in 3 seconds — may be syncing or stalled"
    fi
  else
    error "RPC not responding at $RPC_URL"
  fi
}

# ════════════════���════════════════════════════���════════════════
# VALIDATOR MANAGEMENT
# ══════════════════════════════════════════════════════════════

generate_key() {
  header "Generate Validator Key"
  check_cli || return

  ask "Key name"
  read -r name
  [ -z "$name" ] && { error "Name required."; return; }

  "$CLI" key generate "$name"
  info "Key generated. Use 'solen key lock' to encrypt the keystore."
}

list_keys() {
  header "Keystore"
  check_cli || return
  "$CLI" key list
}

register_validator() {
  header "Register as Validator"
  check_cli || return

  ask "Key name"
  read -r name
  ask "Stake amount (SOLEN, min 500,000)"
  read -r amount

  [ -z "$name" ] || [ -z "$amount" ] && { error "Name and amount required."; return; }

  "$CLI" --network "$NETWORK" register-validator "$name" "$amount"
}

check_validators() {
  header "Validator Set"
  check_cli || return
  "$CLI" --network "$NETWORK" validators
}

stake_tokens() {
  header "Stake Tokens"
  check_cli || return

  ask "Your key name"
  read -r from
  ask "Validator address (Base58 or hex)"
  read -r validator
  ask "Amount (SOLEN)"
  read -r amount

  "$CLI" --network "$NETWORK" stake "$from" "$validator" "$amount"
}

unstake_tokens() {
  header "Unstake Tokens"
  check_cli || return

  ask "Your key name"
  read -r from
  ask "Validator address"
  read -r validator
  ask "Amount (SOLEN)"
  read -r amount

  "$CLI" --network "$NETWORK" unstake "$from" "$validator" "$amount"
}

withdraw_stake() {
  header "Withdraw Matured Stake"
  check_cli || return

  ask "Your key name"
  read -r from
  "$CLI" --network "$NETWORK" withdraw-stake "$from"
}

unjail_validator() {
  header "Unjail Validator"
  check_cli || return

  ask "Validator key name"
  read -r name
  "$CLI" --network "$NETWORK" unjail "$name"
}

# ══���═════════════���═══════════════════════════════��═════════════
# MONITORING
# ══════════════════════════════════════════════════════════════

chain_status() {
  header "Chain Status"
  check_cli || return
  "$CLI" --network "$NETWORK" status
}

network_params() {
  header "Network Parameters"

  local result
  result=$(rpc_call "solen_chainStatus")

  if echo "$result" | python3 -c "import sys,json; json.load(sys.stdin)['result']['config']" &>/dev/null; then
    echo "$result" | python3 -c "
import sys, json
c = json.load(sys.stdin)['result']['config']
print(f\"  Block Time:         {c['block_time_ms']}ms ({c['block_time_ms']/1000}s)\")
print(f\"  Min Validator Stake: {int(c['min_validator_stake'])/100000000:,.0f} SOLEN\")
print(f\"  Unbonding Period:   {c['unbonding_period_epochs']} epochs\")
print(f\"  Epoch Length:       {c['epoch_length']} blocks\")
print(f\"  Base Fee:           {int(c['base_fee_per_gas'])/100000000:.8f} SOLEN/gas\")
print(f\"  Fee Burn Rate:      {c['burn_rate_bps']/100:.0f}%\")
"
  else
    warn "Config not available (node may need updating)."
    "$CLI" --network "$NETWORK" status 2>/dev/null || true
  fi
}

account_balance() {
  header "Check Balance"
  check_cli || return

  ask "Account (key name, Base58, or hex)"
  read -r account
  "$CLI" --network "$NETWORK" balance "$account"
}

# ═══════════════════════════════════��══════════════════════════
# MAINTENANCE
# ═════════���══════════════════════════════��═════════════════════

backup_data() {
  header "Backup Data Directory"

  ask "Data directory [$DATA_DIR]"
  read -r input
  [ -n "$input" ] && DATA_DIR="$input"

  if [ ! -d "$DATA_DIR" ]; then
    error "Data directory not found: $DATA_DIR"
    return
  fi

  local backup="${DATA_DIR}-backup-$(date +%Y%m%d-%H%M%S)"

  if is_running; then
    warn "Node is running. Stop it first for a clean backup."
    ask "Stop node and backup? (y/N)"
    read -r yn
    if [ "$yn" = "y" ]; then
      sudo systemctl stop "$SERVICE_NAME"
      info "Node stopped."
    else
      warn "Backing up while running (may be inconsistent)."
    fi
  fi

  info "Copying $DATA_DIR -> $backup ..."
  cp -r "$DATA_DIR" "$backup"
  info "Backup complete: $backup ($(du -sh "$backup" | cut -f1))"

  if ! is_running; then
    ask "Restart node? (Y/n)"
    read -r yn
    [ "$yn" != "n" ] && sudo systemctl start "$SERVICE_NAME" && info "Node restarted."
  fi
}

wipe_and_resync() {
  header "Wipe Data & Resync"

  ask "Data directory [$DATA_DIR]"
  read -r input
  [ -n "$input" ] && DATA_DIR="$input"

  warn "This will DELETE all chain data in $DATA_DIR"
  warn "The node will resync from peers via snapshot."
  ask "Are you sure? Type 'YES' to confirm"
  read -r confirm
  [ "$confirm" != "YES" ] && { info "Cancelled."; return; }

  if is_running; then
    sudo systemctl stop "$SERVICE_NAME"
    info "Node stopped."
  fi

  # Keep genesis.json if it exists
  local genesis_backup=""
  if [ -f "$DATA_DIR/genesis.json" ]; then
    genesis_backup=$(mktemp)
    cp "$DATA_DIR/genesis.json" "$genesis_backup"
  fi

  rm -rf "$DATA_DIR"
  mkdir -p "$DATA_DIR"

  if [ -n "$genesis_backup" ]; then
    mv "$genesis_backup" "$DATA_DIR/genesis.json"
    info "Preserved genesis.json"
  fi

  info "Data wiped. Start the node to begin resync."
  ask "Start node now? (Y/n)"
  read -r yn
  [ "$yn" != "n" ] && sudo systemctl start "$SERVICE_NAME" && info "Node started. Syncing from peers..."
}

update_binary() {
  header "Update Binary"
  cd "$SOLEN_DIR"

  info "Pulling latest source..."
  git pull --ff-only || { error "Git pull failed. Resolve conflicts manually."; return; }

  info "Building..."
  cargo build --release -p solen-node -p solen-cli

  info "Build complete."
  ls -lh "$BINARY" "$CLI" 2>/dev/null | awk '{print "  " $NF ": " $5}'

  if is_running; then
    ask "Restart node with new binary? (Y/n)"
    read -r yn
    if [ "$yn" != "n" ]; then
      sudo systemctl restart "$SERVICE_NAME"
      sleep 2
      is_running && info "Node restarted with new binary." || error "Restart failed."
    fi
  fi
}

# ════════════════════��═══════════════════════��═════════════════
# MAIN MENU
# ════════════���═════════════════════════════════════════════════

show_menu() {
  echo -e "${BOLD}${CYAN}"
  echo "  ____        _            "
  echo " / ___|  ___ | | ___ _ __  "
  echo " \\___ \\ / _ \\| |/ _ \\ '_ \\ "
  echo "  ___) | (_) | |  __/ | | |"
  echo " |____/ \\___/|_|\\___|_| |_|"
  echo -e "${NC}"
  echo -e " ${BOLD}Node Management${NC}  |  Network: ${YELLOW}${NETWORK}${NC}  |  RPC: ${BLUE}${RPC_URL}${NC}"
  echo ""
  echo -e " ${BOLD}Setup & Installation${NC}"
  echo "  1) Install dependencies"
  echo "  2) Build from source"
  echo "  3) Initialize genesis config"
  echo "  4) Configure systemd service"
  echo ""
  echo -e " ${BOLD}Node Operations${NC}"
  echo "  5) Start node"
  echo "  6) Stop node"
  echo "  7) Restart node"
  echo "  8) View live logs"
  echo "  9) Node health check"
  echo ""
  echo -e " ${BOLD}Validator Management${NC}"
  echo " 10) Generate validator key"
  echo " 11) List keys"
  echo " 12) Register as validator"
  echo " 13) View validator set"
  echo " 14) Stake tokens"
  echo " 15) Unstake tokens"
  echo " 16) Withdraw matured stake"
  echo " 17) Unjail validator"
  echo ""
  echo -e " ${BOLD}Monitoring${NC}"
  echo " 18) Chain status"
  echo " 19) Network parameters"
  echo " 20) Check balance"
  echo ""
  echo -e " ${BOLD}Maintenance${NC}"
  echo " 21) Backup data directory"
  echo " 22) Wipe & resync from peers"
  echo " 23) Update binary (git pull + build)"
  echo ""
  echo "  0) Exit"
  echo ""
}

main() {
  # Allow setting network via argument
  if [ "${1:-}" = "--network" ] && [ -n "${2:-}" ]; then
    NETWORK="$2"
    case "$NETWORK" in
      mainnet)  RPC_URL="https://rpc.solenchain.io";          CHAIN_ID=1 ;;
      testnet)  RPC_URL="https://testnet-rpc.solenchain.io";  CHAIN_ID=9000 ;;
      devnet)   RPC_URL="http://127.0.0.1:29944";             CHAIN_ID=1337 ;;
      *)        error "Unknown network: $NETWORK"; exit 1 ;;
    esac
    shift 2
  fi

  while true; do
    show_menu
    ask "Select option"
    read -r choice

    case "$choice" in
      1)  install_dependencies ;;
      2)  build_from_source ;;
      3)  init_genesis ;;
      4)  setup_systemd ;;
      5)  start_node ;;
      6)  stop_node ;;
      7)  restart_node ;;
      8)  view_logs ;;
      9)  node_health ;;
      10) generate_key ;;
      11) list_keys ;;
      12) register_validator ;;
      13) check_validators ;;
      14) stake_tokens ;;
      15) unstake_tokens ;;
      16) withdraw_stake ;;
      17) unjail_validator ;;
      18) chain_status ;;
      19) network_params ;;
      20) account_balance ;;
      21) backup_data ;;
      22) wipe_and_resync ;;
      23) update_binary ;;
      0|q|Q|exit) echo ""; info "Goodbye."; exit 0 ;;
      *)  error "Invalid option: $choice" ;;
    esac

    echo ""
    echo -e "${BLUE}Press Enter to continue...${NC}"
    read -r
  done
}

main "$@"
