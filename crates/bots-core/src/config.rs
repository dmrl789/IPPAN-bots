use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub scenario: ScenarioConfig,
    pub ramp: RampConfig,
    pub target: TargetConfig,
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
    /// Fixed size for generated payloads in bytes
    pub payload_bytes: u32,
    /// Maximum transactions per batch (0 or 1 means no batching)
    pub batch_max: u32,
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
pub struct ControllerConfig {
    /// List of worker hostnames or addresses
    pub worker_hosts: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_serde() {
        let config_str = r#"
[scenario]
seed = 42
payload_bytes = 512
batch_max = 10

[[ramp.steps]]
tps = 1000
hold_ms = 5000

[target]
rpc_urls = ["http://localhost:8080"]
timeout_ms = 5000
max_in_flight = 1000

[worker]
id = "test-worker"
bind_metrics = "127.0.0.1:9100"
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        assert_eq!(config.scenario.seed, 42);
        assert_eq!(config.scenario.payload_bytes, 512);
        assert_eq!(config.ramp.steps.len(), 1);
        assert_eq!(config.ramp.steps[0].tps, 1000);
        assert_eq!(config.worker.id, "test-worker");
    }
}
