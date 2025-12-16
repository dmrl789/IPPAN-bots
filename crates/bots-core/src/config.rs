use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub scenario: ScenarioConfig,
    pub ramp: RampConfig,
    pub target: TargetConfig,
    pub payment: PaymentConfig,
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
    /// Desired payload size in bytes (used to deterministically pad memo/body)
    pub payload_bytes: u32,
    /// Base memo string (will be capped to 256 bytes at runtime)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToMode {
    RoundRobin,
    Single,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentConfig {
    pub from: String,
    pub to_mode: ToMode,
    /// Optional path to a newline-delimited list of recipients.
    /// - `round_robin`: cycle through recipients
    /// - `single`: always use the first recipient
    #[serde(default)]
    pub to_list_path: Option<String>,
    pub amount: u128,
    /// Custodial signing key passed to `/tx/payment` (test keys only)
    pub signing_key: String,
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
memo = "hello"

[[ramp.steps]]
tps = 1000
hold_ms = 5000

[target]
rpc_urls = ["http://localhost:8080"]
timeout_ms = 5000
max_in_flight = 1000

[payment]
from = "from1"
to_mode = "single"
to_list_path = "secrets/to_list.txt"
amount = 123
signing_key = "test_key"
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        assert_eq!(config.scenario.seed, 42);
        assert_eq!(config.scenario.payload_bytes, 512);
        assert_eq!(config.ramp.steps.len(), 1);
        assert_eq!(config.ramp.steps[0].tps, 1000);
        assert_eq!(config.payment.from, "from1");
        assert_eq!(config.payment.to_mode, ToMode::Single);
        assert_eq!(config.payment.amount, 123);
    }
}
