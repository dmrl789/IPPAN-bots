# IPPAN-bots Load Rig

A distributed load testing system for IPPAN blockchain that generates high-throughput transaction loads against the HTTP API endpoint `POST /tx/payment`.

## Architecture

This system consists of:

- **Worker**: Generates and sends individual payment transactions at a controlled rate with precise timing, backpressure management, and detailed metrics tracking
- **Controller**: Orchestrates multiple workers (local processes or SSH across machines), applies ramp schedules, and aggregates results

## Key Features

- **Deterministic & Reproducible**: All randomness is seeded; runs are fully reproducible
- **No Float Arithmetic**: All rates and timings use integer math (u64/u128) for consistency
- **Gradual Ramp-Up**: Configure multi-step TPS ramps (e.g., 1k → 10k → 50k)
- **Single-TX Per Request**: Targets `/tx/payment` with one transaction per HTTP POST (no batching)
- **Distributed**: Run workers across multiple servers via SSH orchestration or locally
- **Metrics**: Per-worker statistics with latency percentiles (p50/p95/p99) in integer ms

## Important Notes

### Always run preflight before load tests

Before any real run (especially distributed), run the preflight checks against your configured `rpc_urls`:

```bash
cargo run --bin preflight -- --config config/example.deventer.toml
```

### Port Usage

- **Port 8080**: IPPAN HTTP API endpoint (used by bots for `/tx/payment`)
- **Port 9000**: Node-to-node P2P (NOT used by bots)

### Transaction Signing

The `/tx/payment` endpoint uses **custodial signing** (server-side signing). You must provide a `signing_key` in the configuration.

**⚠️ SECURITY WARNING**: Only use test keys! Never use production keys with these load testing tools.

### Performance Reality

Because IPPAN currently has **no batch endpoint**, achieving very high TPS requires:

- Many concurrent requests (`max_in_flight` tuning)
- Multiple worker processes or servers
- Possibly multiple ingress nodes
- Proper TCP/HTTP keep-alive settings

This repo is designed to **measure** where bottlenecks are before changing IPPAN core.

## Quickstart

### Local Dry-Run (Mock Mode)

```bash
# Build the workspace
cargo build --release

# Run a single worker in mock mode (no real RPC calls)
./scripts/run_local.sh
```

This will:
- Generate deterministic payment transactions
- Simulate submission with configurable latency
- Write results to `results/worker_*.json`

### Local Worker Against Real RPC

```bash
cargo run --release --bin worker -- \
  --config config/example.local.toml \
  --mode http \
  --worker-id worker-1
```

Make sure to:
1. Update `config/example.local.toml` with your IPPAN node URL
2. Set proper `from`, `to`, `amount`, and `signing_key` values
3. Use test keys only!

### Multi-Worker Local Testing

```bash
# Spawn 4 local worker processes
./scripts/run_local_controller.sh 4

# Or use controller directly
cargo run --release --bin controller -- \
  --config config/example.local.toml \
  --local 4
```

### View Ramp Schedule

```bash
cargo run --release --bin controller -- \
  --config config/example.local.toml \
  --ramp-only
```

### Multi-Worker Cluster via SSH

```bash
# Deventer-style run: pass config + 4 worker hosts
./scripts/run_cluster_ssh.sh config/example.deventer.toml \
  bot1@188.245.97.41 bot2@135.181.145.174 bot3@5.223.51.238 bot4@178.156.219.107
```

The script will:
- Build and deploy worker binary to each host
- Start workers with synchronized ramp schedules
- Collect results into `results/deventer/<run_id>/` and merge into `results/run_<run_id>_merged.json`

### Prefund estimator (fees enabled)

```bash
cargo run --bin prefund -- --config config/example.deventer.toml
```

This prints a deterministic prefund estimate and writes `results/prefund_<timestamp>.json`.

## Deventer 4-bot run checklist

1. DNS/TLS sanity:
   - `api1..api4.ippan.uk` resolve to the expected IPs
2. Proxy config:
   - `/tx/payment` → `127.0.0.1:8080`
3. Preflight:
   - `cargo run --bin preflight -- --config config/example.deventer.toml`
4. Prefund:
   - `cargo run --bin prefund -- --config config/example.deventer.toml`
5. Plan fairness (optional but recommended):
   - `cargo run --bin controller -- --config config/example.deventer.toml --ramp-only --plan-workers 4`
6. Run:
   - `./scripts/run_cluster_ssh.sh config/example.deventer.toml bot1@... bot2@... bot3@... bot4@...`
7. Collect + inspect results:
   - `results/deventer/<run_id>/`

## Configuration

Example config (`config/example.local.toml`):

```toml
[scenario]
seed = 42
memo = "ippan-load-test"

[target]
rpc_urls = ["http://127.0.0.1:8080"]
timeout_ms = 3000
max_in_flight = 2000

[payment]
from = "@loadbank.ipn"
to_list_path = "keys/recipients.json"
amount_atomic = 1
fee_atomic = 2000
signing_key = "TEST_ONLY_KEY"

[ramp]
steps = [
  { tps = 1000, hold_ms = 120000 },
  { tps = 5000, hold_ms = 120000 }
]
```

### Configuration Fields

#### `[scenario]`

- `seed` (u64): Deterministic seed for reproducible address selection
- `memo` (string): Memo field for transactions (truncated to 256 bytes)

#### `[ramp]`

Each step has:
- `tps` (u64): Target transactions per second for this step
- `hold_ms` (u64): How long to maintain this rate

#### `[target]`

- `rpc_urls`: List of RPC endpoints (worker will round-robin)
- `timeout_ms`: Request timeout
- `max_in_flight`: Maximum concurrent pending requests

#### `[payment]`

- `from`: Source address for payments
- `to_list_path`: Path to JSON file with destination addresses (round-robin)
- `amount_atomic`: Payment amount (u128)
- `fee_atomic`: Mandatory transaction fee (u128)
- `signing_key`: Signing key for custodial signing (**use test keys only!**)

## Results Format

Worker results are written as JSON:

```json
{
  "worker_id": "worker-1",
  "timestamp": "2025-12-16T10:30:00Z",
  "duration_ms": 60000,
  "attempted": 600000,
  "sent": 599800,
  "accepted": 599500,
  "rejected": 200,
  "backpressure_429": 50,
  "backpressure_503": 10,
  "errors": 100,
  "timeouts": 0,
  "latency_p50_ms": 45,
  "latency_p95_ms": 120,
  "latency_p99_ms": 250,
  "achieved_tps": 9991
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
│   ├── bots-core/       # Shared types, rate limiter, ramp planner, stats
│   ├── worker/          # Load generator binary
│   ├── controller/      # Orchestration binary
│   ├── preflight/       # DNS/TLS/API health checks for rpc_urls
│   ├── prefund/         # Prefund estimator (fees enabled)
│   └── keygen/          # Key generation utility (optional)
├── config/              # Example TOML configurations
├── scripts/             # Helper scripts for running tests
└── results/             # Output directory for run results
```

## API Endpoint Details

### POST `/tx/payment`

Expected request body:

```json
{
  "from": "source_address",
  "to": "destination_address",
  "amount": "1000",
  "signing_key": "your_key",
  "fee": "10",           // optional
  "nonce": 123,          // optional
  "memo": "test"         // optional, max 256 bytes
}
```

Success response should include:
- HTTP 200 status
- JSON body with `tx_hash` field

Error response:
- Non-2xx status or JSON body with `code`/`error`/`message` fields

## Next Steps: Tuning

To push higher TPS on a single bot server, tune:

- Linux ulimits (`ulimit -n 65536`)
- TCP settings (`net.ipv4.tcp_tw_reuse`, `net.core.somaxconn`)
- `max_in_flight` parameter
- reqwest connection pool size
- Multiple worker processes

## License

MIT License - see LICENSE file for details.
