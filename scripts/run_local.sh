#!/usr/bin/env bash
set -euo pipefail

# Run a single local worker in mock mode, then HTTP mode

echo "=== Building release binary ==="
cargo build --release --bin worker

echo ""
echo "=== Running worker in MOCK mode (5 second test) ==="
cargo run --release --bin worker -- \
  --config config/test.toml \
  --mode mock \
  --worker-id worker-mock-test \
  --print-every-ms 1000

echo ""
echo "=== Mock run complete ==="
echo "Results written to results/worker_mock-test_*.json"

echo ""
echo "=== To run against real HTTP endpoint, update config and run: ==="
echo "cargo run --release --bin worker -- \\"
echo "  --config config/example.local.toml \\"
echo "  --mode http \\"
echo "  --worker-id worker-1"
