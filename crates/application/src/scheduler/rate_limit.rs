//! Rate limiting for external APIs.
//!
//! A token bucket per key, so one analytics provider running out of quota cannot stall
//! another, and a provider that asks us to back off is obeyed.

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

/// Whether a request may proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateDecision {
    /// Go ahead.
    Allow,
    /// Wait this long first.
    Wait(Duration),
}

impl RateDecision {
    pub fn is_allowed(self) -> bool {
        matches!(self, RateDecision::Allow)
    }

    pub fn delay(self) -> Duration {
        match self {
            RateDecision::Allow => Duration::zero(),
            RateDecision::Wait(d) => d,
        }
    }
}

/// One key's bucket.
#[derive(Debug, Clone)]
struct Bucket {
    capacity: f64,
    tokens: f64,
    /// Tokens added per second.
    refill_rate: f64,
    last_refill: DateTime<Utc>,
    /// Set when the provider explicitly told us to wait until a moment in time.
    penalty_until: Option<DateTime<Utc>>,
}

impl Bucket {
    fn new(per_minute: u32, now: DateTime<Utc>) -> Self {
        let capacity = f64::from(per_minute.max(1));
        Self {
            capacity,
            tokens: capacity,
            refill_rate: capacity / 60.0,
            last_refill: now,
            penalty_until: None,
        }
    }

    fn refill(&mut self, now: DateTime<Utc>) {
        let elapsed = (now - self.last_refill).num_milliseconds();
        if elapsed <= 0 {
            // Time went backwards (a clock correction). Do not add tokens, but move the
            // marker forward so the bucket is not stuck refusing forever.
            self.last_refill = now;
            return;
        }
        let gained = elapsed as f64 / 1_000.0 * self.refill_rate;
        self.tokens = (self.tokens + gained).min(self.capacity);
        self.last_refill = now;
    }
}

/// Token buckets keyed by provider (or by provider and credential).
#[derive(Debug, Clone, Default)]
pub struct RateLimitManager {
    buckets: Arc<Mutex<HashMap<String, Bucket>>>,
}

impl RateLimitManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or reconfigures a key's budget.
    pub fn configure(&self, key: &str, requests_per_minute: u32, now: DateTime<Utc>) {
        let mut buckets = self.buckets.lock();
        buckets.insert(key.to_owned(), Bucket::new(requests_per_minute, now));
    }

    /// Asks permission to make a request, consuming a token if granted.
    ///
    /// A key that was never configured is unlimited: rate limiting is opt-in, so
    /// forgetting to configure a provider does not silently throttle it to nothing.
    pub fn acquire(&self, key: &str, now: DateTime<Utc>) -> RateDecision {
        let mut buckets = self.buckets.lock();
        let Some(bucket) = buckets.get_mut(key) else {
            return RateDecision::Allow;
        };

        if let Some(until) = bucket.penalty_until {
            if now < until {
                return RateDecision::Wait(until - now);
            }
            bucket.penalty_until = None;
            // Refill resumes from the moment the penalty ended, not from when it began.
            // Otherwise a ten-minute 429 penalty silently banks ten minutes of tokens
            // and we burst straight back into being rate limited.
            bucket.last_refill = until;
        }

        bucket.refill(now);

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            RateDecision::Allow
        } else {
            let deficit = 1.0 - bucket.tokens;
            let seconds = if bucket.refill_rate > 0.0 {
                deficit / bucket.refill_rate
            } else {
                60.0
            };
            RateDecision::Wait(Duration::milliseconds((seconds * 1_000.0).ceil() as i64))
        }
    }

    /// Records that the provider asked us to back off, e.g. an HTTP 429.
    ///
    /// Also drains the bucket, so we do not immediately spend whatever tokens had
    /// accumulated the moment the penalty expires.
    pub fn penalise(&self, key: &str, retry_after: Duration, now: DateTime<Utc>) {
        let mut buckets = self.buckets.lock();
        let bucket = buckets
            .entry(key.to_owned())
            .or_insert_with(|| Bucket::new(60, now));
        bucket.penalty_until = Some(now + retry_after.max(Duration::zero()));
        bucket.tokens = 0.0;
        bucket.last_refill = now;
    }

    /// Tokens currently available, for the debug panel.
    pub fn available(&self, key: &str) -> Option<f64> {
        self.buckets.lock().get(key).map(|b| b.tokens)
    }

    /// Forgets a key, e.g. when an integration is deleted.
    pub fn forget(&self, key: &str) {
        self.buckets.lock().remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    #[test]
    fn an_unconfigured_key_is_unlimited() {
        // Forgetting to configure a provider must not throttle it to a standstill.
        let manager = RateLimitManager::new();
        for _ in 0..1_000 {
            assert!(manager.acquire("unknown", at(0)).is_allowed());
        }
    }

    #[test]
    fn a_configured_key_spends_its_budget() {
        let manager = RateLimitManager::new();
        manager.configure("yandex", 5, at(0));

        for i in 0..5 {
            assert!(
                manager.acquire("yandex", at(0)).is_allowed(),
                "request {i} refused"
            );
        }
        assert!(!manager.acquire("yandex", at(0)).is_allowed());
    }

    #[test]
    fn the_bucket_refills_over_time() {
        let manager = RateLimitManager::new();
        manager.configure("yandex", 60, at(0)); // one per second
        for _ in 0..60 {
            assert!(manager.acquire("yandex", at(0)).is_allowed());
        }
        assert!(!manager.acquire("yandex", at(0)).is_allowed());

        // Ten seconds later, ten tokens are back.
        for _ in 0..10 {
            assert!(manager.acquire("yandex", at(10)).is_allowed());
        }
        assert!(!manager.acquire("yandex", at(10)).is_allowed());
    }

    #[test]
    fn a_refusal_says_how_long_to_wait() {
        let manager = RateLimitManager::new();
        manager.configure("yandex", 60, at(0));
        for _ in 0..60 {
            manager.acquire("yandex", at(0));
        }
        let decision = manager.acquire("yandex", at(0));
        // At one token per second, waiting for one token is about one second.
        assert_eq!(decision, RateDecision::Wait(Duration::seconds(1)));
    }

    #[test]
    fn the_bucket_never_overfills() {
        let manager = RateLimitManager::new();
        manager.configure("yandex", 10, at(0));
        // An hour of idleness must not bank 600 requests.
        let available = {
            manager.acquire("yandex", at(3_600));
            manager.available("yandex").expect("configured")
        };
        assert!(available <= 10.0, "banked {available} tokens");
    }

    #[test]
    fn a_provider_penalty_is_obeyed_regardless_of_available_tokens() {
        let manager = RateLimitManager::new();
        manager.configure("yandex", 100, at(0));
        manager.penalise("yandex", Duration::seconds(30), at(0));

        let decision = manager.acquire("yandex", at(10));
        assert_eq!(decision, RateDecision::Wait(Duration::seconds(20)));
    }

    #[test]
    fn the_penalty_lifts_when_it_expires() {
        let manager = RateLimitManager::new();
        manager.configure("yandex", 100, at(0));
        manager.penalise("yandex", Duration::seconds(30), at(0));
        assert!(!manager.acquire("yandex", at(29)).is_allowed());
        assert!(manager.acquire("yandex", at(31)).is_allowed());
    }

    #[test]
    fn a_penalty_drains_the_bucket_so_we_do_not_burst_afterwards() {
        // Otherwise the moment a 429 penalty expires we would fire off every banked
        // token and be rate-limited again immediately.
        let manager = RateLimitManager::new();
        manager.configure("yandex", 60, at(0));
        manager.penalise("yandex", Duration::seconds(10), at(0));

        // One second past the penalty, only ~1 second of refill is available.
        assert!(manager.acquire("yandex", at(11)).is_allowed());
        assert!(!manager.acquire("yandex", at(11)).is_allowed());
    }

    #[test]
    fn penalising_an_unconfigured_key_still_works() {
        let manager = RateLimitManager::new();
        manager.penalise("surprise", Duration::seconds(5), at(0));
        assert!(!manager.acquire("surprise", at(1)).is_allowed());
    }

    #[test]
    fn keys_are_independent() {
        let manager = RateLimitManager::new();
        manager.configure("a", 1, at(0));
        manager.configure("b", 1, at(0));

        assert!(manager.acquire("a", at(0)).is_allowed());
        assert!(!manager.acquire("a", at(0)).is_allowed());
        // Exhausting one provider must not affect another.
        assert!(manager.acquire("b", at(0)).is_allowed());
    }

    #[test]
    fn a_backwards_clock_does_not_wedge_the_bucket() {
        // NTP corrections and laptop suspend/resume both move the clock backwards.
        let manager = RateLimitManager::new();
        manager.configure("yandex", 60, at(1_000));
        assert!(manager.acquire("yandex", at(1_000)).is_allowed());
        assert!(manager.acquire("yandex", at(500)).is_allowed());
        assert!(manager.acquire("yandex", at(1_100)).is_allowed());
    }

    #[test]
    fn forgetting_a_key_makes_it_unlimited_again() {
        let manager = RateLimitManager::new();
        manager.configure("gone", 1, at(0));
        manager.acquire("gone", at(0));
        assert!(!manager.acquire("gone", at(0)).is_allowed());

        manager.forget("gone");
        assert!(manager.acquire("gone", at(0)).is_allowed());
    }
}
