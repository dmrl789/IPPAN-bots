#!/usr/bin/env bash
set -euo pipefail

# SSH-based cluster orchestration script
# Usage: ./scripts/run_cluster_ssh.sh <config.toml>

CONFIG=${1:-config/example.cluster.toml}

if [ ! -f "$CONFIG" ]; then
    echo "Error: Config file not found: $CONFIG"
    exit 1
fi

echo "=== Building release binary ==="
cargo build --release --bin worker

BINARY="target/release/worker"
WORKER_HOSTS=("bot-server-1" "bot-server-2" "bot-server-3")  # Update with your hosts
REMOTE_DIR="/opt/ippan-bots"

echo ""
echo "=== Deploying to worker hosts ==="
for host in "${WORKER_HOSTS[@]}"; do
    echo "Deploying to $host..."
    ssh "$host" "mkdir -p $REMOTE_DIR/config $REMOTE_DIR/results"
    scp "$BINARY" "$host:$REMOTE_DIR/"
    scp "$CONFIG" "$host:$REMOTE_DIR/config/config.toml"
done

echo ""
echo "=== Starting workers ==="
PIDS=()
for i in "${!WORKER_HOSTS[@]}"; do
    host="${WORKER_HOSTS[$i]}"
    worker_id="worker-$i"
    echo "Starting $worker_id on $host..."
    
    ssh "$host" "cd $REMOTE_DIR && nohup ./worker \
        --config config/config.toml \
        --mode http \
        --worker-id $worker_id \
        > results/${worker_id}.log 2>&1 &"
done

echo ""
echo "=== Workers started ==="
echo "Monitor logs on each host: $REMOTE_DIR/results/worker-*.log"
echo ""
echo "To collect results, run:"
echo "for host in ${WORKER_HOSTS[@]}; do"
echo "  scp \$host:$REMOTE_DIR/results/worker_*.json results/"
echo "done"
echo ""
echo "Then merge with controller:"
echo "cargo run --release --bin controller -- --config $CONFIG"
