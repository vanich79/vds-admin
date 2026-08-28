//! The time port.
//!
//! Everything that needs "now" takes a [`Clock`]. That is what lets the alert engine's
//! five-minute holds, the offline detector's failure streaks and the retention policy
//! be tested deterministically instead of with `sleep`.

use chrono::{DateTime, NaiveDate, Utc};

/// A source of the current time.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    /// Today's date in the local timezone.
    ///
    /// Analytics providers work in whole local days, so this is separate from `now()`
    /// rather than derived from it — deriving it in UTC produces off-by-one traffic
    /// reports for anyone east or west of Greenwich.
    fn today_local(&self) -> NaiveDate;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn today_local(&self) -> NaiveDate {
        chrono::Local::now().date_naive()
    }
}

/// A clock the tests drive by hand.
///
/// Lives here rather than in a test module because every crate's tests need it.
#[derive(Debug, Clone)]
pub struct FixedClock {
    now: std::sync::Arc<std::sync::Mutex<DateTime<Utc>>>,
}

impl FixedClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: std::sync::Arc::new(std::sync::Mutex::new(now)),
        }
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: chrono::Duration) {
        if let Ok(mut guard) = self.now.lock() {
            *guard += by;
        }
    }

    pub fn set(&self, to: DateTime<Utc>) {
        if let Ok(mut guard) = self.now.lock() {
            *guard = to;
        }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        // A poisoned mutex here means a test panicked while holding it; falling back to
        // the epoch keeps the failure readable instead of cascading into more panics.
        self.now
            .lock()
            .map(|guard| *guard)
            .unwrap_or(DateTime::UNIX_EPOCH)
    }

    fn today_local(&self) -> NaiveDate {
        self.now().date_naive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn the_fixed_clock_only_moves_when_told_to() {
        let clock = FixedClock::new(DateTime::UNIX_EPOCH);
        assert_eq!(clock.now(), DateTime::UNIX_EPOCH);
        assert_eq!(clock.now(), DateTime::UNIX_EPOCH);

        clock.advance(Duration::hours(2));
        assert_eq!(clock.now(), DateTime::UNIX_EPOCH + Duration::hours(2));
    }

    #[test]
    fn the_system_clock_advances() {
        let clock = SystemClock;
        assert!(clock.now() > DateTime::UNIX_EPOCH);
    }
}
