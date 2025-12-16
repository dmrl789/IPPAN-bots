use anyhow::{Context, Result};
use bots_core::{Config, RampPlanner};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "controller")]
#[command(about = "IPPAN load test controller - orchestrates multiple workers")]
struct Args {
    /// Path to configuration file
    #[arg(long, default_value = "config/example.cluster.toml")]
    config: PathBuf,

    /// Only print the ramp schedule without running
    #[arg(long)]
    ramp_only: bool,

    /// Dry run: print what would be executed without running
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkerResult {
    worker_id: String,
    timestamp: String,
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
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    // Load configuration
    let config = Config::from_file(&args.config)
        .with_context(|| format!("Failed to load config from {:?}", args.config))?;

    // Create ramp planner and display schedule
    let planner = RampPlanner::new(config.ramp.clone());
    print_ramp_schedule(&planner);

    if args.ramp_only {
        return Ok(());
    }

    // Check if controller config exists
    let controller_config = config
        .controller
        .as_ref()
        .context("Controller configuration is required")?;

    info!(
        "Controller will orchestrate {} workers",
        controller_config.worker_hosts.len()
    );

    if args.dry_run {
        print_dry_run(&config, controller_config);
        return Ok(());
    }

    // Note: Full SSH orchestration would be implemented here
    // For now, we provide a script-based approach (see scripts/run_cluster_ssh.sh)
    info!("Note: Use scripts/run_cluster_ssh.sh for SSH-based orchestration");
    info!("Controller binary can be extended to handle SSH orchestration directly");

    // Simulate collecting results (in real implementation, would collect from workers)
    let results = collect_worker_results(controller_config).await?;

    if !results.is_empty() {
        // Merge and save results
        let merged = merge_results(results);
        save_merged_results(&merged)?;
        print_merged_summary(&merged);
    } else {
        info!("No worker results found. Run workers first.");
    }

    Ok(())
}

fn print_ramp_schedule(planner: &RampPlanner) {
    println!("\n=== Ramp Schedule ===");
    println!("Total duration: {}ms", planner.total_duration_ms());
    println!();

    let start = std::time::Instant::now();
    let windows = planner.plan(start);

    for window in windows {
        let duration_ms = window.end.duration_since(window.start).as_millis();
        println!(
            "Step {}: {} TPS for {}ms",
            window.step_idx, window.tps, duration_ms
        );
    }
    println!();
}

fn print_dry_run(config: &Config, controller_config: &bots_core::ControllerConfig) {
    println!("\n=== Dry Run ===");
    println!("Worker hosts: {:?}", controller_config.worker_hosts);
    println!("RPC targets: {:?}", config.target.rpc_urls);
    println!("Max in-flight: {}", config.target.max_in_flight);
    println!("Batch max: {}", config.scenario.batch_max);
    println!("Payload bytes: {}", config.scenario.payload_bytes);
    println!("Seed: {}", config.scenario.seed);
    println!("\nWould execute:");

    for (idx, host) in controller_config.worker_hosts.iter().enumerate() {
        println!(
            "  ssh {} 'cd /path/to/worker && ./worker --worker-id worker-{} --config config.toml --mode http'",
            host, idx
        );
    }
    println!();
}

async fn collect_worker_results(
    _controller_config: &bots_core::ControllerConfig,
) -> Result<Vec<WorkerResult>> {
    let mut results = Vec::new();

    // Look for worker result files in results/ directory
    let results_dir = PathBuf::from("results");
    if !results_dir.exists() {
        return Ok(results);
    }

    let entries = std::fs::read_dir(&results_dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) == Some("json")
            && path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.starts_with("worker_"))
                .unwrap_or(false)
        {
            info!("Found worker result: {:?}", path);
            let content = std::fs::read_to_string(&path)?;
            let result: WorkerResult = serde_json::from_str(&content)?;
            results.push(result);
        }
    }

    Ok(results)
}

fn merge_results(workers: Vec<WorkerResult>) -> MergedResult {
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
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let output_path = format!("results/run_{}_merged.json", timestamp);

    std::fs::create_dir_all("results").ok();
    let result_json = serde_json::to_string_pretty(&merged)?;
    std::fs::write(&output_path, result_json)?;

    info!("Merged results written to {}", output_path);
    Ok(())
}

fn print_merged_summary(merged: &MergedResult) {
    println!("\n=== Merged Results ({} workers) ===", merged.worker_count);
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

    println!("=== Per-Worker Breakdown ===");
    for worker in &merged.workers {
        println!(
            "  {}: accepted={} tps={} p95={}ms",
            worker.worker_id, worker.accepted, worker.achieved_tps, worker.latency_p95_ms
        );
    }
    println!();
}
