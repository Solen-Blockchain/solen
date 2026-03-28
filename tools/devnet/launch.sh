#!/usr/bin/env bash
set -euo pipefail

echo "=== Solen Local Devnet ==="
echo "Building workspace..."
cargo build --workspace

echo "Starting solen-node..."
# TODO: launch node with devnet config
echo "Devnet not yet implemented. Build succeeded."
