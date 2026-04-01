#!/usr/bin/env bash
set -euo pipefail

# Solen Testnet Deployment Script — Server 1 (Seed Node + RPC + Faucet)
#
# Prerequisites:
#   1. Install Rust:
#        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
#        source ~/.cargo/env
#
#   2. Install build tools and nginx (Ubuntu/Debian):
#        sudo apt-get update && sudo apt-get install -y build-essential pkg-config libssl-dev clang nginx
#
#   3. Set up SSL certs with certbot:
#        sudo apt-get install -y certbot python3-certbot-nginx
#        sudo certbot --nginx -d testnet-rpc.solenchain.io -d testnet-faucet.solenchain.io -d testnet-api.solenchain.io
#
#   4. Clone the repo:
#        git clone <repo> ~/solen && cd ~/solen
#
# Usage:
#   ./deploy/testnet/setup.sh

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
echo "  RPC:     https://testnet-rpc.solenchain.io"
echo "  Faucet:  https://testnet-faucet.solenchain.io"
echo "  API:     https://testnet-api.solenchain.io"
echo ""
echo "Test with CLI:"
echo "  solen --rpc https://testnet-rpc.solenchain.io status"
echo "  curl -X POST https://testnet-faucet.solenchain.io/drip -H 'Content-Type: application/json' -d '{\"account\": \"myaccount\"}'"
