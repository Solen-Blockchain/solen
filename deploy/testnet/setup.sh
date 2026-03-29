#!/usr/bin/env bash
set -euo pipefail

# Solen Testnet Deployment Script
#
# Prerequisites:
#   - Ubuntu 22.04+
#   - Rust toolchain installed
#   - nginx installed
#   - certbot configured for solenchain.com
#
# Usage:
#   ./setup.sh

echo "=== Solen Testnet Setup ==="

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
export C_INCLUDE_PATH=/usr/lib/gcc/x86_64-linux-gnu/11/include
cargo build --release --bin solen-node --bin solen-faucet --bin solen

# Install binaries.
sudo cp target/release/solen-node /opt/solen/bin/
sudo cp target/release/solen-faucet /opt/solen/bin/
sudo cp target/release/solen /opt/solen/bin/

# Install genesis config.
sudo cp deploy/testnet/genesis.json /opt/solen/config/

# Install systemd services.
sudo cp deploy/testnet/solen-node.service /etc/systemd/system/
sudo cp deploy/testnet/solen-faucet.service /etc/systemd/system/
sudo systemctl daemon-reload

# Install nginx config.
sudo cp deploy/testnet/nginx.conf /etc/nginx/sites-available/solenchain
sudo ln -sf /etc/nginx/sites-available/solenchain /etc/nginx/sites-enabled/
sudo nginx -t && sudo systemctl reload nginx

echo ""
echo "=== Setup Complete ==="
echo ""
echo "Start the testnet:"
echo "  sudo systemctl start solen-node"
echo "  sudo systemctl start solen-faucet"
echo ""
echo "Enable on boot:"
echo "  sudo systemctl enable solen-node"
echo "  sudo systemctl enable solen-faucet"
echo ""
echo "View logs:"
echo "  journalctl -u solen-node -f"
echo "  journalctl -u solen-faucet -f"
echo ""
echo "Endpoints:"
echo "  RPC:     https://rpc.solenchain.com"
echo "  Faucet:  https://faucet.solenchain.com"
echo "  API:     https://api.solenchain.com"
echo ""
echo "Test with CLI:"
echo "  solen --rpc https://rpc.solenchain.com status"
echo "  curl -X POST https://faucet.solenchain.com/drip -H 'Content-Type: application/json' -d '{\"account\": \"myaccount\"}'"
