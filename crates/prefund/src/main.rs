use anyhow::{Context, Result};
use bots_core::Config;
use clap::Parser;
use serde::Serialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(name = "prefund")]
#[command(about = "Prefund estimator for a configured ramp (integer math, fees enabled)")]
struct Args {
    /// Path to configuration file
    #[arg(long, default_value = "config/example.deventer.toml")]
    config: PathBuf,
}

#[derive(Debug, Serialize)]
struct PrefundReport {
    timestamp: String,
    config_path: String,
    total_tx_count: u128,
    amount_atomic: u128,
    fee_atomic: u128,
    required_fees_total_atomic: u128,
    required_total_atomic: u128,
    recommended_prefund_atomic: u128,
    buffer_percent: u64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = load_config(&args.config)
        .with_context(|| format!("Failed to load config from {:?}", args.config))?;

    if cfg.payment.fee_atomic == 0 {
        anyhow::bail!("fee_atomic must be non-zero (fees are mandatory)");
    }

    let total_tx = total_tx_count(&cfg);
    let per_tx_total = cfg
        .payment
        .amount_atomic
        .saturating_add(cfg.payment.fee_atomic);

    let required_total = total_tx.saturating_mul(per_tx_total);
    let required_fees_total = total_tx.saturating_mul(cfg.payment.fee_atomic);

    // +20% buffer, integer-only.
    const BUFFER_PERCENT: u64 = 20;
    let recommended_prefund = required_total
        .saturating_mul(120u128)
        .saturating_div(100u128);

    println!();
    println!("=== Prefund Report ===");
    println!("Config: {:?}", args.config);
    println!("Total tx count (Σ tps * (hold_ms/1000)): {total_tx}");
    println!("Per-tx amount_atomic: {}", cfg.payment.amount_atomic);
    println!("Per-tx fee_atomic: {}", cfg.payment.fee_atomic);
    println!("Required fees total (atomic): {required_fees_total}");
    println!("Required total (amount+fees) (atomic): {required_total}");
    println!(
        "Recommended prefund ({}% buffer) (atomic): {}",
        BUFFER_PERCENT, recommended_prefund
    );

    let timestamp = now_id();
    let out_dir = PathBuf::from("results");
    std::fs::create_dir_all(&out_dir).ok();
    let out_path = out_dir.join(format!("prefund_{timestamp}.json"));

    let report = PrefundReport {
        timestamp: timestamp.clone(),
        config_path: args.config.to_string_lossy().to_string(),
        total_tx_count: total_tx,
        amount_atomic: cfg.payment.amount_atomic,
        fee_atomic: cfg.payment.fee_atomic,
        required_fees_total_atomic: required_fees_total,
        required_total_atomic: required_total,
        recommended_prefund_atomic: recommended_prefund,
        buffer_percent: BUFFER_PERCENT,
    };

    std::fs::write(&out_path, serde_json::to_string_pretty(&report)?)?;
    println!();
    println!("Wrote: {}", out_path.to_string_lossy());
    Ok(())
}

fn load_config(path: &PathBuf) -> Result<Config> {
    let s = std::fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&s)?;
    Ok(cfg)
}

fn total_tx_count(cfg: &Config) -> u128 {
    let mut total: u128 = 0;
    for step in &cfg.ramp.steps {
        let seconds = (step.hold_ms / 1000) as u128;
        let step_tx = (step.tps as u128).saturating_mul(seconds);
        total = total.saturating_add(step_tx);
    }
    total
}

fn now_id() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
        .to_string()
}

