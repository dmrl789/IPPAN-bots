#!/usr/bin/env bash
set -euo pipefail

# SSH-based cluster orchestration script
# Usage: ./scripts/run_cluster_ssh.sh <config.toml>

CONFIG=${1:-config/example.cluster.toml}
RUN_ID=${RUN_ID:-$(date -u +"%Y%m%d_%H%M%S")}
REMOTE_DIR=${REMOTE_DIR:-/opt/ippan-bots}
COLLECT_AFTER_SEC=${COLLECT_AFTER_SEC:-0}

if [ ! -f "$CONFIG" ]; then
    echo "Error: Config file not found: $CONFIG"
    exit 1
fi

echo "=== Building release binary ==="
cargo build --release --bin worker

BINARY="target/release/worker"
if [ -n "${WORKER_HOSTS:-}" ]; then
  IFS=',' read -r -a WORKER_HOSTS <<< "${WORKER_HOSTS}"
else
  WORKER_HOSTS=("bot-server-1" "bot-server-2" "bot-server-3" "bot-server-4")  # set WORKER_HOSTS=host1,host2,host3,host4
fi

echo ""
echo "=== Deploying to worker hosts ==="
for host in "${WORKER_HOSTS[@]}"; do
    echo "Deploying to $host..."
    ssh "$host" "mkdir -p $REMOTE_DIR/config $REMOTE_DIR/results $REMOTE_DIR/keys"
    scp "$BINARY" "$host:$REMOTE_DIR/"
    scp "$CONFIG" "$host:$REMOTE_DIR/config/config.toml"
    if [ -f "keys/recipients.json" ]; then
      scp "keys/recipients.json" "$host:$REMOTE_DIR/keys/recipients.json"
    fi
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
        --run-id $RUN_ID \
        > results/${worker_id}.log 2>&1 &"
done

echo ""
echo "=== Workers started ==="
echo "Monitor logs on each host: $REMOTE_DIR/results/worker-*.log"

if [ "$COLLECT_AFTER_SEC" -gt 0 ]; then
  echo ""
  echo "=== Waiting ${COLLECT_AFTER_SEC}s before collecting results ==="
  sleep "$COLLECT_AFTER_SEC"
fi

echo ""
echo "=== Collecting any worker results into ./results ==="
mkdir -p results
for host in "${WORKER_HOSTS[@]}"; do
  scp "$host:$REMOTE_DIR/results/worker_*_${RUN_ID}.json" "results/" 2>/dev/null || true
done

echo ""
echo "=== If all workers have finished, merge with controller ==="
echo "cargo run --release --bin controller -- --config $CONFIG --merge-run-id $RUN_ID"
