use anyhow::{Context, Result};
use bots_core::Config;
use clap::Parser;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "preflight")]
#[command(about = "Preflight checks for Deventer distributed runs (DNS/TLS/API health)")]
struct Args {
    /// Path to configuration file
    #[arg(long, default_value = "config/example.deventer.toml")]
    config: PathBuf,
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
    let cfg = load_config(&args.config)
        .with_context(|| format!("Failed to load config from {:?}", args.config))?;

    // Deterministic allow-list: Deventer run is fixed to 4 API hosts.
    let allowed_hosts: BTreeSet<&'static str> = [
        "api1.ippan.uk",
        "api2.ippan.uk",
        "api3.ippan.uk",
        "api4.ippan.uk",
    ]
    .into_iter()
    .collect();

    info!("Validating config RPC URLs...");
    let mut base_urls = Vec::new();
    for u in &cfg.target.rpc_urls {
        let url = validate_rpc_url(u, &allowed_hosts)
            .with_context(|| format!("Invalid target.rpc_urls entry: {u}"))?;
        base_urls.push(url);
    }

    info!("Using fixed tx endpoint: /tx/payment");
    println!();
    println!("=== Preflight ({} endpoints) ===", base_urls.len());

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(cfg.target.timeout_ms.max(1)))
        .tcp_keepalive(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")?;

    let mut failures = 0u64;
    for base in base_urls {
        let host = base
            .host_str()
            .context("validated URL must have host_str")?
            .to_string();
        let shown_base = base.as_str().trim_end_matches('/').to_string();

        println!();
        println!("Endpoint: {shown_base}");
        println!("TLS hostname: {host}");
        println!("Tx path: {shown_base}/tx/payment");

        // Health (required)
        if !check_required(&client, &base, "/health").await? {
            failures = failures.saturating_add(1);
        }
        // Version (required)
        if !check_required(&client, &base, "/version").await? {
            failures = failures.saturating_add(1);
        }
        // Status (best-effort)
        check_status_best_effort(&client, &base, "/status").await?;
    }

    if failures > 0 {
        anyhow::bail!("Preflight failed: {failures} required endpoint check(s) failed");
    }

    println!();
    println!("Preflight OK.");
    Ok(())
}

fn load_config(path: &PathBuf) -> Result<Config> {
    let s = std::fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&s)?;
    Ok(cfg)
}

fn validate_rpc_url(raw: &str, allowed_hosts: &BTreeSet<&'static str>) -> Result<url::Url> {
    let url = url::Url::parse(raw).with_context(|| format!("Invalid URL: {raw}"))?;

    if url.scheme() != "https" {
        anyhow::bail!("RPC URL must be https:// (got {raw})");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("RPC URL must not include credentials (got {raw})");
    }

    let host = url.host_str().context("RPC URL must include a hostname")?;

    if !allowed_hosts.contains(host) {
        anyhow::bail!("RPC URL host must be one of api1..api4.ippan.uk (got host={host})");
    }

    if host.parse::<std::net::IpAddr>().is_ok() {
        anyhow::bail!("RPC URL must not use a raw IP address (got {raw})");
    }

    if let Some(port) = url.port() {
        if port == 9000 {
            anyhow::bail!("RPC URL must not use port 9000 (got {raw})");
        }
        if port != 443 {
            anyhow::bail!("RPC URL must not specify non-443 ports (got {raw})");
        }
    }

    // Enforce base URL only; worker will append `/tx/payment`.
    if url.path() != "/" {
        anyhow::bail!(
            "RPC URL must not include a path (got path={}, url={raw})",
            url.path()
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        anyhow::bail!("RPC URL must not include query/fragment (got {raw})");
    }

    Ok(url)
}

async fn check_required(client: &reqwest::Client, base: &url::Url, path: &str) -> Result<bool> {
    let url = base.join(path.trim_start_matches('/'))?;
    let start = Instant::now();
    let resp = client.get(url.clone()).send().await;
    let latency_ms = start.elapsed().as_millis() as u64;

    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            println!("GET {:<10} -> {} ({}ms)", path, status, latency_ms);
            if status != 200 {
                warn!("Required endpoint returned non-200: {} {}", path, status);
                Ok(false)
            } else {
                Ok(true)
            }
        }
        Err(e) => {
            println!("GET {:<10} -> ERROR ({}ms)", path, latency_ms);
            warn!("Required endpoint request failed: {} error={}", path, e);
            Ok(false)
        }
    }
}

async fn check_status_best_effort(
    client: &reqwest::Client,
    base: &url::Url,
    path: &str,
) -> Result<()> {
    let url = base.join(path.trim_start_matches('/'))?;
    let start = Instant::now();
    let resp = client.get(url.clone()).send().await;
    let latency_ms = start.elapsed().as_millis() as u64;

    // Slow status should warn but never fail the preflight.
    const SLOW_STATUS_WARN_MS: u64 = 1_000;

    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            println!("GET {:<10} -> {} ({}ms)", path, status, latency_ms);
            if latency_ms > SLOW_STATUS_WARN_MS {
                warn!(
                    "Status endpoint is slow: {}ms (threshold {}ms)",
                    latency_ms, SLOW_STATUS_WARN_MS
                );
            }
            if status != 200 {
                warn!("Status endpoint returned non-200 (best-effort): {}", status);
            }
        }
        Err(e) => {
            println!("GET {:<10} -> ERROR ({}ms)", path, latency_ms);
            warn!("Status endpoint request failed (best-effort): {}", e);
        }
    }

    Ok(())
}
