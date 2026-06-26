//! Token-bucket rate limiter.
//!
//! Each provider gets its own bucket. Buckets refill at a configurable
//! rate (tokens/sec) up to a burst capacity. Requests that exceed the
//! limit are rejected with HTTP 429.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
    rate: f64,   // tokens per second
    capacity: f64, // max tokens (burst)
}

impl Bucket {
    fn new(rate: f64, capacity: f64) -> Self {
        Bucket {
            tokens: capacity,
            last_refill: Instant::now(),
            rate,
            capacity,
        }
    }

    /// Try to consume one token. Returns true if allowed.
    fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.last_refill = now;
    }
}

/// Thread-safe token-bucket rate limiter keyed by provider name.
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
    default_rate: f64,
    default_capacity: f64,
}

impl RateLimiter {
    pub fn new(rate: f64, capacity: f64) -> Self {
        RateLimiter {
            buckets: Mutex::new(HashMap::new()),
            default_rate: rate,
            default_capacity: capacity,
        }
    }

    /// Check if a request from the given provider is allowed.
    /// Returns true if allowed, false if rate-limited.
    pub fn allow(&self, provider: &str) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        let bucket = buckets
            .entry(provider.to_string())
            .or_insert_with(|| Bucket::new(self.default_rate, self.default_capacity));
        bucket.try_consume()
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        // Default: 10 requests/sec, burst 20
        RateLimiter::new(10.0, 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_allows_up_to_capacity() {
        let mut b = Bucket::new(10.0, 5.0);
        for _ in 0..5 {
            assert!(b.try_consume());
        }
        assert!(!b.try_consume()); // 6th fails
    }

    #[test]
    fn test_rate_limiter_per_provider() {
        let rl = RateLimiter::new(100.0, 3.0);
        assert!(rl.allow("deepseek"));
        assert!(rl.allow("deepseek"));
        assert!(rl.allow("deepseek"));
        assert!(!rl.allow("deepseek")); // exceeded
        assert!(rl.allow("anthropic")); // separate bucket
    }
}
