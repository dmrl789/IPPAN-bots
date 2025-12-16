use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Rate limiter using integer-only math with token bucket algorithm
pub struct RateLimiter {
    /// Target rate in tokens per second
    tps: u64,
    /// Maximum tokens that can accumulate
    capacity: u64,
    /// Current token count (scaled by SCALE for precision)
    tokens: u128,
    /// Last refill timestamp
    last_refill: Instant,
    /// Scaling factor for sub-millisecond precision (microseconds)
    scale: u128,
}

const MICROS_PER_SECOND: u128 = 1_000_000;

impl RateLimiter {
    pub fn new(tps: u64) -> Self {
        Self::with_capacity(tps, tps)
    }

    pub fn with_capacity(tps: u64, capacity: u64) -> Self {
        Self {
            tps,
            capacity,
            tokens: (capacity as u128) * MICROS_PER_SECOND,
            last_refill: Instant::now(),
            scale: MICROS_PER_SECOND,
        }
    }

    /// Update the target rate (for dynamic ramp adjustments)
    pub fn set_rate(&mut self, tps: u64) {
        self.refill();
        self.tps = tps;
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill);
        let elapsed_micros = elapsed.as_micros();

        if elapsed_micros == 0 {
            return;
        }

        // Calculate tokens to add: (tps * elapsed_micros) / MICROS_PER_SECOND
        // Scaled representation: tps * elapsed_micros (already in micro-scale)
        let tokens_to_add = (self.tps as u128) * elapsed_micros;
        self.tokens = self.tokens.saturating_add(tokens_to_add);

        // Cap at capacity
        let max_tokens = (self.capacity as u128) * MICROS_PER_SECOND;
        if self.tokens > max_tokens {
            self.tokens = max_tokens;
        }

        self.last_refill = now;
    }

    /// Try to acquire a token, returns true if successful
    pub fn try_acquire(&mut self) -> bool {
        self.refill();

        if self.tokens >= self.scale {
            self.tokens -= self.scale;
            true
        } else {
            false
        }
    }

    /// Wait until a token is available and acquire it
    pub async fn acquire(&mut self) {
        loop {
            if self.try_acquire() {
                return;
            }

            // Calculate time until next token is available
            let deficit = self.scale.saturating_sub(self.tokens);
            if self.tps == 0 {
                // If rate is 0, sleep indefinitely (or could return error)
                sleep(Duration::from_secs(1)).await;
                continue;
            }

            // Time in microseconds = deficit / tps
            let wait_micros = deficit / (self.tps as u128);
            let wait_duration = Duration::from_micros(wait_micros as u64);

            sleep(wait_duration).await;
            self.refill();
        }
    }

    /// Try to acquire N tokens at once (for batching)
    pub fn try_acquire_batch(&mut self, count: u64) -> bool {
        self.refill();

        let required = (count as u128) * self.scale;
        if self.tokens >= required {
            self.tokens -= required;
            true
        } else {
            false
        }
    }

    /// Wait until N tokens are available and acquire them
    pub async fn acquire_batch(&mut self, count: u64) {
        loop {
            if self.try_acquire_batch(count) {
                return;
            }

            // Calculate time until enough tokens are available
            let required = (count as u128) * self.scale;
            let deficit = required.saturating_sub(self.tokens);
            if self.tps == 0 {
                sleep(Duration::from_secs(1)).await;
                continue;
            }

            let wait_micros = deficit / (self.tps as u128);
            let wait_duration = Duration::from_micros(wait_micros as u64);

            sleep(wait_duration).await;
            self.refill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_rate_limiter_basic() {
        let mut limiter = RateLimiter::new(100); // 100 TPS

        // Should be able to acquire immediately
        assert!(limiter.try_acquire());
    }

    #[tokio::test]
    async fn test_rate_limiter_refill() {
        let mut limiter = RateLimiter::new(1000); // 1000 TPS

        // Drain tokens
        for _ in 0..1000 {
            assert!(limiter.try_acquire());
        }

        // Should not be able to acquire more
        assert!(!limiter.try_acquire());

        // Wait for refill
        sleep(Duration::from_millis(100)).await;

        // Should be able to acquire ~100 more tokens (1000 TPS * 0.1s)
        let mut acquired = 0;
        for _ in 0..150 {
            if limiter.try_acquire() {
                acquired += 1;
            }
        }

        // Should have acquired around 100, allow some variance
        assert!((95..=105).contains(&acquired), "acquired: {}", acquired);
    }

    #[tokio::test]
    async fn test_rate_limiter_batch() {
        let mut limiter = RateLimiter::new(1000);

        // Should be able to acquire batch of 10
        assert!(limiter.try_acquire_batch(10));

        // Drain remaining
        for _ in 0..99 {
            limiter.try_acquire_batch(10);
        }

        // Should not be able to acquire more
        assert!(!limiter.try_acquire_batch(10));
    }

    #[tokio::test]
    async fn test_rate_limiter_set_rate() {
        let mut limiter = RateLimiter::new(100);

        // Change rate
        limiter.set_rate(1000);

        // Should work at new rate
        assert!(limiter.try_acquire());
    }
}
