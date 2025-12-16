#!/bin/bash
set -e

# Configuration
CONFIG_FILE="${1:-config/example.cluster.toml}"
WORKER_BINARY="./target/release/worker"
REMOTE_DIR="/tmp/ippan-load-robot"

if [ ! -f "$CONFIG_FILE" ]; then
    echo "Error: Config file not found: $CONFIG_FILE"
    exit 1
fi

# Parse worker hosts from config
WORKER_HOSTS=$(grep "worker_hosts" "$CONFIG_FILE" -A 10 | grep -o '"[^"]*"' | tr -d '"' | grep -v worker_hosts)

if [ -z "$WORKER_HOSTS" ]; then
    echo "Error: No worker hosts found in config"
    exit 1
fi

echo "=== Building Worker Binary ==="
cargo build --release --bin worker

echo ""
echo "=== Deploying to Worker Hosts ==="
for HOST in $WORKER_HOSTS; do
    echo "Deploying to $HOST..."
    ssh "$HOST" "mkdir -p $REMOTE_DIR"
    scp "$WORKER_BINARY" "$HOST:$REMOTE_DIR/"
    scp "$CONFIG_FILE" "$HOST:$REMOTE_DIR/config.toml"
done

echo ""
echo "=== Starting Workers ==="
WORKER_PIDS=()
WORKER_IDX=0

for HOST in $WORKER_HOSTS; do
    WORKER_ID="worker-$WORKER_IDX"
    echo "Starting $WORKER_ID on $HOST..."
    
    ssh "$HOST" "cd $REMOTE_DIR && nohup ./worker --config config.toml --mode http --worker-id $WORKER_ID > worker.log 2>&1 &" &
    WORKER_PIDS+=($!)
    
    WORKER_IDX=$((WORKER_IDX + 1))
    sleep 1
done

echo ""
echo "=== Waiting for Workers to Complete ==="
echo "Press Ctrl+C to stop early"

# Wait for SSH commands to complete
for PID in "${WORKER_PIDS[@]}"; do
    wait "$PID" || true
done

# Give workers time to finish
sleep 5

echo ""
echo "=== Collecting Results ==="
mkdir -p results

for HOST in $WORKER_HOSTS; do
    echo "Collecting results from $HOST..."
    scp "$HOST:$REMOTE_DIR/results/*.json" results/ || echo "No results from $HOST"
done

echo ""
echo "=== Running Controller to Merge Results ==="
cargo run --release --bin controller -- --config "$CONFIG_FILE"

echo ""
echo "=== Cluster Run Complete ==="
echo "Check results/ directory for merged output"
