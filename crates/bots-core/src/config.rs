use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub scenario: ScenarioConfig,
    pub ramp: RampConfig,
    pub target: TargetConfig,
    pub payment: PaymentConfig,
    pub worker: WorkerConfig,
    #[serde(default)]
    pub controller: Option<ControllerConfig>,
}

impl Config {
    pub fn from_file(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioConfig {
    /// Deterministic seed for reproducible payload generation
    pub seed: u64,
    /// Optional global duration cap in milliseconds
    pub duration_ms: Option<u64>,
    /// Memo string to include in transactions (will be truncated to 256 bytes)
    #[serde(default)]
    pub memo: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampStep {
    /// Target transactions per second
    pub tps: u64,
    /// Duration to hold this rate in milliseconds
    pub hold_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampConfig {
    pub steps: Vec<RampStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetConfig {
    /// List of RPC endpoints (worker will round-robin)
    pub rpc_urls: Vec<String>,
    /// Request timeout in milliseconds
    pub timeout_ms: u64,
    /// Maximum concurrent pending requests
    pub max_in_flight: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerConfig {
    /// Worker identifier for metrics and results
    pub id: String,
    /// Address to bind metrics endpoint (e.g., "127.0.0.1:9100")
    pub bind_metrics: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentConfig {
    /// Source address for payments
    pub from: String,
    /// Destination mode: "round_robin" or "single"
    pub to_mode: String,
    /// Path to file with list of destination addresses (for round_robin mode)
    pub to_list_path: Option<String>,
    /// Single destination address (for single mode)
    pub to_single: Option<String>,
    /// Payment amount (u128 as string to preserve precision)
    pub amount: String,
    /// Signing key (test key only!)
    pub signing_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControllerConfig {
    /// List of worker hostnames or addresses (for SSH orchestration)
    pub worker_hosts: Option<Vec<String>>,
    /// Number of local workers to spawn (for local testing)
    pub local_workers: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_serde() {
        let config_str = r#"
[scenario]
seed = 42
memo = "test"

[[ramp.steps]]
tps = 1000
hold_ms = 5000

[target]
rpc_urls = ["http://localhost:8080"]
timeout_ms = 5000
max_in_flight = 1000

[payment]
from = "test_from_addr"
to_mode = "single"
to_single = "test_to_addr"
amount = "1000"
signing_key = "test_key"

[worker]
id = "test-worker"
bind_metrics = "127.0.0.1:9100"
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        assert_eq!(config.scenario.seed, 42);
        assert_eq!(config.scenario.memo, "test");
        assert_eq!(config.ramp.steps.len(), 1);
        assert_eq!(config.ramp.steps[0].tps, 1000);
        assert_eq!(config.worker.id, "test-worker");
        assert_eq!(config.payment.from, "test_from_addr");
        assert_eq!(config.payment.amount, "1000");
    }
}
