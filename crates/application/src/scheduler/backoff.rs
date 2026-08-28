//! Exponential backoff with jitter.
//!
//! Kept as a pure value type with no clock and no randomness source of its own, so the
//! retry behaviour of every subsystem can be asserted exactly.

use chrono::Duration;

/// Backoff policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    /// Delay after the first failure.
    pub initial_secs: u32,
    /// Delay is never longer than this.
    pub max_secs: u32,
    /// Multiplier applied per consecutive failure.
    pub multiplier: u32,
    /// Random spread, as a percentage of the computed delay.
    ///
    /// Without it, a hundred servers that all went down together would all retry in the
    /// same instant, forever.
    pub jitter_percent: u32,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial_secs: 5,
            max_secs: 300,
            multiplier: 2,
            jitter_percent: 20,
        }
    }
}

impl BackoffPolicy {
    /// A policy that never waits, for tests and for jobs that must not be delayed.
    pub const IMMEDIATE: BackoffPolicy = BackoffPolicy {
        initial_secs: 0,
        max_secs: 0,
        multiplier: 1,
        jitter_percent: 0,
    };

    /// Base delay after `failures` consecutive failures, before jitter.
    ///
    /// `failures == 0` means nothing has failed, so there is no delay.
    pub fn base_delay(&self, failures: u32) -> Duration {
        if failures == 0 {
            return Duration::zero();
        }
        let exponent = failures.saturating_sub(1).min(32);
        // Saturating throughout: a server offline for a week must not overflow into a
        // negative delay and start hammering.
        let delay = u64::from(self.initial_secs)
            .saturating_mul(u64::from(self.multiplier).saturating_pow(exponent));
        let capped = delay.min(u64::from(self.max_secs));
        Duration::seconds(capped as i64)
    }

    /// Delay including jitter.
    ///
    /// `jitter_fraction` must be in `0.0..=1.0`; the caller supplies it so this stays a
    /// pure function. In production it comes from a random source; in tests it is fixed.
    pub fn delay(&self, failures: u32, jitter_fraction: f64) -> Duration {
        let base = self.base_delay(failures);
        if self.jitter_percent == 0 || base.is_zero() {
            return base;
        }
        let fraction = jitter_fraction.clamp(0.0, 1.0);
        let spread = base.num_seconds() as f64 * f64::from(self.jitter_percent) / 100.0;
        // Jitter is symmetric around the base delay: [base - spread, base + spread].
        let offset = (fraction * 2.0 - 1.0) * spread;
        let seconds = (base.num_seconds() as f64 + offset).max(0.0);
        Duration::seconds(seconds.round() as i64)
    }

    /// Whether the policy has reached its ceiling at this failure count.
    pub fn is_saturated(&self, failures: u32) -> bool {
        self.base_delay(failures).num_seconds() >= i64::from(self.max_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: BackoffPolicy = BackoffPolicy {
        initial_secs: 5,
        max_secs: 300,
        multiplier: 2,
        jitter_percent: 20,
    };

    #[test]
    fn no_failures_means_no_delay() {
        assert_eq!(POLICY.base_delay(0), Duration::zero());
    }

    #[test]
    fn the_delay_doubles_with_each_failure() {
        assert_eq!(POLICY.base_delay(1), Duration::seconds(5));
        assert_eq!(POLICY.base_delay(2), Duration::seconds(10));
        assert_eq!(POLICY.base_delay(3), Duration::seconds(20));
        assert_eq!(POLICY.base_delay(4), Duration::seconds(40));
    }

    #[test]
    fn the_delay_is_capped() {
        assert_eq!(POLICY.base_delay(20), Duration::seconds(300));
        assert!(POLICY.is_saturated(20));
        assert!(!POLICY.is_saturated(2));
    }

    #[test]
    fn a_very_long_outage_does_not_overflow_into_a_short_delay() {
        // The failure that a naive `initial * multiplier.pow(n)` would produce: after
        // enough failures the shift wraps and the scheduler starts hammering again.
        for failures in [30_u32, 100, 1_000, u32::MAX] {
            let delay = POLICY.base_delay(failures);
            assert_eq!(delay, Duration::seconds(300), "failures = {failures}");
        }
    }

    #[test]
    fn jitter_spreads_symmetrically_around_the_base_delay() {
        // failures = 3 ⇒ base 20s, ±20% ⇒ 16s..24s
        assert_eq!(POLICY.delay(3, 0.0), Duration::seconds(16));
        assert_eq!(POLICY.delay(3, 0.5), Duration::seconds(20));
        assert_eq!(POLICY.delay(3, 1.0), Duration::seconds(24));
    }

    #[test]
    fn jitter_never_produces_a_negative_delay() {
        let aggressive = BackoffPolicy {
            initial_secs: 1,
            max_secs: 60,
            multiplier: 2,
            jitter_percent: 100,
        };
        assert!(aggressive.delay(1, 0.0) >= Duration::zero());
    }

    #[test]
    fn a_fraction_outside_the_unit_range_is_clamped_rather_than_extrapolated() {
        assert_eq!(POLICY.delay(3, -5.0), POLICY.delay(3, 0.0));
        assert_eq!(POLICY.delay(3, 5.0), POLICY.delay(3, 1.0));
    }

    #[test]
    fn disabling_jitter_gives_the_exact_base_delay() {
        let exact = BackoffPolicy {
            jitter_percent: 0,
            ..POLICY
        };
        assert_eq!(exact.delay(3, 0.0), Duration::seconds(20));
        assert_eq!(exact.delay(3, 1.0), Duration::seconds(20));
    }

    #[test]
    fn the_immediate_policy_never_waits() {
        assert_eq!(BackoffPolicy::IMMEDIATE.delay(10, 0.5), Duration::zero());
    }
}
