//! Client-side token-bucket rate limiting.
//!
//! Two independent buckets per endpoint key — one for the per-minute limit,
//! one for the per-day limit. `acquire()` waits until both can yield a token,
//! then deducts atomically. Uses `std::sync::Mutex` (never held across `.await`).
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::endpoint::EndpointKey;

#[derive(Debug, Clone, Copy)]
pub struct RateLimit {
    pub per_minute: Option<u32>,
    pub per_day: Option<u32>,
}

impl RateLimit {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            per_minute: None,
            per_day: None,
        }
    }

    #[must_use]
    pub const fn per_minute(n: u32) -> Self {
        Self {
            per_minute: Some(n),
            per_day: None,
        }
    }

    #[must_use]
    pub const fn with_day(per_minute: u32, per_day: u32) -> Self {
        Self {
            per_minute: Some(per_minute),
            per_day: Some(per_day),
        }
    }
}

#[derive(Debug)]
struct Bucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
}

impl Bucket {
    fn new(capacity: u32, window: Duration) -> Self {
        let cap = f64::from(capacity);
        Self {
            capacity: cap,
            refill_per_sec: cap / window.as_secs_f64(),
            tokens: cap,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        self.last_refill = now;
    }

    fn wait_for_one(&self) -> Duration {
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / self.refill_per_sec)
        }
    }
}

type BucketPair = (Option<Bucket>, Option<Bucket>);

#[derive(Debug)]
pub(crate) struct EndpointLimiter {
    buckets: Mutex<HashMap<EndpointKey, BucketPair>>,
}

impl EndpointLimiter {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Block until a token can be deducted from both the minute and day buckets
    /// for `key` (if those buckets exist). Returns immediately if the endpoint
    /// has no rate limit.
    pub async fn acquire(&self, key: EndpointKey) {
        loop {
            let wait = self.try_take_or_wait(key);
            if wait.is_zero() {
                return;
            }
            tokio::time::sleep(wait).await;
        }
    }

    fn try_take_or_wait(&self, key: EndpointKey) -> Duration {
        let mut buckets = self.buckets.lock().expect("rate-limit mutex poisoned");
        let entry = buckets.entry(key).or_insert_with(|| {
            let rl = key.rate_limit();
            (
                rl.per_minute
                    .map(|c| Bucket::new(c, Duration::from_secs(60))),
                rl.per_day
                    .map(|c| Bucket::new(c, Duration::from_secs(86_400))),
            )
        });

        let now = Instant::now();
        if let Some(b) = &mut entry.0 {
            b.refill(now);
        }
        if let Some(b) = &mut entry.1 {
            b.refill(now);
        }

        let wait_min = entry
            .0
            .as_ref()
            .map_or(Duration::ZERO, Bucket::wait_for_one);
        let wait_day = entry
            .1
            .as_ref()
            .map_or(Duration::ZERO, Bucket::wait_for_one);

        if wait_min.is_zero() && wait_day.is_zero() {
            if let Some(b) = &mut entry.0 {
                b.tokens -= 1.0;
            }
            if let Some(b) = &mut entry.1 {
                b.tokens -= 1.0;
            }
            Duration::ZERO
        } else {
            wait_min.max(wait_day)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlimited_endpoint_returns_immediately() {
        let limiter = EndpointLimiter::new();
        let start = Instant::now();
        limiter.acquire(EndpointKey::AuthLogin).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn capped_endpoint_drains_then_waits() {
        let limiter = EndpointLimiter::new();
        // EntriesCreate: 2/min, 10/day. Take both tokens with no wait.
        let start = Instant::now();
        limiter.acquire(EndpointKey::EntriesCreate).await;
        limiter.acquire(EndpointKey::EntriesCreate).await;
        assert!(start.elapsed() < Duration::from_millis(50));

        // Third call must wait (rough check: refill is 2/60s = ~30s/token).
        let wait_estimate = {
            let buckets = limiter.buckets.lock().unwrap();
            let entry = buckets.get(&EndpointKey::EntriesCreate).unwrap();
            entry.0.as_ref().unwrap().wait_for_one()
        };
        assert!(
            wait_estimate > Duration::from_secs(20),
            "expected ~30s wait, got {wait_estimate:?}"
        );
    }
}
