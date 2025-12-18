//! Deterministic, pure logic shared by IPPAN load-generator binaries.
//!
//! Hard rules:
//! - No floating point types (`f32` / `f64`)
//! - Integer-only math for ramp planning and percentiles
//! - No wall-clock dependency in this crate (no `Instant`, no timers)

use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Root config (matches the TOML layout).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub scenario: ScenarioConfig,
    pub ramp: RampConfig,
    pub target: TargetConfig,
    pub payment: PaymentConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioConfig {
    pub seed: u64,
    pub memo: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampStep {
    pub tps: u64,
    pub hold_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampConfig {
    pub steps: Vec<RampStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetConfig {
    pub rpc_urls: Vec<String>,
    pub timeout_ms: u64,
    pub max_in_flight: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentConfig {
    pub from: String,
    pub to_list_path: String,
    // TOML (via `toml` crate) does not support serializing u128, but we need u128 at runtime.
    // Serialize as base-10 strings, while accepting either integer or string on input.
    #[serde(
        serialize_with = "ser_u128_as_string",
        deserialize_with = "de_u128_any"
    )]
    pub amount_atomic: u128,
    #[serde(
        serialize_with = "ser_u128_as_string",
        deserialize_with = "de_u128_any"
    )]
    pub fee_atomic: u128,
    pub signing_key: String,
}

fn ser_u128_as_string<S>(v: &u128, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&v.to_string())
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

/// Deterministic ramp planner: steps are applied strictly in order.
#[derive(Debug, Clone)]
pub struct RampPlanner {
    steps: Vec<RampStep>,
}

impl RampPlanner {
    pub fn new(config: RampConfig) -> Self {
        Self {
            steps: config.steps,
        }
    }

    /// Iterate steps strictly in order.
    pub fn steps(&self) -> &[RampStep] {
        &self.steps
    }

    /// Total duration of all ramp steps in milliseconds.
    pub fn total_duration_ms(&self) -> u64 {
        self.steps.iter().map(|s| s.hold_ms).sum()
    }

    /// Current target TPS at a given elapsed time in ms (pure function).
    pub fn current_tps(&self, elapsed_ms: u64) -> Option<u64> {
        let mut cumulative_ms = 0u64;
        for step in &self.steps {
            if elapsed_ms < cumulative_ms.saturating_add(step.hold_ms) {
                return Some(step.tps);
            }
            cumulative_ms = cumulative_ms.saturating_add(step.hold_ms);
        }
        None
    }
}

/// Integer-ms latency histogram.
///
/// Buckets are 1ms wide: bucket `i` counts observations with `latency_ms == i`,
/// with the final bucket (`max_ms`) acting as a catch-all for any larger values.
#[derive(Debug, Clone)]
pub struct LatencyHistogram {
    max_ms: u64,
    buckets: Vec<u64>,
    total: u64,
}

impl LatencyHistogram {
    pub fn new(max_ms: u64) -> Self {
        let len = (max_ms as usize).saturating_add(1).max(1);
        Self {
            max_ms,
            buckets: vec![0u64; len],
            total: 0,
        }
    }

    #[inline]
    pub fn observe_ms(&mut self, latency_ms: u64) {
        self.add_count_ms(latency_ms, 1);
    }

    #[inline]
    pub fn add_count_ms(&mut self, latency_ms: u64, count: u64) {
        if count == 0 {
            return;
        }
        let idx = latency_ms.min(self.max_ms) as usize;
        // Deterministic, saturating accounting.
        self.buckets[idx] = self.buckets[idx].saturating_add(count);
        self.total = self.total.saturating_add(count);
    }

    pub fn total_count(&self) -> u64 {
        self.total
    }

    /// Deterministic percentile in integer milliseconds.
    ///
    /// Uses a 1-indexed rank: `ceil(p/100 * total)` (with p=0 treated as 1).
    pub fn percentile_ms(&self, p: u64) -> u64 {
        let p = p.clamp(0, 100);
        if self.total == 0 {
            return 0;
        }

        let rank = if p == 0 {
            1
        } else {
            ((self.total.saturating_mul(p) + 99) / 100).max(1)
        };

        let mut cumulative = 0u64;
        for (i, c) in self.buckets.iter().enumerate() {
            cumulative = cumulative.saturating_add(*c);
            if cumulative >= rank {
                return i as u64;
            }
        }
        self.max_ms
    }
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        // 60s catch-all is enough for load-test latencies/timeouts.
        Self::new(60_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_parsing_inline_tables() {
        let config_str = r#"
[scenario]
seed = 42
memo = "ippan-load-test"

[target]
rpc_urls = [
  "https://api1.ippan.uk",
  "https://api2.ippan.uk"
]
timeout_ms = 3000
max_in_flight = 2000

[payment]
from = "@loadbank.ipn"
to_list_path = "keys/recipients.json"
amount_atomic = 1
fee_atomic = 2000
signing_key = "TEST_ONLY_KEY"

[ramp]
steps = [
  { tps = 1000, hold_ms = 120000 },
  { tps = 5000, hold_ms = 120000 }
]
"#;

        let cfg: Config = toml::from_str(config_str).unwrap();
        assert_eq!(cfg.scenario.seed, 42);
        assert_eq!(cfg.ramp.steps.len(), 2);
        assert_eq!(cfg.ramp.steps[0].tps, 1000);
        assert_eq!(cfg.ramp.steps[1].hold_ms, 120000);
        assert_eq!(cfg.payment.amount_atomic, 1);
        assert_eq!(cfg.payment.fee_atomic, 2000);
    }

    #[test]
    fn payment_u128_serializes_as_string_for_toml() {
        let cfg = Config {
            scenario: ScenarioConfig {
                seed: 42,
                memo: "m".to_string(),
            },
            ramp: RampConfig {
                steps: vec![RampStep {
                    tps: 1,
                    hold_ms: 1000,
                }],
            },
            target: TargetConfig {
                rpc_urls: vec!["http://127.0.0.1:8080".to_string()],
                timeout_ms: 3000,
                max_in_flight: 1,
            },
            payment: PaymentConfig {
                from: "@a.ipn".to_string(),
                to_list_path: "keys/recipients.json".to_string(),
                amount_atomic: 1u128,
                fee_atomic: 2000u128,
                signing_key: "k".to_string(),
            },
        };

        let s = toml::to_string_pretty(&cfg).expect("config should serialize to TOML");
        assert!(s.contains("amount_atomic = \"1\""));
        assert!(s.contains("fee_atomic = \"2000\""));
    }

    #[test]
    fn percentile_math_is_deterministic() {
        let mut h = LatencyHistogram::new(1_000);

        // 100 samples:
        // - 50 at 10ms
        // - 30 at 20ms
        // - 20 at 50ms
        for _ in 0..50 {
            h.observe_ms(10);
        }
        for _ in 0..30 {
            h.observe_ms(20);
        }
        for _ in 0..20 {
            h.observe_ms(50);
        }

        assert_eq!(h.total_count(), 100);
        assert_eq!(h.percentile_ms(50), 10);
        assert_eq!(h.percentile_ms(95), 50);
        assert_eq!(h.percentile_ms(99), 50);
    }
}
