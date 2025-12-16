use anyhow::{Context, Result};
use bots_core::{Config, RampPlanner, StatsCollector};
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
        "max_in_flight: {}, target_urls: {}",
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

    let config = Arc::new(config);
    let recipients = Arc::new(load_recipients(config.as_ref())?);
    info!("Recipients loaded: {}", recipients.len(),);

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
    info!(
        "Planned {} ramp steps, total duration: {}ms",
        planner.steps().len(),
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

    for (step_idx, step) in planner.steps().iter().enumerate() {
        let hold_ms = step.hold_ms;
        info!(
            "Starting step {}: {} TPS for {}ms",
            step_idx, step.tps, hold_ms
        );

        let env = StepEnv {
            config: config.clone(),
            recipients: recipients.clone(),
            submitter: submitter.clone(),
            stats: stats.clone(),
            semaphore: semaphore.clone(),
            url_idx: url_idx.clone(),
            to_idx: to_idx.clone(),
            from: config.payment.from.clone(),
            signing_key: config.payment.signing_key.clone(),
            memo_base: config.scenario.memo.clone(),
            amount_atomic: config.payment.amount_atomic,
            fee_atomic: config.payment.fee_atomic,
        };

        run_ramp_step(&env, step.tps, hold_ms).await?;
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
    amount: String,
    fee: String,
    signing_key: String,
    memo: String,
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

                    // Spec:
                    // - Success: HTTP 200 + `tx_hash`
                    // - Error: non-2xx OR `{code,message}`
                    let parsed: Option<serde_json::Value> = r.json().await.ok();

                    let has_tx_hash = parsed
                        .as_ref()
                        .and_then(|v| v.get("tx_hash"))
                        .and_then(|v| v.as_str())
                        .is_some();
                    let looks_like_error = parsed.as_ref().and_then(|v| v.get("code")).is_some()
                        && parsed.as_ref().and_then(|v| v.get("message")).is_some();

                    if status.as_u16() == 200 && has_tx_hash && !looks_like_error {
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
    from: String,
    signing_key: String,
    memo_base: String,
    amount_atomic: u128,
    fee_atomic: u128,
}

async fn run_ramp_step(env: &StepEnv, tps: u64, hold_ms: u64) -> Result<()> {
    let mut seq = 0u64;

    let step_start = tokio::time::Instant::now();
    let full_seconds = hold_ms / 1000;
    let tail_ms = hold_ms % 1000;

    for sec in 0..full_seconds {
        let window_start = step_start + Duration::from_millis(sec.saturating_mul(1000));
        launch_window(env, tps, window_start, 1000, &mut seq).await?;
    }

    if tail_ms > 0 {
        let window_start = step_start + Duration::from_millis(full_seconds.saturating_mul(1000));
        launch_window(env, tps, window_start, tail_ms, &mut seq).await?;
    }

    Ok(())
}

fn load_recipients(config: &Config) -> Result<Vec<String>> {
    let path = config.payment.to_list_path.as_str();

    let content =
        std::fs::read_to_string(path).with_context(|| format!("Failed to read {path:?}"))?;

    // Accept either:
    // - `["addr1","addr2", ...]`
    // - `{ "recipients": ["addr1", ...] }`
    let v: serde_json::Value =
        serde_json::from_str(&content).context("recipients.json must be valid JSON")?;
    let arr = if let Some(a) = v.as_array() {
        a
    } else if let Some(a) = v.get("recipients").and_then(|x| x.as_array()) {
        a
    } else {
        anyhow::bail!(
            "recipients.json must be a JSON array or an object with a 'recipients' array"
        );
    };

    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .context("recipient entries must be strings")?
            .trim();
        if s.is_empty() {
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

fn build_memo(base: &str, seed: u64, seq: u64) -> String {
    // Deterministic and stable; capped to 256 bytes.
    let mut s = String::new();
    s.push_str(base);
    if !s.is_empty() {
        s.push('|');
    }
    s.push_str("seed=");
    s.push_str(&seed.to_string());
    s.push_str("|seq=");
    s.push_str(&seq.to_string());
    if s.as_bytes().len() > 256 {
        s.truncate(256);
    }
    s
}

async fn launch_window(
    env: &StepEnv,
    tps: u64,
    window_start: tokio::time::Instant,
    window_ms: u64,
    seq: &mut u64,
) -> Result<()> {
    // Integer scheduler: exactly `tps` per full second.
    // For partial windows (<1000ms), schedules floor(tps * window_ms / 1000).
    let mut launched = 0u64;
    let target_total =
        ((tps as u128).saturating_mul(window_ms as u128) / 1000u128).min(u64::MAX as u128) as u64;

    for ms in 0..window_ms {
        let due = window_start + Duration::from_millis(ms);
        tokio::time::sleep_until(due).await;

        // desired by this millisecond (inclusive), in this window.
        let desired = ((tps as u128).saturating_mul((ms + 1) as u128) / 1000u128)
            .min(target_total as u128) as u64;
        let mut to_launch = desired.saturating_sub(launched);
        while to_launch > 0 {
            let permit = env.semaphore.clone().acquire_owned().await?;
            env.stats.record_attempted(1);
            env.stats.record_sent(1);

            let base_url = choose_round_robin(&env.config.target.rpc_urls, &env.url_idx);
            let to = choose_round_robin(env.recipients.as_ref(), &env.to_idx);
            let memo = build_memo(&env.memo_base, env.config.scenario.seed, *seq);

            let body = PaymentBody {
                from: env.from.clone(),
                to,
                amount: env.amount_atomic.to_string(),
                fee: env.fee_atomic.to_string(),
                signing_key: env.signing_key.clone(),
                memo,
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

            launched = launched.saturating_add(1);
            *seq = seq.saturating_add(1);
            to_launch -= 1;
        }
    }

    // If window_ms < 1000, `target_total` may be > desired at the last ms due to rounding.
    // Launch any remaining deterministically at the end.
    while launched < target_total {
        let permit = env.semaphore.clone().acquire_owned().await?;
        env.stats.record_attempted(1);
        env.stats.record_sent(1);

        let base_url = choose_round_robin(&env.config.target.rpc_urls, &env.url_idx);
        let to = choose_round_robin(env.recipients.as_ref(), &env.to_idx);
        let memo = build_memo(&env.memo_base, env.config.scenario.seed, *seq);

        let body = PaymentBody {
            from: env.from.clone(),
            to,
            amount: env.amount_atomic.to_string(),
            fee: env.fee_atomic.to_string(),
            signing_key: env.signing_key.clone(),
            memo,
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

        launched = launched.saturating_add(1);
        *seq = seq.saturating_add(1);
    }

    Ok(())
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
