use anyhow::{Context, Result};
use bots_core::{
    Config, HttpJsonSubmitter, MockSubmitter, PaymentTx, RampPlanner, RateLimiter, StatsCollector,
    SubmitResult, TxSubmitter,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{mpsc, Semaphore};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "worker")]
#[command(about = "IPPAN load test worker - generates and sends transactions")]
struct Args {
    /// Path to configuration file
    #[arg(long, default_value = "config/example.local.toml")]
    config: PathBuf,

    /// Submission mode: mock or http
    #[arg(long, default_value = "mock")]
    mode: String,

    /// Worker ID (overrides config)
    #[arg(long)]
    worker_id: Option<String>,

    /// Print stats every N milliseconds
    #[arg(long, default_value = "1000")]
    print_every_ms: u64,
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
    let mut config = Config::from_file(&args.config)
        .with_context(|| format!("Failed to load config from {:?}", args.config))?;

    // Override worker ID if provided
    if let Some(worker_id) = args.worker_id {
        config.worker.id = worker_id;
    }

    info!(
        "Starting worker '{}' in {} mode",
        config.worker.id, args.mode
    );
    info!("Seed: {}", config.scenario.seed);
    info!("Payment from: {}", config.payment.from);
    info!("Payment to_mode: {}", config.payment.to_mode);
    info!("Payment amount: {}", config.payment.amount);

    // Create submitter based on mode
    let submitter: Arc<dyn TxSubmitter> = match args.mode.as_str() {
        "mock" => Arc::new(MockSubmitter::new(5)), // 5ms simulated latency
        "http" => Arc::new(
            HttpJsonSubmitter::new(config.target.rpc_urls.clone(), config.target.timeout_ms)
                .context("Failed to create HTTP submitter")?,
        ),
        _ => anyhow::bail!("Invalid mode: {}, must be 'mock' or 'http'", args.mode),
    };

    info!("Using submitter: {}", submitter.name());

    // Load destination addresses
    let to_addresses = load_to_addresses(&config)?;
    info!("Loaded {} destination addresses", to_addresses.len());

    // Run the load test
    let result =
        run_load_test(config.clone(), submitter, to_addresses, args.print_every_ms).await?;

    // Write results to file
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let output_path = format!("results/worker_{}_{}.json", config.worker.id, timestamp);

    std::fs::create_dir_all("results").ok();
    let result_json = serde_json::to_string_pretty(&result)?;
    std::fs::write(&output_path, result_json)?;

    info!("Results written to {}", output_path);
    print_summary(&result);

    Ok(())
}

fn load_to_addresses(config: &Config) -> Result<Vec<String>> {
    match config.payment.to_mode.as_str() {
        "single" => {
            let to_addr = config
                .payment
                .to_single
                .as_ref()
                .context("to_single required for single mode")?;
            Ok(vec![to_addr.clone()])
        }
        "round_robin" => {
            let path = config
                .payment
                .to_list_path
                .as_ref()
                .context("to_list_path required for round_robin mode")?;
            let contents = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read to_list_path: {}", path))?;
            let addresses: Vec<String> = contents
                .lines()
                .map(|s: &str| s.trim().to_string())
                .filter(|s: &String| !s.is_empty())
                .collect();
            if addresses.is_empty() {
                anyhow::bail!("to_list_path file is empty: {}", path);
            }
            Ok(addresses)
        }
        other => anyhow::bail!(
            "Invalid to_mode: {}, must be 'single' or 'round_robin'",
            other
        ),
    }
}

async fn run_load_test(
    config: Config,
    submitter: Arc<dyn TxSubmitter>,
    to_addresses: Vec<String>,
    print_every_ms: u64,
) -> Result<WorkerResult> {
    let start_time = Instant::now();
    let mut stats = StatsCollector::new();

    // Create ramp planner
    let planner = RampPlanner::new(config.ramp.clone());
    let windows = planner.plan(start_time);

    info!(
        "Planned {} ramp steps, total duration: {}ms",
        windows.len(),
        planner.total_duration_ms()
    );

    // Create semaphore for max in-flight control
    let semaphore = Arc::new(Semaphore::new(config.target.max_in_flight as usize));

    // Create channel for collecting results
    let (result_tx, mut result_rx) = mpsc::channel::<SubmitResult>(10000);

    // Track last print time
    let mut last_print = Instant::now();

    // Spawn stats collector task
    let stats_handle = {
        let mut stats_clone = stats.clone();
        tokio::spawn(async move {
            while let Some(result) = result_rx.recv().await {
                stats_clone.record_sent(1);
                stats_clone.record_success(
                    result.accepted,
                    result.rejected,
                    result.timeouts,
                    result.latency_ms,
                );
            }
            stats_clone
        })
    };

    // Truncate memo to 256 bytes
    let memo = if config.scenario.memo.len() > 256 {
        config.scenario.memo[..256].to_string()
    } else {
        config.scenario.memo.clone()
    };
    let memo = if memo.is_empty() { None } else { Some(memo) };

    // Execute ramp plan
    for window in windows {
        info!(
            "Starting step {}: {} TPS for {}ms",
            window.step_idx,
            window.tps,
            window.end.duration_since(window.start).as_millis()
        );

        let mut rate_limiter = RateLimiter::new(window.tps);
        let mut to_idx = 0;

        // Generate transactions for this window
        while Instant::now() < window.end {
            // Check if we should print stats
            if last_print.elapsed().as_millis() >= print_every_ms as u128 {
                print_progress(&stats);
                last_print = Instant::now();
            }

            // Wait for rate limiter
            rate_limiter.acquire().await;

            // Select destination address (round-robin)
            let to_addr = to_addresses[to_idx % to_addresses.len()].clone();
            to_idx += 1;

            // Create payment transaction
            let tx = PaymentTx {
                from: config.payment.from.clone(),
                to: to_addr,
                amount: config.payment.amount.clone(),
                signing_key: config.payment.signing_key.clone(),
                fee: None,
                nonce: None,
                memo: memo.clone(),
            };

            stats.record_attempted(1);

            // Acquire semaphore permit
            let permit = semaphore.clone().acquire_owned().await?;
            let submitter = submitter.clone();
            let result_tx = result_tx.clone();

            // Submit in background
            tokio::spawn(async move {
                match submitter.submit_payment(&tx).await {
                    Ok(result) => {
                        let _ = result_tx.send(result).await;
                    }
                    Err(e) => {
                        warn!("Submission error: {}", e);
                        // Send error result
                        let _ = result_tx
                            .send(SubmitResult {
                                accepted: 0,
                                rejected: 0,
                                timeouts: 0,
                                latency_ms: 0,
                            })
                            .await;
                    }
                }
                drop(permit);
            });
        }
    }

    // Wait for in-flight requests to complete
    info!("Waiting for in-flight requests to complete...");
    let _ = semaphore.acquire_many(config.target.max_in_flight).await?;

    // Close result channel and wait for stats collector
    drop(result_tx);
    stats = stats_handle.await?;

    let summary = stats.summary();
    let elapsed_ms = start_time.elapsed().as_millis() as u64;
    let achieved_tps = if elapsed_ms > 0 {
        (summary.accepted * 1000) / elapsed_ms
    } else {
        0
    };

    Ok(WorkerResult {
        worker_id: config.worker.id.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        duration_ms: elapsed_ms,
        attempted: summary.attempted,
        sent: summary.sent,
        accepted: summary.accepted,
        rejected: summary.rejected,
        errors: summary.errors,
        timeouts: summary.timeouts,
        latency_p50_ms: summary.latency_p50_ms,
        latency_p95_ms: summary.latency_p95_ms,
        latency_p99_ms: summary.latency_p99_ms,
        achieved_tps,
    })
}

fn print_progress(stats: &StatsCollector) {
    let summary = stats.summary();
    let elapsed_s = summary.duration_ms / 1000;
    let tps = if elapsed_s > 0 {
        summary.accepted / elapsed_s
    } else {
        0
    };

    info!(
        "Progress: attempted={} sent={} accepted={} rejected={} errors={} tps={} p50={}ms p95={}ms p99={}ms",
        summary.attempted,
        summary.sent,
        summary.accepted,
        summary.rejected,
        summary.errors,
        tps,
        summary.latency_p50_ms,
        summary.latency_p95_ms,
        summary.latency_p99_ms
    );
}

fn print_summary(result: &WorkerResult) {
    println!("\n=== Worker {} Summary ===", result.worker_id);
    println!("Duration: {}ms", result.duration_ms);
    println!("Attempted: {}", result.attempted);
    println!("Sent: {}", result.sent);
    println!("Accepted: {}", result.accepted);
    println!("Rejected: {}", result.rejected);
    println!("Errors: {}", result.errors);
    println!("Timeouts: {}", result.timeouts);
    println!("Achieved TPS: {}", result.achieved_tps);
    println!("Latency p50: {}ms", result.latency_p50_ms);
    println!("Latency p95: {}ms", result.latency_p95_ms);
    println!("Latency p99: {}ms", result.latency_p99_ms);
    println!();
}
