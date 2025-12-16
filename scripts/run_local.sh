#!/bin/bash
set -e

echo "=== Building IPPAN Load Robot ==="
cargo build --release

echo ""
echo "=== Running Worker in Mock Mode ==="
cargo run --release --bin worker -- \
  --config config/example.local.toml \
  --mode mock \
  --worker-id local-worker-1 \
  --print-every-ms 1000

echo ""
echo "=== Run Complete ==="
echo "Check results/ directory for output files"
