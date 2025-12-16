# IPPAN Load Robot

A distributed load testing system for IPPAN blockchain that can generate and submit transactions at high throughput with precise rate control and detailed metrics collection.

## Architecture

This system consists of two main components:

- **Worker**: Generates and sends transactions at a controlled rate with backpressure management, batching support, and detailed metrics tracking
- **Controller**: Orchestrates multiple workers across machines, applies ramp schedules, and aggregates results (v1: SSH orchestration, v2: HTTP control plane TODO)

## Features

- **Deterministic & Reproducible**: All randomness is seeded; runs are fully reproducible
- **No Float Arithmetic**: All rates and timings use integer math for consistency
- **Gradual Ramp-Up**: Configure multi-step TPS ramps (e.g., 1k → 10k → 50k)
- **Pluggable RPC Adapter**: Clean trait abstraction for transaction submission (currently stubbed for IPPAN RPC)
- **Distributed**: Run workers across multiple servers via SSH orchestration
- **Metrics**: Per-worker statistics with latency percentiles (p50/p95/p99)

## Quickstart

### Local Dry-Run (Mock Mode)

```bash
# Build the workspace
cargo build --release

# Run a single worker in mock mode (no real RPC calls)
./scripts/run_local.sh
```

This will:
- Generate deterministic transaction payloads
- Simulate submission with configurable latency
- Write results to `results/worker_*.json`

### Local Worker Against Real RPC

```bash
cargo run --release --bin worker -- \
  --config config/example.local.toml \
  --mode http \
  --worker-id worker-1
```

### View Ramp Schedule

```bash
cargo run --release --bin controller -- \
  --config config/example.local.toml \
  --ramp-only
```

### Multi-Worker Cluster via SSH

```bash
# Edit config/example.cluster.toml to set worker_hosts
# Ensure SSH access and that worker binaries are deployed

./scripts/run_cluster_ssh.sh config/example.cluster.toml
```

The controller will:
- Connect to each worker host via SSH
- Start workers with synchronized ramp schedules
- Collect and merge results into `results/run_<timestamp>_merged.json`

## Configuration

Example config (`config/example.local.toml`):

```toml
[scenario]
seed = 42
duration_ms = 60000
payload_bytes = 512
batch_max = 10

[[ramp.steps]]
tps = 1000
hold_ms = 10000

[[ramp.steps]]
tps = 5000
hold_ms = 10000

[[ramp.steps]]
tps = 10000
hold_ms = 20000

[target]
rpc_urls = ["http://127.0.0.1:8080"]
timeout_ms = 5000
max_in_flight = 1000

[worker]
id = "worker-1"
bind_metrics = "127.0.0.1:9100"
```

### Configuration Fields

- **scenario**
  - `seed` (u64): Deterministic seed for payload generation
  - `duration_ms` (optional u64): Global duration cap
  - `payload_bytes` (u32): Fixed size for generated payloads
  - `batch_max` (u32): Maximum transactions per batch (0 or 1 = no batching)

- **ramp.steps**: Each step has:
  - `tps` (u64): Target transactions per second for this step
  - `hold_ms` (u64): How long to maintain this rate

- **target**
  - `rpc_urls`: List of RPC endpoints (worker will round-robin)
  - `timeout_ms`: Request timeout
  - `max_in_flight`: Maximum concurrent pending requests

- **worker**
  - `id`: Worker identifier for metrics and results
  - `bind_metrics`: Address to bind metrics endpoint (optional)

- **controller**
  - `worker_hosts`: List of SSH hosts (v1) for orchestration

## RPC Adapter Note

The transaction submission logic is currently **stubbed** behind the `TxSubmitter` trait in `bots-core`. The exact IPPAN RPC endpoint format and transaction structure can be plugged in later without refactoring the core load generation logic.

Current implementations:
- `MockSubmitter`: Always accepts transactions with configurable delay (for testing)
- `HttpJsonSubmitter`: Stubbed HTTP implementation marked with TODOs for IPPAN-specific details

To add real IPPAN support, implement the `TxSubmitter` trait with proper transaction signing and RPC calls.

## Key Generation

Generate deterministic keys (not committed to repo):

```bash
cargo run --release --bin keygen -- \
  --out keys/worker-keys.json \
  --count 1000 \
  --seed 12345
```

Keys are stored in the `keys/` directory which is gitignored.

## Results Format

Worker results are written as JSON:

```json
{
  "worker_id": "worker-1",
  "timestamp": "2025-12-16T10:30:00Z",
  "duration_ms": 60000,
  "attempted": 300000,
  "sent": 299800,
  "accepted": 299500,
  "rejected": 200,
  "errors": 100,
  "latency_p50_ms": 45,
  "latency_p95_ms": 120,
  "latency_p99_ms": 250
}
```

## Development

### Build

```bash
cargo build --release
```

### Run Tests

```bash
cargo test --all
```

### Format & Lint

```bash
cargo fmt --all
cargo clippy --all --all-targets -- -D warnings
```

### CI

GitHub Actions runs:

- `cargo fmt --all --check`
- `cargo clippy --all --all-targets -- -D warnings`
- `cargo test --all`

## Project Structure

```
.
├── crates/
│   ├── bots-core/       # Shared types, rate limiter, ramp planner
│   ├── worker/          # Load generator binary
│   ├── controller/      # Orchestration binary
│   └── keygen/          # Key generation utility
├── config/              # Example TOML configurations
├── scripts/             # Helper scripts for running tests
└── results/             # Output directory for run results
```

## License

MIT License - see LICENSE file for details.
