#!/usr/bin/env bash
set -euo pipefail

# Run controller with N local workers

WORKERS=${1:-4}

echo "=== Building release binaries ==="
cargo build --release --bin worker --bin controller

echo ""
echo "=== Running controller with $WORKERS local workers ==="
cargo run --release --bin controller -- \
  --config config/example.local.toml \
  --local "$WORKERS"

echo ""
echo "=== Run complete ==="
echo "Individual worker results: results/worker_*.json"
echo "Merged results: results/run_*_merged.json"
