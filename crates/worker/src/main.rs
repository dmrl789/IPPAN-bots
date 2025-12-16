use anyhow::{Context, Result};
use bots_core::{Config, LatencyHistogram, RampPlanner};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
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
    backpressure_429: u64,
    backpressure_503: u64,
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
    let config = load_config(&args.config)
        .with_context(|| format!("Failed to load config from {:?}", args.config))?;

    if args.mode == "http" {
        validate_https_only(&config.target.rpc_urls)?;
    }
    validate_fee_nonzero(config.payment.fee_atomic)?;

    let run_id = args.run_id.clone().unwrap_or_else(now_id);

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
    let stats = Arc::new(WorkerStats::new());

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
                "progress attempted={} sent={} ok={} rej={} bp429={} bp503={} err={} to={} tps={} p50={}ms p95={}ms p99={}ms",
                s.attempted,
                s.sent,
                s.accepted,
                s.rejected,
                s.backpressure_429,
                s.backpressure_503,
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
        timestamp: now_id(),
        seed: config.scenario.seed,
        mode,
        duration_ms: elapsed_ms,
        attempted: summary.attempted,
        sent: summary.sent,
        accepted: summary.accepted,
        rejected: summary.rejected,
        backpressure_429: summary.backpressure_429,
        backpressure_503: summary.backpressure_503,
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
    fee: u128,
    signing_key: String,
    memo: String,
}

#[derive(Debug, Clone, Copy)]
struct SubmitOutcome {
    kind: OutcomeKind,
    latency_ms: u64,
    backpressure_429: u64,
    backpressure_503: u64,
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
                backpressure_429: 0,
                backpressure_503: 0,
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
            // Retry policy (bounded; per-tx):
            // - Network errors/timeouts: retry up to 2 times (10ms then 30ms backoff)
            // - HTTP 429/503: count as backpressure signals and retry once (10ms backoff)
            // - HTTP 4xx (except 429): do not retry
            let start_all = std::time::Instant::now();
            let endpoint = Self::endpoint(base_url);

            let mut backpressure_429 = 0u64;
            let mut backpressure_503 = 0u64;

            let mut network_retry_used = 0u8; // 0..=2
            let mut backpressure_retry_used = 0u8; // 0..=1

            loop {
                let resp = self.client.post(&endpoint).json(body).send().await;
                match resp {
                    Ok(r) => {
                        let status = r.status();
                        let status_u16 = status.as_u16();

                        if status_u16 == 429 {
                            backpressure_429 = backpressure_429.saturating_add(1);
                            if backpressure_retry_used < 1 {
                                backpressure_retry_used = backpressure_retry_used.saturating_add(1);
                                tokio::time::sleep(Duration::from_millis(10)).await;
                                continue;
                            }
                        } else if status_u16 == 503 {
                            backpressure_503 = backpressure_503.saturating_add(1);
                            if backpressure_retry_used < 1 {
                                backpressure_retry_used = backpressure_retry_used.saturating_add(1);
                                tokio::time::sleep(Duration::from_millis(10)).await;
                                continue;
                            }
                        } else if status.is_client_error() {
                            // 4xx (except 429 handled above): no retry
                        }

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

                        let latency_ms = start_all.elapsed().as_millis() as u64;
                        if status_u16 == 200 && has_tx_hash && !looks_like_error {
                            return Ok(SubmitOutcome {
                                kind: OutcomeKind::Accepted,
                                latency_ms,
                                backpressure_429,
                                backpressure_503,
                            });
                        } else {
                            return Ok(SubmitOutcome {
                                kind: OutcomeKind::Rejected,
                                latency_ms,
                                backpressure_429,
                                backpressure_503,
                            });
                        }
                    }
                    Err(e) if e.is_timeout() => {
                        if network_retry_used < 2 {
                            let backoff_ms = if network_retry_used == 0 { 10 } else { 30 };
                            network_retry_used = network_retry_used.saturating_add(1);
                            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                            continue;
                        }
                        return Ok(SubmitOutcome {
                            kind: OutcomeKind::Timeout,
                            latency_ms: self.timeout_ms,
                            backpressure_429,
                            backpressure_503,
                        });
                    }
                    Err(e) if e.is_connect() => {
                        if network_retry_used < 2 {
                            let backoff_ms = if network_retry_used == 0 { 10 } else { 30 };
                            network_retry_used = network_retry_used.saturating_add(1);
                            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                            continue;
                        }
                        return Err(e.into());
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        })
    }
}

struct StepEnv {
    config: Arc<Config>,
    recipients: Arc<Vec<String>>,
    submitter: Arc<dyn PaymentSubmitter>,
    stats: Arc<WorkerStats>,
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
                amount: env.amount_atomic,
                fee: env.fee_atomic,
                signing_key: env.signing_key.clone(),
                memo,
            };

            let submitter = env.submitter.clone();
            let stats = env.stats.clone();
            tokio::spawn(async move {
                let _permit: OwnedSemaphorePermit = permit;
                match submitter.submit_payment(&base_url, &body).await {
                    Ok(outcome) => {
                        stats.record_backpressure(outcome.backpressure_429, outcome.backpressure_503);
                        match outcome.kind {
                            OutcomeKind::Accepted => stats.record_accepted(1, outcome.latency_ms),
                            OutcomeKind::Rejected => stats.record_rejected(1, outcome.latency_ms),
                            OutcomeKind::Timeout => stats.record_timeout(1, outcome.latency_ms),
                        }
                    }
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
            amount: env.amount_atomic,
            fee: env.fee_atomic,
            signing_key: env.signing_key.clone(),
            memo,
        };

        let submitter = env.submitter.clone();
        let stats = env.stats.clone();
        tokio::spawn(async move {
            let _permit: OwnedSemaphorePermit = permit;
            match submitter.submit_payment(&base_url, &body).await {
                Ok(outcome) => {
                    stats.record_backpressure(outcome.backpressure_429, outcome.backpressure_503);
                    match outcome.kind {
                        OutcomeKind::Accepted => stats.record_accepted(1, outcome.latency_ms),
                        OutcomeKind::Rejected => stats.record_rejected(1, outcome.latency_ms),
                        OutcomeKind::Timeout => stats.record_timeout(1, outcome.latency_ms),
                    }
                }
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
    println!("Backpressure 429: {}", result.backpressure_429);
    println!("Backpressure 503: {}", result.backpressure_503);
    println!("Errors: {}", result.errors);
    println!("Timeouts: {}", result.timeouts);
    println!("Achieved TPS: {}", result.achieved_tps);
    println!("Latency p50: {}ms", result.latency_p50_ms);
    println!("Latency p95: {}ms", result.latency_p95_ms);
    println!("Latency p99: {}ms", result.latency_p99_ms);
    println!();
}

fn load_config(path: &PathBuf) -> Result<Config> {
    let s = std::fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&s)?;
    Ok(cfg)
}

fn now_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
        .to_string()
}

fn validate_fee_nonzero(fee_atomic: u128) -> Result<()> {
    if fee_atomic == 0 {
        anyhow::bail!("fee_atomic must be non-zero (fees are mandatory)");
    }
    Ok(())
}

fn validate_https_only(rpc_urls: &[String]) -> Result<()> {
    if rpc_urls.is_empty() {
        anyhow::bail!("target.rpc_urls must be non-empty");
    }

    for u in rpc_urls {
        if !u.starts_with("https://") {
            anyhow::bail!("RPC URL must be HTTPS (got {u})");
        }

        // Disallow explicit non-443 ports (node ports).
        // Accept:
        // - https://api1.ippan.uk
        // - https://api1.ippan.uk:443
        let rest = &u["https://".len()..];
        let authority = rest.split('/').next().unwrap_or("");
        if let Some((_, port_str)) = authority.rsplit_once(':') {
            if !port_str.is_empty() {
                let port: u16 = port_str
                    .parse()
                    .with_context(|| format!("Invalid port in RPC URL: {u}"))?;
                if port != 443 {
                    anyhow::bail!("RPC URL must not target node ports (got {u})");
                }
            }
        }
    }
    Ok(())
}

struct AtomicHistogram {
    max_ms: u64,
    buckets: Vec<AtomicU64>,
}

impl AtomicHistogram {
    fn new(max_ms: u64) -> Self {
        let len = (max_ms as usize).saturating_add(1).max(1);
        let mut buckets = Vec::with_capacity(len);
        for _ in 0..len {
            buckets.push(AtomicU64::new(0));
        }
        Self { max_ms, buckets }
    }

    #[inline]
    fn observe_ms(&self, latency_ms: u64) {
        let idx = latency_ms.min(self.max_ms) as usize;
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
    }

    fn percentiles(&self) -> (u64, u64, u64) {
        let mut h = LatencyHistogram::new(self.max_ms);
        for (i, b) in self.buckets.iter().enumerate() {
            let c = b.load(Ordering::Relaxed);
            if c != 0 {
                h.add_count_ms(i as u64, c);
            }
        }
        (
            h.percentile_ms(50),
            h.percentile_ms(95),
            h.percentile_ms(99),
        )
    }
}

struct WorkerStats {
    start_time: std::time::Instant,
    attempted: AtomicU64,
    sent: AtomicU64,
    accepted: AtomicU64,
    rejected: AtomicU64,
    backpressure_429: AtomicU64,
    backpressure_503: AtomicU64,
    errors: AtomicU64,
    timeouts: AtomicU64,
    latency: AtomicHistogram,
}

impl WorkerStats {
    fn new() -> Self {
        Self {
            start_time: std::time::Instant::now(),
            attempted: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            backpressure_429: AtomicU64::new(0),
            backpressure_503: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            latency: AtomicHistogram::new(60_000),
        }
    }

    #[inline]
    fn record_attempted(&self, count: u64) {
        self.attempted.fetch_add(count, Ordering::Relaxed);
    }

    #[inline]
    fn record_sent(&self, count: u64) {
        self.sent.fetch_add(count, Ordering::Relaxed);
    }

    #[inline]
    fn record_accepted(&self, count: u64, latency_ms: u64) {
        self.accepted.fetch_add(count, Ordering::Relaxed);
        self.latency.observe_ms(latency_ms);
    }

    #[inline]
    fn record_rejected(&self, count: u64, latency_ms: u64) {
        self.rejected.fetch_add(count, Ordering::Relaxed);
        self.latency.observe_ms(latency_ms);
    }

    #[inline]
    fn record_backpressure(&self, count_429: u64, count_503: u64) {
        if count_429 != 0 {
            self.backpressure_429.fetch_add(count_429, Ordering::Relaxed);
        }
        if count_503 != 0 {
            self.backpressure_503.fetch_add(count_503, Ordering::Relaxed);
        }
    }

    #[inline]
    fn record_timeout(&self, count: u64, latency_ms: u64) {
        self.timeouts.fetch_add(count, Ordering::Relaxed);
        self.latency.observe_ms(latency_ms);
    }

    #[inline]
    fn record_error(&self, count: u64) {
        self.errors.fetch_add(count, Ordering::Relaxed);
    }

    fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    fn summary(&self) -> StatsSummary {
        let (p50, p95, p99) = self.latency.percentiles();
        StatsSummary {
            attempted: self.attempted.load(Ordering::Relaxed),
            sent: self.sent.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            backpressure_429: self.backpressure_429.load(Ordering::Relaxed),
            backpressure_503: self.backpressure_503.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
            latency_p50_ms: p50,
            latency_p95_ms: p95,
            latency_p99_ms: p99,
            duration_ms: self.elapsed_ms(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct StatsSummary {
    attempted: u64,
    sent: u64,
    accepted: u64,
    rejected: u64,
    backpressure_429: u64,
    backpressure_503: u64,
    errors: u64,
    timeouts: u64,
    latency_p50_ms: u64,
    latency_p95_ms: u64,
    latency_p99_ms: u64,
    duration_ms: u64,
}
