#!/usr/bin/env bash
set -euo pipefail

# SSH-based cluster orchestration script
# Usage:
#   ./scripts/run_cluster_ssh.sh <config.toml> <host1> <host2> <host3> <host4>
# Example:
#   ./scripts/run_cluster_ssh.sh config/example.deventer.toml \
#     bot1@188.245.97.41 bot2@135.181.145.174 bot3@5.223.51.238 bot4@178.156.219.107

CONFIG=${1:-}
shift || true
RUN_ID=${RUN_ID:-$(date -u +"%Y%m%d_%H%M%S")}
REMOTE_DIR=${REMOTE_DIR:-/opt/ippan-bots}
WAIT_FOR_RESULTS=${WAIT_FOR_RESULTS:-1}
MAX_WAIT_SEC=${MAX_WAIT_SEC:-7200}
POLL_SEC=${POLL_SEC:-2}

if [ -z "${CONFIG}" ]; then
  echo "Usage: $0 <config.toml> <host1> <host2> <host3> <host4>"
  exit 1
fi

if [ ! -f "$CONFIG" ]; then
  echo "Error: Config file not found: $CONFIG"
  exit 1
fi

WORKER_HOSTS=("$@")
if [ "${#WORKER_HOSTS[@]}" -eq 0 ]; then
  echo "Error: No worker hosts provided."
  echo "Usage: $0 <config.toml> <host1> <host2> <host3> <host4>"
  exit 1
fi

LOCAL_OUT="results/deventer/${RUN_ID}"
mkdir -p "${LOCAL_OUT}"

stop_workers() {
  echo ""
  echo "=== Stopping workers (run_id=${RUN_ID}) ==="
  for host in "${WORKER_HOSTS[@]}"; do
    # Best-effort stop by PID file; ignore errors.
    ssh "$host" "set -e; cd $REMOTE_DIR || exit 0; \
      for f in results/*.pid; do \
        [ -f \"\$f\" ] || continue; \
        pid=\$(cat \"\$f\" 2>/dev/null || true); \
        if [ -n \"\$pid\" ]; then kill \"\$pid\" 2>/dev/null || true; fi; \
      done" >/dev/null 2>&1 || true
  done
}
trap stop_workers INT TERM

echo "=== Building release binaries locally ==="
cargo build --release --bin worker --bin controller

BINARY="target/release/worker"

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
for host in "${WORKER_HOSTS[@]}"; do
  # Prefer the SSH username as the worker-id (bot1@..., bot2@..., ...).
  worker_id="${host%@*}"
  if [ -z "${worker_id}" ] || [ "${worker_id}" = "${host}" ]; then
    # Fallback: sanitize host string.
    worker_id="$(echo "${host}" | tr '.:@/' '____')"
  fi

  echo "Starting ${worker_id} on ${host}..."
  ssh "$host" "set -e; cd $REMOTE_DIR; \
    nohup ./worker \
      --config config/config.toml \
      --mode http \
      --worker-id ${worker_id} \
      --run-id ${RUN_ID} \
      --print-every-ms 1000 \
      > results/worker_${worker_id}_${RUN_ID}.log 2>&1 & \
    echo \$! > results/worker_${worker_id}_${RUN_ID}.pid"
done

echo ""
echo "=== Workers started ==="
echo "run_id: ${RUN_ID}"
echo "Remote logs: ${REMOTE_DIR}/results/worker_*_${RUN_ID}.log"
echo "Local output dir: ${LOCAL_OUT}"

echo ""
echo "=== Tail logs (brief) ==="
for host in "${WORKER_HOSTS[@]}"; do
  worker_id="${host%@*}"
  if [ -z "${worker_id}" ] || [ "${worker_id}" = "${host}" ]; then
    worker_id="$(echo "${host}" | tr '.:@/' '____')"
  fi
  echo "--- ${host} (${worker_id}) ---"
  ssh "$host" "tail -n 20 $REMOTE_DIR/results/worker_${worker_id}_${RUN_ID}.log 2>/dev/null || true" || true
done

if [ "${WAIT_FOR_RESULTS}" -eq 1 ]; then
  echo ""
  echo "=== Waiting for results (max ${MAX_WAIT_SEC}s, poll ${POLL_SEC}s) ==="
  start_epoch="$(date -u +%s)"
  while true; do
    done_count=0
    for host in "${WORKER_HOSTS[@]}"; do
      worker_id="${host%@*}"
      if [ -z "${worker_id}" ] || [ "${worker_id}" = "${host}" ]; then
        worker_id="$(echo "${host}" | tr '.:@/' '____')"
      fi
      if ssh "$host" "test -f $REMOTE_DIR/results/worker_${worker_id}_${RUN_ID}.json"; then
        done_count=$((done_count + 1))
      fi
    done
    if [ "${done_count}" -eq "${#WORKER_HOSTS[@]}" ]; then
      break
    fi
    now_epoch="$(date -u +%s)"
    elapsed=$((now_epoch - start_epoch))
    if [ "${elapsed}" -ge "${MAX_WAIT_SEC}" ]; then
      echo "Timed out waiting for results after ${elapsed}s."
      break
    fi
    sleep "${POLL_SEC}"
  done
fi

echo ""
echo "=== Collecting worker results into ${LOCAL_OUT} ==="
mkdir -p results "${LOCAL_OUT}"
for host in "${WORKER_HOSTS[@]}"; do
  worker_id="${host%@*}"
  if [ -z "${worker_id}" ] || [ "${worker_id}" = "${host}" ]; then
    worker_id="$(echo "${host}" | tr '.:@/' '____')"
  fi
  scp "$host:$REMOTE_DIR/results/worker_${worker_id}_${RUN_ID}.json" "${LOCAL_OUT}/" 2>/dev/null || true
done

echo ""
echo "=== Merging results locally (controller) ==="
# Controller currently reads from ./results, so stage a copy there for merging.
cp -f "${LOCAL_OUT}"/worker_*_"${RUN_ID}".json results/ 2>/dev/null || true

if ls "results/worker_*_${RUN_ID}.json" >/dev/null 2>&1; then
  cargo run --release --bin controller -- --config "$CONFIG" --merge-run-id "$RUN_ID"
  if [ -f "results/run_${RUN_ID}_merged.json" ]; then
    cp -f "results/run_${RUN_ID}_merged.json" "${LOCAL_OUT}/"
    echo "Merged: ${LOCAL_OUT}/run_${RUN_ID}_merged.json"
  fi
else
  echo "No worker result JSONs staged in ./results for run_id=${RUN_ID}; merge skipped."
  echo "Re-run collection after workers finish, then run:"
  echo "  cargo run --release --bin controller -- --config $CONFIG --merge-run-id $RUN_ID"
fi
