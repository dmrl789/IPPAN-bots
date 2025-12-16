## IPPAN distributed load robots (controller + workers)

This repo builds a **deterministic, integer-only** load generator for IPPAN.

- **Worker**: generates placeholder “tx bytes”, rate-limits (no floats), applies backpressure (`max_in_flight`), submits via an adapter, and writes a JSON summary.
- **Controller**: v1 orchestration via SSH + results merging (v2 HTTP control plane is a TODO).
- **RPC adapter**: all IPPAN-specific RPC + tx format/signing are **behind `TxSubmitter`**.
  - `mock` mode works today.
  - `http` mode is a compiling **stub** with a clearly marked TODO for real IPPAN RPC shape.

### Quickstart

#### Local dry-run (print ramp schedule)

```bash
cargo run -p controller -- --config config/example.local.toml --dry-run
```

#### Local worker (mock submitter)

```bash
./scripts/run_local.sh
```

You should see a `results/worker_*.json` file created.

#### Local worker (HTTP stub)

```bash
cargo run -p worker -- --config config/example.local.toml --mode http --worker-id worker-1
```

This will attempt to POST to the configured `target.rpc_urls` with a placeholder endpoint/body.

#### Multi-worker across servers via SSH (v1)

- Build once locally:

```bash
cargo build --release
```

- Ensure SSH access from the controller machine to each host.
- Run:

```bash
./scripts/run_cluster_ssh.sh config/example.cluster.toml
```

This script copies `target/release/worker` to each host, starts it, and pulls back `results/*.json`.

### Config format (TOML)

See `config/example.local.toml` and `config/example.cluster.toml`.

- **scenario**
  - `seed` (u64): deterministic randomness seed (logged).
  - `duration_ms` (optional u64): global cap.
  - `payload_bytes` (u32): fixed payload template size.
  - `batch_max` (u32): `0/1` disables batching, otherwise max batch size.
- **ramp.steps**: each step has `tps` (u64) and `hold_ms` (u64).
- **target**
  - `rpc_urls`: list of RPC base URLs (worker round-robins).
  - `timeout_ms`: request timeout.
  - `max_in_flight`: backpressure limit.
- **worker**
  - `id`: worker identifier.
  - `bind_metrics`: optional metrics bind (currently unused by default).
- **controller**
  - `workers`: list of SSH hosts (v1) for scripts/controller.

### Keys (no secrets committed)

Use the key generator to create deterministic fake keys (placeholders for real signing later):

```bash
cargo run -p keygen -- --out keys/worker-keys.json --count 1000 --seed 123
```

`keys/` is gitignored.

### RPC adapter note

IPPAN RPC details and tx signing/format are currently unknown here, so `HttpJsonSubmitter` is a **stub**.
To plug the real IPPAN endpoint later, implement/adjust the `TxSubmitter` adapter in `crates/bots-core` without refactoring the worker logic.

### CI

GitHub Actions runs:

- `cargo fmt --all --check`
- `cargo clippy --all --all-targets -- -D warnings`
- `cargo test --all`
