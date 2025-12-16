use anyhow::{Context, Result};
use bots_core::{Config, RampPlanner, StatsCollector, ToMode};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "worker")]
#[command(about = "IPPAN load rig worker - POST /tx/payment at a controlled ramp rate")]
struct Args {
    /// Path to configuration file
    #[arg(long, default_value = "config/example.local.toml")]
    config: PathBuf,

    /// Submission mode: http or mock
    #[arg(long, default_value = "http")]
    mode: String,

    /// Worker ID (used in logs + results filename)
    #[arg(long, default_value = "worker-1")]
    worker_id: String,

    /// Print stats every N milliseconds
    #[arg(long, default_value = "1000")]
    print_every_ms: u64,

    /// Optional run id (timestamp-like). If not set, worker uses current time.
    #[arg(long)]
    run_id: Option<String>,
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

    let run_id = args
        .run_id
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string());

    info!(
        "Starting worker '{}' (run_id={}) in {} mode",
        args.worker_id, run_id, args.mode
    );
    info!("Seed: {}", config.scenario.seed);
    info!(
        "Payload bytes: {}, max_in_flight: {}, target_urls: {}",
        config.scenario.payload_bytes,
        config.target.max_in_flight,
        config.target.rpc_urls.len()
    );

    let result = run_load_test(
        config.clone(),
        args.mode.clone(),
        args.worker_id.clone(),
        args.print_every_ms,
    )
    .await?;

    std::fs::create_dir_all("results").ok();
    let output_path = format!("results/worker_{}_{}.json", args.worker_id, run_id);
    std::fs::write(&output_path, serde_json::to_string_pretty(&result)?)?;

    info!("Results written to {}", output_path);
    print_summary(&result);
    Ok(())
}

async fn run_load_test(
    config: Config,
    mode: String,
    worker_id: String,
    print_every_ms: u64,
) -> Result<WorkerResult> {
    let start_time = std::time::Instant::now();
    let stats = Arc::new(StatsCollector::new());

    let amount: u128 = config.payment.amount.parse().with_context(|| {
        format!(
            "payment.amount must be a base-10 u128 string, got {:?}",
            config.payment.amount
        )
    })?;

    let config = Arc::new(config);
    let recipients = Arc::new(load_recipients(config.as_ref())?);
    info!(
        "Recipients loaded: {} (to_mode={:?})",
        recipients.len(),
        config.payment.to_mode
    );

    let url_idx = Arc::new(AtomicUsize::new(0));
    let to_idx = Arc::new(AtomicUsize::new(0));

    let submitter: Arc<dyn PaymentSubmitter> = match mode.as_str() {
        "mock" => Arc::new(MockPaymentSubmitter::new(5)),
        "http" => Arc::new(
            HttpPaymentSubmitter::new(config.target.timeout_ms, config.target.max_in_flight)
                .context("Failed to create HTTP submitter")?,
        ),
        _ => anyhow::bail!("Invalid mode: {mode}, must be 'http' or 'mock'"),
    };

    let semaphore = Arc::new(Semaphore::new(config.target.max_in_flight as usize));

    let planner = RampPlanner::new(config.ramp.clone());
    let windows = planner.plan(start_time);
    info!(
        "Planned {} ramp steps, total duration: {}ms",
        windows.len(),
        planner.total_duration_ms()
    );

    let stats_for_log = stats.clone();
    let log_task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(print_every_ms.max(1)));
        loop {
            ticker.tick().await;
            let s = stats_for_log.summary();
            let elapsed_ms = s.duration_ms.max(1);
            let tps = (s.accepted.saturating_mul(1000)) / elapsed_ms;
            info!(
                "progress attempted={} sent={} ok={} rej={} err={} to={} tps={} p50={}ms p95={}ms p99={}ms",
                s.attempted,
                s.sent,
                s.accepted,
                s.rejected,
                s.errors,
                s.timeouts,
                tps,
                s.latency_p50_ms,
                s.latency_p95_ms,
                s.latency_p99_ms
            );
        }
    });

    for window in windows {
        let hold_ms = window.end.duration_since(window.start).as_millis() as u64;
        info!(
            "Starting step {}: {} TPS for {}ms",
            window.step_idx, window.tps, hold_ms
        );

        let env = StepEnv {
            config: config.clone(),
            recipients: recipients.clone(),
            submitter: submitter.clone(),
            stats: stats.clone(),
            semaphore: semaphore.clone(),
            url_idx: url_idx.clone(),
            to_idx: to_idx.clone(),
            amount,
        };

        run_ramp_step(&env, window.tps, hold_ms).await?;
    }

    info!("Waiting for in-flight requests to complete...");
    let _ = semaphore.acquire_many(config.target.max_in_flight).await?;

    log_task.abort();

    let summary = stats.summary();
    let elapsed_ms = start_time.elapsed().as_millis() as u64;
    let achieved_tps = if elapsed_ms > 0 {
        (summary.accepted * 1000) / elapsed_ms
    } else {
        0
    };

    Ok(WorkerResult {
        worker_id,
        timestamp: chrono::Utc::now().to_rfc3339(),
        seed: config.scenario.seed,
        mode,
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

#[derive(Debug, Serialize)]
struct PaymentBody {
    from: String,
    to: String,
    amount: u128,
    signing_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    fee: Option<u128>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nonce: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memo: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct SubmitOutcome {
    kind: OutcomeKind,
    latency_ms: u64,
}

#[derive(Debug, Clone, Copy)]
enum OutcomeKind {
    Accepted,
    Rejected,
    Timeout,
}

trait PaymentSubmitter: Send + Sync {
    fn submit_payment<'a>(
        &'a self,
        base_url: &'a str,
        body: &'a PaymentBody,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SubmitOutcome>> + Send + 'a>>;
}

struct MockPaymentSubmitter {
    delay_ms: u64,
}

impl MockPaymentSubmitter {
    fn new(delay_ms: u64) -> Self {
        Self { delay_ms }
    }
}

impl PaymentSubmitter for MockPaymentSubmitter {
    fn submit_payment<'a>(
        &'a self,
        _base_url: &'a str,
        _body: &'a PaymentBody,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SubmitOutcome>> + Send + 'a>>
    {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
            Ok(SubmitOutcome {
                kind: OutcomeKind::Accepted,
                latency_ms: self.delay_ms,
            })
        })
    }
}

struct HttpPaymentSubmitter {
    client: reqwest::Client,
    timeout_ms: u64,
}

impl HttpPaymentSubmitter {
    fn new(timeout_ms: u64, max_in_flight: u32) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(timeout_ms))
            .tcp_keepalive(Duration::from_secs(60))
            .pool_idle_timeout(Duration::from_secs(60))
            .pool_max_idle_per_host(max_in_flight as usize)
            .build()?;
        Ok(Self { client, timeout_ms })
    }

    fn endpoint(base_url: &str) -> String {
        let base = base_url.trim_end_matches('/');
        format!("{base}/tx/payment")
    }
}

impl PaymentSubmitter for HttpPaymentSubmitter {
    fn submit_payment<'a>(
        &'a self,
        base_url: &'a str,
        body: &'a PaymentBody,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SubmitOutcome>> + Send + 'a>>
    {
        Box::pin(async move {
            let start = std::time::Instant::now();
            let endpoint = Self::endpoint(base_url);

            let resp = self.client.post(endpoint).json(body).send().await;
            match resp {
                Ok(r) => {
                    let latency_ms = start.elapsed().as_millis() as u64;
                    let status = r.status();

                    if status.is_success() {
                        let is_200 = status.as_u16() == 200;

                        // Best-effort parse: treat `{code,message}` as rejection, `tx_hash` as accept.
                        let parsed: Option<serde_json::Value> = r.json().await.ok();
                        let has_tx_hash = parsed
                            .as_ref()
                            .and_then(|v| v.get("tx_hash"))
                            .and_then(|v| v.as_str())
                            .is_some();
                        let looks_like_error =
                            parsed.as_ref().and_then(|v| v.get("code")).is_some()
                                && parsed.as_ref().and_then(|v| v.get("message")).is_some();

                        if has_tx_hash || (is_200 && !looks_like_error) {
                            Ok(SubmitOutcome {
                                kind: OutcomeKind::Accepted,
                                latency_ms,
                            })
                        } else if is_200 {
                            // Spec: HTTP 200 counts as success.
                            Ok(SubmitOutcome {
                                kind: OutcomeKind::Accepted,
                                latency_ms,
                            })
                        } else {
                            Ok(SubmitOutcome {
                                kind: OutcomeKind::Rejected,
                                latency_ms,
                            })
                        }
                    } else {
                        Ok(SubmitOutcome {
                            kind: OutcomeKind::Rejected,
                            latency_ms,
                        })
                    }
                }
                Err(e) if e.is_timeout() => Ok(SubmitOutcome {
                    kind: OutcomeKind::Timeout,
                    latency_ms: self.timeout_ms,
                }),
                Err(e) => Err(e.into()),
            }
        })
    }
}

struct StepEnv {
    config: Arc<Config>,
    recipients: Arc<Vec<String>>,
    submitter: Arc<dyn PaymentSubmitter>,
    stats: Arc<StatsCollector>,
    semaphore: Arc<Semaphore>,
    url_idx: Arc<AtomicUsize>,
    to_idx: Arc<AtomicUsize>,
    amount: u128,
}

async fn run_ramp_step(env: &StepEnv, tps: u64, hold_ms: u64) -> Result<()> {
    let step_start = tokio::time::Instant::now();
    let step_end = step_start + Duration::from_millis(hold_ms);

    let planned_u128 = (tps as u128).saturating_mul(hold_ms as u128) / 1000u128;
    let planned = planned_u128.min(u64::MAX as u128) as u64;
    info!("Planned sends for step: {}", planned);

    let mut scheduled = 0u64;
    let mut seq = 0u64;

    while scheduled < planned {
        let now = tokio::time::Instant::now();
        let elapsed_ms = if now >= step_end {
            hold_ms
        } else {
            now.duration_since(step_start).as_millis() as u64
        };

        let should_have = ((tps as u128).saturating_mul(elapsed_ms as u128) / 1000u128)
            .min(u64::MAX as u128) as u64;

        let mut to_launch = should_have.saturating_sub(scheduled);
        if now >= step_end {
            to_launch = planned.saturating_sub(scheduled);
        }

        while to_launch > 0 {
            let permit = env.semaphore.clone().acquire_owned().await?;
            env.stats.record_attempted(1);
            env.stats.record_sent(1);

            let base_url = choose_round_robin(&env.config.target.rpc_urls, &env.url_idx);
            let to = choose_recipient(
                env.config.payment.to_mode,
                env.recipients.as_ref(),
                &env.to_idx,
            );
            let memo = build_memo(
                &env.config.scenario.memo,
                env.config.scenario.payload_bytes,
                env.config.scenario.seed,
                seq,
            );

            let body = PaymentBody {
                from: env.config.payment.from.clone(),
                to,
                amount: env.amount,
                signing_key: env.config.payment.signing_key.clone(),
                fee: None,
                nonce: None,
                memo: Some(memo),
            };

            let submitter = env.submitter.clone();
            let stats = env.stats.clone();
            tokio::spawn(async move {
                let _permit: OwnedSemaphorePermit = permit;
                match submitter.submit_payment(&base_url, &body).await {
                    Ok(outcome) => match outcome.kind {
                        OutcomeKind::Accepted => stats.record_accepted(1, outcome.latency_ms),
                        OutcomeKind::Rejected => stats.record_rejected(1, outcome.latency_ms),
                        OutcomeKind::Timeout => stats.record_timeout(1, outcome.latency_ms),
                    },
                    Err(e) => {
                        warn!("request error: {}", e);
                        stats.record_error(1);
                    }
                }
            });

            scheduled = scheduled.saturating_add(1);
            seq = seq.saturating_add(1);
            to_launch -= 1;
        }

        if scheduled >= planned {
            break;
        }

        if now < step_end {
            let next_tick = step_start + Duration::from_millis(elapsed_ms.saturating_add(1));
            tokio::time::sleep_until(next_tick).await;
        } else {
            tokio::task::yield_now().await;
        }
    }

    Ok(())
}

fn load_recipients(config: &Config) -> Result<Vec<String>> {
    let path = config
        .payment
        .to_list_path
        .as_deref()
        .context("payment.to_list_path is required (newline-delimited recipients)")?;

    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read recipients list at {path:?}"))?;

    let mut out = Vec::new();
    for line in content.lines() {
        let s = line.trim();
        if s.is_empty() || s.starts_with('#') {
            continue;
        }
        out.push(s.to_string());
    }

    if out.is_empty() {
        anyhow::bail!("Recipient list at {path:?} is empty");
    }

    Ok(out)
}

fn choose_round_robin(items: &[String], idx: &AtomicUsize) -> String {
    let i = idx.fetch_add(1, Ordering::Relaxed);
    items[i % items.len()].clone()
}

fn choose_recipient(mode: ToMode, recipients: &[String], idx: &AtomicUsize) -> String {
    match mode {
        ToMode::Single => recipients[0].clone(),
        ToMode::RoundRobin => {
            let i = idx.fetch_add(1, Ordering::Relaxed);
            recipients[i % recipients.len()].clone()
        }
    }
}

fn build_memo(base: &str, payload_bytes: u32, seed: u64, seq: u64) -> String {
    // Memo is capped to 256 bytes; payload_bytes beyond that has no effect.
    let target = (payload_bytes as usize).clamp(1, 256);

    // Deterministic, cheap, and stable: base + `|seed=<seed>|seq=<seq>|` then 'a' padding.
    let mut s = String::new();
    s.push_str(base);
    if !s.is_empty() {
        s.push('|');
    }
    s.push_str("seed=");
    s.push_str(&seed.to_string());
    s.push_str("|seq=");
    s.push_str(&seq.to_string());

    while s.as_bytes().len() < target {
        s.push('a');
    }

    if s.as_bytes().len() > 256 {
        s.truncate(256);
    }

    s
}

fn print_summary(result: &WorkerResult) {
    println!("\n=== Worker {} Summary ===", result.worker_id);
    println!("Duration: {}ms", result.duration_ms);
    println!("Seed: {}", result.seed);
    println!("Mode: {}", result.mode);
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
