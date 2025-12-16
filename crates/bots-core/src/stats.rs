use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Integer-ms latency histogram backed by atomics for low-overhead hot-path updates.
///
/// Buckets are 1ms wide: bucket `i` counts observations with `latency_ms == i`,
/// with the final bucket (`max_ms`) acting as a catch-all for any larger values.
pub struct LatencyHistogram {
    max_ms: u64,
    buckets: Vec<AtomicU64>,
}

impl LatencyHistogram {
    pub fn new(max_ms: u64) -> Self {
        let len = (max_ms as usize).saturating_add(1).max(1);
        let mut buckets = Vec::with_capacity(len);
        for _ in 0..len {
            buckets.push(AtomicU64::new(0));
        }
        Self { max_ms, buckets }
    }

    #[inline]
    pub fn observe_ms(&self, latency_ms: u64) {
        let idx = latency_ms.min(self.max_ms) as usize;
        self.buckets[idx].fetch_add(1, Ordering::Relaxed);
    }

    pub fn total_count(&self) -> u64 {
        self.buckets.iter().map(|b| b.load(Ordering::Relaxed)).sum()
    }

    pub fn percentile_ms(&self, p: u64) -> u64 {
        let p = p.clamp(0, 100);
        let total = self.total_count();
        if total == 0 {
            return 0;
        }

        // Rank is 1-indexed: ceil(p/100 * total). For p=0, treat as 1 (min).
        let rank = if p == 0 {
            1
        } else {
            ((total.saturating_mul(p) + 99) / 100).max(1)
        };

        let mut cumulative = 0u64;
        for (i, b) in self.buckets.iter().enumerate() {
            cumulative = cumulative.saturating_add(b.load(Ordering::Relaxed));
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

/// Low-overhead counters + latency distribution for a run.
pub struct StatsCollector {
    start_time: Instant,
    attempted: AtomicU64,
    sent: AtomicU64,
    accepted: AtomicU64,
    rejected: AtomicU64,
    errors: AtomicU64,
    timeouts: AtomicU64,
    latency: LatencyHistogram,
}

impl StatsCollector {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
            attempted: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            latency: LatencyHistogram::default(),
        }
    }

    #[inline]
    pub fn record_attempted(&self, count: u64) {
        self.attempted.fetch_add(count, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_sent(&self, count: u64) {
        self.sent.fetch_add(count, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_accepted(&self, count: u64, latency_ms: u64) {
        self.accepted.fetch_add(count, Ordering::Relaxed);
        self.latency.observe_ms(latency_ms);
    }

    #[inline]
    pub fn record_rejected(&self, count: u64, latency_ms: u64) {
        self.rejected.fetch_add(count, Ordering::Relaxed);
        self.latency.observe_ms(latency_ms);
    }

    #[inline]
    pub fn record_timeout(&self, count: u64, latency_ms: u64) {
        self.timeouts.fetch_add(count, Ordering::Relaxed);
        self.latency.observe_ms(latency_ms);
    }

    #[inline]
    pub fn record_error(&self, count: u64) {
        self.errors.fetch_add(count, Ordering::Relaxed);
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    pub fn summary(&self) -> StatsSummary {
        StatsSummary {
            attempted: self.attempted.load(Ordering::Relaxed),
            sent: self.sent.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            timeouts: self.timeouts.load(Ordering::Relaxed),
            latency_p50_ms: self.latency.percentile_ms(50),
            latency_p95_ms: self.latency.percentile_ms(95),
            latency_p99_ms: self.latency.percentile_ms(99),
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
    fn test_histogram_percentiles_are_stable() {
        let h = LatencyHistogram::new(1_000);

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

    #[test]
    fn test_stats_collector_basic() {
        let s = StatsCollector::new();
        s.record_attempted(100);
        s.record_sent(100);
        s.record_accepted(95, 10);
        s.record_rejected(5, 20);

        let summary = s.summary();
        assert_eq!(summary.attempted, 100);
        assert_eq!(summary.sent, 100);
        assert_eq!(summary.accepted, 95);
        assert_eq!(summary.rejected, 5);
        assert!(summary.latency_p50_ms >= 10);
    }
}
