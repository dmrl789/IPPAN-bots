use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub scenario: ScenarioConfig,
    pub ramp: RampConfig,
    pub target: TargetConfig,
    pub payment: PaymentConfig,
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
    /// Base memo string (worker caps to 256 bytes)
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
pub struct PaymentConfig {
    pub from: String,
    /// Path to a JSON file containing recipient addresses (round-robin).
    pub to_list_path: String,
    /// Payment amount in atomic units.
    ///
    /// Accepts either a TOML integer (u64/i64) or a base-10 string.
    #[serde(deserialize_with = "de_u128_any")]
    pub amount_atomic: u128,
    /// Mandatory transaction fee in atomic units.
    ///
    /// Accepts either a TOML integer (u64/i64) or a base-10 string.
    #[serde(deserialize_with = "de_u128_any")]
    pub fee_atomic: u128,
    /// Custodial signing key passed to `/tx/payment` (test keys only)
    pub signing_key: String,
}

fn de_u128_any<'de, D>(deserializer: D) -> Result<u128, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct U128AnyVisitor;

    impl Visitor<'_> for U128AnyVisitor {
        type Value = u128;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a u128 encoded as TOML integer or base-10 string")
        }

        fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            Ok(v as u128)
        }

        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            if v < 0 {
                return Err(E::custom("expected non-negative integer for u128"));
            }
            Ok(v as u128)
        }

        fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            v.trim()
                .parse::<u128>()
                .map_err(|e| E::custom(format!("invalid u128 string: {e}")))
        }

        fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
        where
            E: DeError,
        {
            self.visit_str(&v)
        }
    }

    deserializer.deserialize_any(U128AnyVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_serde() {
        let config_str = r#"
[scenario]
seed = 42
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
to_list_path = "keys/recipients.json"
amount_atomic = 123
fee_atomic = "2000"
signing_key = "test_key"
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        assert_eq!(config.scenario.seed, 42);
        assert_eq!(config.scenario.memo, "hello");
        assert_eq!(config.ramp.steps.len(), 1);
        assert_eq!(config.ramp.steps[0].tps, 1000);
        assert_eq!(config.payment.from, "from1");
        assert_eq!(config.payment.to_list_path, "keys/recipients.json");
        assert_eq!(config.payment.amount_atomic, 123);
        assert_eq!(config.payment.fee_atomic, 2000);
    }

    #[test]
    fn test_ramp_inline_table_parsing() {
        let config_str = r#"
[scenario]
seed = 1
memo = "m"

[ramp]
steps = [
  { tps = 10, hold_ms = 1000 },
  { tps = 20, hold_ms = 2000 }
]

[target]
rpc_urls = ["http://localhost:8080"]
timeout_ms = 1000
max_in_flight = 10

[payment]
from = "from"
to_list_path = "keys/recipients.json"
amount_atomic = 1
fee_atomic = 2000
signing_key = "k"
"#;

        let config: Config = toml::from_str(config_str).unwrap();
        assert_eq!(config.ramp.steps.len(), 2);
        assert_eq!(config.ramp.steps[0].tps, 10);
        assert_eq!(config.ramp.steps[1].hold_ms, 2000);
    }
}
