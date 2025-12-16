use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Statistics tracker with integer-based latency histogram
pub struct StatsCollector {
    pub attempted: u64,
    pub sent: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub errors: u64,
    pub timeouts: u64,
    latency_buckets: Vec<u64>,
    start_time: Instant,
}

impl StatsCollector {
    pub fn new() -> Self {
        // Create histogram buckets: 0-1ms, 1-2ms, 2-5ms, 5-10ms, 10-20ms, ...
        // Using powers and multiples for reasonable coverage
        let buckets = vec![0; 100]; // Up to ~10 seconds
        Self {
            attempted: 0,
            sent: 0,
            accepted: 0,
            rejected: 0,
            errors: 0,
            timeouts: 0,
            latency_buckets: buckets,
            start_time: Instant::now(),
        }
    }

    pub fn record_attempted(&mut self, count: u64) {
        self.attempted += count;
    }

    pub fn record_sent(&mut self, count: u64) {
        self.sent += count;
    }

    pub fn record_success(&mut self, accepted: u64, rejected: u64, timeouts: u64, latency_ms: u64) {
        self.accepted += accepted;
        self.rejected += rejected;
        self.timeouts += timeouts;
        self.record_latency(latency_ms);
    }

    pub fn record_error(&mut self, count: u64) {
        self.errors += count;
    }

    fn record_latency(&mut self, latency_ms: u64) {
        // Simple bucket assignment: use latency_ms directly capped at bucket length
        let bucket_idx = latency_ms.min((self.latency_buckets.len() - 1) as u64) as usize;
        self.latency_buckets[bucket_idx] += 1;
    }

    /// Calculate percentile from histogram (integer ms)
    pub fn percentile(&self, p: u64) -> u64 {
        let total: u64 = self.latency_buckets.iter().sum();
        if total == 0 {
            return 0;
        }

        let target = (total * p) / 100;
        let mut cumulative = 0u64;

        for (bucket_idx, &count) in self.latency_buckets.iter().enumerate() {
            cumulative += count;
            if cumulative >= target {
                return bucket_idx as u64;
            }
        }

        self.latency_buckets.len() as u64
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    pub fn summary(&self) -> StatsSummary {
        StatsSummary {
            attempted: self.attempted,
            sent: self.sent,
            accepted: self.accepted,
            rejected: self.rejected,
            errors: self.errors,
            timeouts: self.timeouts,
            latency_p50_ms: self.percentile(50),
            latency_p95_ms: self.percentile(95),
            latency_p99_ms: self.percentile(99),
            duration_ms: self.elapsed_ms(),
        }
    }
}

impl Default for StatsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSummary {
    pub attempted: u64,
    pub sent: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub errors: u64,
    pub timeouts: u64,
    pub latency_p50_ms: u64,
    pub latency_p95_ms: u64,
    pub latency_p99_ms: u64,
    pub duration_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_collector_basic() {
        let mut stats = StatsCollector::new();

        stats.record_attempted(100);
        stats.record_sent(100);
        stats.record_success(95, 5, 0, 10);

        assert_eq!(stats.attempted, 100);
        assert_eq!(stats.sent, 100);
        assert_eq!(stats.accepted, 95);
        assert_eq!(stats.rejected, 5);
    }

    #[test]
    fn test_percentile_calculation() {
        let mut stats = StatsCollector::new();

        // Record 100 samples at various latencies
        for _ in 0..50 {
            stats.record_success(1, 0, 0, 10);
        }
        for _ in 0..30 {
            stats.record_success(1, 0, 0, 20);
        }
        for _ in 0..20 {
            stats.record_success(1, 0, 0, 50);
        }

        let p50 = stats.percentile(50);
        let p95 = stats.percentile(95);

        assert!(p50 <= 20, "p50 should be around 10-20ms, got {}", p50);
        assert!(p95 >= 20, "p95 should be >= 20ms, got {}", p95);
    }
}
