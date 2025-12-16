use anyhow::{Context, Result};
use bots_core::{Config, RampPlanner};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "controller")]
#[command(about = "IPPAN load rig controller - spawns workers locally and merges results")]
struct Args {
    /// Path to configuration file
    #[arg(long, default_value = "config/example.local.toml")]
    config: PathBuf,

    /// Dry run: print what would be executed without running
    #[arg(long)]
    dry_run: bool,

    /// Spawn N local worker processes
    #[arg(long = "local-workers", alias = "local", default_value_t = 0)]
    local_workers: u32,

    /// Only print the ramp schedule without running
    #[arg(long)]
    ramp_only: bool,

    /// Merge an existing run's worker result JSONs without spawning workers.
    #[arg(long)]
    merge_run_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkerResult {
    worker_id: String,
    timestamp: String,
    seed: u64,
    mode: String,
    duration_ms: u64,
    attempted: u64,
    sent: u64,
    accepted: u64,
    rejected: u64,
    errors: u64,
    timeouts: u64,
    latency_p50_ms: u64,
    latency_p95_ms: u64,
    latency_p99_ms: u64,
    achieved_tps: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct MergedResult {
    timestamp: String,
    run_id: String,
    worker_count: usize,
    total_duration_ms: u64,
    total_attempted: u64,
    total_sent: u64,
    total_accepted: u64,
    total_rejected: u64,
    total_errors: u64,
    total_timeouts: u64,
    avg_latency_p50_ms: u64,
    avg_latency_p95_ms: u64,
    avg_latency_p99_ms: u64,
    total_achieved_tps: u64,
    workers: Vec<WorkerResult>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    let config = Config::from_file(&args.config)
        .with_context(|| format!("Failed to load config from {:?}", args.config))?;

    let planner = RampPlanner::new(config.ramp.clone());
    print_ramp_schedule(&planner);

    if args.ramp_only {
        return Ok(());
    }

    let local_n = args.local_workers;
    if local_n == 0 {
        if let Some(run_id) = args.merge_run_id.as_deref() {
            let workers = collect_worker_results(run_id)?;
            if workers.is_empty() {
                anyhow::bail!("No worker results found for run_id={run_id}");
            }
            let merged = merge_results(run_id.to_string(), workers);
            save_merged_results(&merged)?;
            print_merged_summary(&merged);
            return Ok(());
        }

        info!("No --local-workers N provided. For SSH-based orchestration, use scripts/run_cluster_ssh.sh");
        return Ok(());
    }

    let run_id = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let worker_bin = infer_worker_binary().context("Failed to infer worker binary path")?;

    if args.dry_run {
        print_dry_run(&worker_bin, &args.config, local_n, &run_id);
        return Ok(());
    }

    info!(
        "Spawning {} local workers using {:?} (run_id={})",
        local_n, worker_bin, run_id
    );

    std::fs::create_dir_all("results").ok();
    std::fs::create_dir_all("runs").ok();

    let mut handles = Vec::new();
    for i in 0..local_n {
        let worker_id = format!("worker-{}", i);
        let worker_cfg_path =
            write_worker_config(&config, local_n, i, &run_id).context("write worker config")?;

        let mut cmd = Command::new(&worker_bin);
        cmd.arg("--config")
            .arg(&worker_cfg_path)
            .arg("--mode")
            .arg("http")
            .arg("--worker-id")
            .arg(&worker_id)
            .arg("--run-id")
            .arg(&run_id)
            .arg("--print-every-ms")
            .arg("1000")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn worker {worker_id}"))?;

        handles.push(tokio::spawn(async move {
            let status = child.wait().await?;
            if !status.success() {
                anyhow::bail!("Worker {worker_id} exited with status {status}");
            }
            Ok::<(), anyhow::Error>(())
        }));
    }

    for h in handles {
        h.await??;
    }

    let workers = collect_worker_results(&run_id)?;
    if workers.is_empty() {
        anyhow::bail!("No worker results found for run_id={run_id}");
    }

    let merged = merge_results(run_id.clone(), workers);
    save_merged_results(&merged)?;
    print_merged_summary(&merged);

    Ok(())
}

fn print_ramp_schedule(planner: &RampPlanner) {
    println!("\n=== Ramp Schedule ===");
    println!("Total duration: {}ms", planner.total_duration_ms());
    println!();

    for (idx, step) in planner.steps().iter().enumerate() {
        println!("Step {}: {} TPS for {}ms", idx, step.tps, step.hold_ms);
    }
    println!();
}

fn print_dry_run(worker_bin: &Path, config: &Path, local_n: u32, run_id: &str) {
    println!("\n=== Dry Run ===");
    println!("worker_bin: {:?}", worker_bin);
    println!("config: {:?}", config);
    println!("local workers: {}", local_n);
    println!("run_id: {}", run_id);
    println!("\nWould execute:");

    for i in 0..local_n {
        println!(
            "  {:?} --config {:?} --mode http --worker-id worker-{} --run-id {} --print-every-ms 1000",
            worker_bin, config, i, run_id
        );
    }
    println!();
}

fn split_tps(total: u64, workers: u32, worker_idx: u32) -> u64 {
    if workers == 0 {
        return 0;
    }
    let base = total / workers as u64;
    let rem = total % workers as u64;
    if (worker_idx as u64) < rem {
        base.saturating_add(1)
    } else {
        base
    }
}

fn write_worker_config(
    base: &Config,
    workers: u32,
    worker_idx: u32,
    run_id: &str,
) -> Result<PathBuf> {
    let mut cfg = base.clone();
    for step in &mut cfg.ramp.steps {
        step.tps = split_tps(step.tps, workers, worker_idx);
    }

    let out = PathBuf::from(format!("runs/run_{run_id}_worker_{worker_idx}.toml"));
    let s = toml::to_string_pretty(&cfg)?;
    std::fs::write(&out, s)?;
    Ok(out)
}

fn infer_worker_binary() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("WORKER_BIN") {
        return Ok(PathBuf::from(p));
    }

    let exe = std::env::current_exe()?;
    let exe_dir = exe
        .parent()
        .context("current_exe has no parent directory")?;

    // If running via cargo, controller is in target/{debug,release}/controller.
    // Prefer sibling worker in the same directory.
    let candidate = exe_dir.join("worker");
    if candidate.exists() {
        return Ok(candidate);
    }

    for p in ["target/release/worker", "target/debug/worker"] {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return Ok(pb);
        }
    }

    anyhow::bail!("Could not find worker binary. Build it first (cargo build --bin worker).")
}

fn collect_worker_results(run_id: &str) -> Result<Vec<WorkerResult>> {
    let mut results = Vec::new();

    let results_dir = PathBuf::from("results");
    if !results_dir.exists() {
        return Ok(results);
    }

    for entry in std::fs::read_dir(&results_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n,
            None => continue,
        };

        if !name.starts_with("worker_") {
            continue;
        }
        if !name.ends_with(&format!("_{run_id}.json")) {
            continue;
        }

        let content = std::fs::read_to_string(&path)?;
        let result: WorkerResult = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {:?}", path))?;
        results.push(result);
    }

    Ok(results)
}

fn merge_results(run_id: String, workers: Vec<WorkerResult>) -> MergedResult {
    let worker_count = workers.len();

    let total_attempted = workers.iter().map(|w| w.attempted).sum();
    let total_sent = workers.iter().map(|w| w.sent).sum();
    let total_accepted = workers.iter().map(|w| w.accepted).sum();
    let total_rejected = workers.iter().map(|w| w.rejected).sum();
    let total_errors = workers.iter().map(|w| w.errors).sum();
    let total_timeouts = workers.iter().map(|w| w.timeouts).sum();

    let avg_latency_p50_ms = if worker_count > 0 {
        workers.iter().map(|w| w.latency_p50_ms).sum::<u64>() / worker_count as u64
    } else {
        0
    };

    let avg_latency_p95_ms = if worker_count > 0 {
        workers.iter().map(|w| w.latency_p95_ms).sum::<u64>() / worker_count as u64
    } else {
        0
    };

    let avg_latency_p99_ms = if worker_count > 0 {
        workers.iter().map(|w| w.latency_p99_ms).sum::<u64>() / worker_count as u64
    } else {
        0
    };

    let total_duration_ms = workers.iter().map(|w| w.duration_ms).max().unwrap_or(0);
    let total_achieved_tps = workers.iter().map(|w| w.achieved_tps).sum();

    MergedResult {
        timestamp: chrono::Utc::now().to_rfc3339(),
        run_id,
        worker_count,
        total_duration_ms,
        total_attempted,
        total_sent,
        total_accepted,
        total_rejected,
        total_errors,
        total_timeouts,
        avg_latency_p50_ms,
        avg_latency_p95_ms,
        avg_latency_p99_ms,
        total_achieved_tps,
        workers,
    }
}

fn save_merged_results(merged: &MergedResult) -> Result<()> {
    std::fs::create_dir_all("results").ok();
    let output_path = format!("results/run_{}_merged.json", merged.run_id);
    let result_json = serde_json::to_string_pretty(merged)?;
    std::fs::write(&output_path, result_json)?;
    info!("Merged results written to {}", output_path);
    Ok(())
}

fn print_merged_summary(merged: &MergedResult) {
    println!(
        "\n=== Merged Results ({} workers, run_id={}) ===",
        merged.worker_count, merged.run_id
    );
    println!("Total duration: {}ms", merged.total_duration_ms);
    println!("Total attempted: {}", merged.total_attempted);
    println!("Total sent: {}", merged.total_sent);
    println!("Total accepted: {}", merged.total_accepted);
    println!("Total rejected: {}", merged.total_rejected);
    println!("Total errors: {}", merged.total_errors);
    println!("Total timeouts: {}", merged.total_timeouts);
    println!("Total achieved TPS: {}", merged.total_achieved_tps);
    println!("Avg latency p50: {}ms", merged.avg_latency_p50_ms);
    println!("Avg latency p95: {}ms", merged.avg_latency_p95_ms);
    println!("Avg latency p99: {}ms", merged.avg_latency_p99_ms);
    println!();
}
