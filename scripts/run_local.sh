#!/usr/bin/env bash
set -euo pipefail

# Build and run controller with 4 local workers.
# Generates a throwaway recipients file if missing.

WORKERS=${1:-4}
CONFIG=${2:-config/example.local.toml}

mkdir -p keys results runs

if [ ! -f "keys/recipients.json" ]; then
  echo "=== Creating keys/recipients.json (local-only, ignored by git) ==="
  cat > keys/recipients.json <<'EOF'
[
  "@recipient1.ipn",
  "@recipient2.ipn",
  "@recipient3.ipn",
  "@recipient4.ipn"
]
EOF
fi

echo "=== Building release binaries ==="
cargo build --release --workspace

echo ""
echo "=== Running controller with $WORKERS local workers ==="
cargo run --release --bin controller -- \
  --config "$CONFIG" \
  --local-workers "$WORKERS"

echo ""
echo "=== Run complete ==="
echo "Individual worker results: results/worker_*.json"
echo "Merged results: results/run_*_merged.json"
