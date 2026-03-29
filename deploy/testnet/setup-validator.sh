#!/usr/bin/env bash
set -euo pipefail

# Solen Testnet — Additional Validator Setup
#
# Run this on servers 2, 3, and 4 to join the testnet.
# Server 1 (validator-1 / seed node) should already be running via setup.sh.
#
# Prerequisites:
#   1. Install Rust:
#        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
#        source ~/.cargo/env
#
#   2. Install build tools (Ubuntu/Debian):
#        sudo apt-get update && sudo apt-get install -y build-essential pkg-config libssl-dev clang
#
#   3. Clone the repo:
#        git clone <repo> ~/solen && cd ~/solen
#
# Usage:
#   ./deploy/testnet/setup-validator.sh <validator-number>
#
# Examples:
#   ./deploy/testnet/setup-validator.sh 2    # validator-2 (server 2)
#   ./deploy/testnet/setup-validator.sh 3    # validator-3 (server 3)
#   ./deploy/testnet/setup-validator.sh 4    # validator-4 (server 4)

if [ $# -lt 1 ]; then
    echo "Usage: $0 <validator-number>"
    echo ""
    echo "  validator-number:  2, 3, or 4"
    echo "  (validator-1 is the seed node — use setup.sh for that)"
    exit 1
fi

INDEX=$1

# Map index to seed.
case $INDEX in
    2) SEED="0202020202020202020202020202020202020202020202020202020202020202" ;;
    3) SEED="0303030303030303030303030303030303030303030303030303030303030303" ;;
    4) SEED="0404040404040404040404040404040404040404040404040404040404040404" ;;
    *)
        echo "Error: validator-number must be 2, 3, or 4"
        echo "  (validator-1 is the seed node — use setup.sh for that)"
        exit 1
        ;;
esac

echo "=== Solen Testnet — Validator $INDEX Setup ==="
echo ""

# Create solen user if needed.
if ! id -u solen &>/dev/null; then
    sudo useradd -r -m -s /bin/bash solen
    echo "Created solen user"
fi

# Create directories.
sudo mkdir -p /opt/solen/{bin,config,data/testnet}
sudo chown -R solen:solen /opt/solen

# Build release binaries.
echo "Building release binaries..."
export C_INCLUDE_PATH=${C_INCLUDE_PATH:-/usr/lib/gcc/x86_64-linux-gnu/11/include}
cargo build --release --bin solen-node --bin solen

# Install binaries.
sudo cp target/release/solen-node /opt/solen/bin/
sudo cp target/release/solen /opt/solen/bin/

# Install genesis config (must match seed node).
sudo cp deploy/testnet/genesis.json /opt/solen/config/

# Create systemd service for this validator.
cat <<EOF | sudo tee /etc/systemd/system/solen-node.service > /dev/null
[Unit]
Description=Solen Testnet Validator $INDEX
After=network.target
Wants=network-online.target

[Service]
Type=simple
User=solen
Group=solen
WorkingDirectory=/opt/solen
ExecStart=/opt/solen/bin/solen-node \\
    --network testnet \\
    --genesis /opt/solen/config/genesis.json \\
    --data-dir /opt/solen/data/testnet \\
    --validator-seed $SEED \\
    --bootstrap /dns4/testnet-seed1.solenchain.com/tcp/40333
Restart=always
RestartSec=5
LimitNOFILE=65536

Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload

echo ""
echo "=== Setup Complete ==="
echo ""
echo "Start the validator:"
echo "  sudo systemctl start solen-node"
echo ""
echo "Enable on boot:"
echo "  sudo systemctl enable solen-node"
echo ""
echo "View logs:"
echo "  journalctl -u solen-node -f"
echo ""
echo "Check status:"
echo "  /opt/solen/bin/solen --rpc http://127.0.0.1:19944 status"
echo ""
echo "This node will connect to testnet-seed1.solenchain.com"
echo "and participate as validator-$INDEX in consensus."
