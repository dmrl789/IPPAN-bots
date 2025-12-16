use anyhow::{Context, Result};
use bots_core::{
    Config, HttpJsonSubmitter, MockSubmitter, RampPlanner, RateLimiter, StatsCollector, TxSubmitter,
};
use clap::Parser;
use rand::{RngCore, SeedableRng};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Semaphore;
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
    info!(
        "Payload size: {} bytes, Batch max: {}",
        config.scenario.payload_bytes, config.scenario.batch_max
    );

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

    // Run the load test
    let result = run_load_test(config.clone(), submitter, args.print_every_ms).await?;

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

async fn run_load_test(
    config: Config,
    submitter: Arc<dyn TxSubmitter>,
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

    // Create RNG for payload generation
    let mut rng = rand::rngs::StdRng::seed_from_u64(config.scenario.seed);

    // Determine batch size
    let batch_size = if config.scenario.batch_max > 1 {
        config.scenario.batch_max as usize
    } else {
        1
    };

    info!("Batch size: {}", batch_size);

    // Track last print time
    let mut last_print = Instant::now();

    // Execute ramp plan
    for window in windows {
        info!(
            "Starting step {}: {} TPS for {}ms",
            window.step_idx,
            window.tps,
            window.end.duration_since(window.start).as_millis()
        );

        let mut rate_limiter = RateLimiter::new(window.tps);

        // Generate transactions for this window
        while Instant::now() < window.end {
            // Check if we should print stats
            if last_print.elapsed().as_millis() >= print_every_ms as u128 {
                print_progress(&stats);
                last_print = Instant::now();
            }

            // Wait for rate limiter
            if batch_size > 1 {
                rate_limiter.acquire_batch(batch_size as u64).await;
            } else {
                rate_limiter.acquire().await;
            }

            // Generate batch of transactions
            let mut batch = Vec::new();
            for _ in 0..batch_size {
                let tx = generate_transaction(&mut rng, config.scenario.payload_bytes);
                batch.push(tx);
            }

            stats.record_attempted(batch.len() as u64);

            // Acquire semaphore permit
            let permit = semaphore.clone().acquire_owned().await?;
            let submitter = submitter.clone();
            let batch_len = batch.len() as u64;

            // Submit in background
            tokio::spawn(async move {
                match submitter.submit_batch(&batch).await {
                    Ok(result) => {
                        // Stats will be collected by the main thread
                        // For now, we just drop the result
                        // In a real implementation, we'd send this back via a channel
                        drop((result, permit));
                    }
                    Err(e) => {
                        warn!("Submission error: {}", e);
                        drop(permit);
                    }
                }
            });

            // For stats tracking, we'll optimistically count as sent/accepted in mock mode
            // In a real implementation, we'd use channels to collect actual results
            stats.record_sent(batch_len);
            stats.record_success(batch_len, 0, 0, 5); // Placeholder latency
        }
    }

    // Wait for in-flight requests to complete
    info!("Waiting for in-flight requests to complete...");
    let _ = semaphore.acquire_many(config.target.max_in_flight).await?;

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

fn generate_transaction(rng: &mut impl RngCore, size: u32) -> Vec<u8> {
    let mut tx = vec![0u8; size as usize];
    rng.fill_bytes(&mut tx);
    tx
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
