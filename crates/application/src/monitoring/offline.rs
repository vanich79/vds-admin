//! Offline detection.
//!
//! A single timed-out check does not mean a server is down — it usually means a packet
//! was lost. Declaring `Offline` takes N consecutive failures, configurable per server,
//! defaulting to 3. Everything here is a pure state transition so the rule can be
//! asserted exactly rather than observed over minutes.

use chrono::{DateTime, Utc};
use vds_domain::Status;
use vds_domain::server::ServerRuntimeState;
use vds_domain::website::WebsiteRuntimeState;

/// What a check produced, from the detector's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    /// Metrics were obtained.
    Reachable,
    /// The subject could not be reached.
    Unreachable(String),
}

/// The status transition a check caused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: Status,
    pub to: Status,
    pub reason: Option<String>,
}

impl Transition {
    pub fn changed(&self) -> bool {
        self.from != self.to
    }
}

/// Decides when repeated failures amount to being offline.
#[derive(Debug, Clone, Copy)]
pub struct OfflineDetector {
    /// Consecutive failures required before declaring `Offline`.
    threshold: u32,
}

impl OfflineDetector {
    pub fn new(threshold: u32) -> Self {
        // Zero would mean "offline before any check has failed", which is nonsense; one
        // is the most aggressive meaningful setting.
        Self {
            threshold: threshold.max(1),
        }
    }

    pub fn threshold(&self) -> u32 {
        self.threshold
    }

    /// Applies a successful check to a server's state.
    ///
    /// `health` is the status derived from the collected metrics, so a reachable but
    /// overloaded server becomes `Warning`, not `Healthy`.
    pub fn record_server_success(
        &self,
        state: &mut ServerRuntimeState,
        health: Status,
        now: DateTime<Utc>,
    ) -> Transition {
        let from = state.status;
        state.consecutive_failures = 0;
        state.last_check = Some(now);
        state.last_success = Some(now);
        state.last_error = None;
        state.status = health;
        Transition {
            from,
            to: health,
            reason: None,
        }
    }

    /// Applies a failed check to a server's state.
    pub fn record_server_failure(
        &self,
        state: &mut ServerRuntimeState,
        error: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Transition {
        let from = state.status;
        let error = error.into();

        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.last_check = Some(now);
        state.last_error = Some(error.clone());

        // Below the threshold the status is Unknown, not Offline and not the previous
        // value: we genuinely do not know, and pretending the last reading still holds
        // would leave a stale CPU figure on the dashboard.
        state.status = if state.consecutive_failures >= self.threshold {
            Status::Offline
        } else {
            Status::Unknown
        };

        // Metrics from a failed check are not merely old, they are unverified. Clearing
        // them is what stops the UI showing "CPU 12%" for a machine that has been
        // unreachable for an hour.
        if state.status == Status::Offline {
            state.cpu_percent = vds_domain::metrics::MetricValue::NotAvailable;
            state.memory_percent = vds_domain::metrics::MetricValue::NotAvailable;
            state.disk_percent = vds_domain::metrics::MetricValue::NotAvailable;
            state.uptime_secs = None;
        }

        Transition {
            from,
            to: state.status,
            reason: Some(error),
        }
    }

    /// Applies a successful website check.
    pub fn record_website_success(
        &self,
        state: &mut WebsiteRuntimeState,
        health: Status,
        now: DateTime<Utc>,
    ) -> Transition {
        let from = state.status;
        state.consecutive_failures = 0;
        state.last_check = Some(now);
        state.last_success = Some(now);
        state.last_error = None;
        state.status = health;
        Transition {
            from,
            to: health,
            reason: None,
        }
    }

    /// Applies a failed website check.
    pub fn record_website_failure(
        &self,
        state: &mut WebsiteRuntimeState,
        error: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Transition {
        let from = state.status;
        let error = error.into();

        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.last_check = Some(now);
        state.last_error = Some(error.clone());
        state.status = if state.consecutive_failures >= self.threshold {
            Status::Offline
        } else {
            Status::Unknown
        };

        if state.status == Status::Offline {
            state.response_ms = None;
            state.http_status = None;
        }

        Transition {
            from,
            to: state.status,
            reason: Some(error),
        }
    }

    /// How many more failures before the subject is declared offline.
    pub fn failures_remaining(&self, consecutive_failures: u32) -> u32 {
        self.threshold.saturating_sub(consecutive_failures)
    }
}

impl Default for OfflineDetector {
    fn default() -> Self {
        Self::new(vds_domain::server::DEFAULT_OFFLINE_AFTER_FAILURES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::ids::{ServerId, WebsiteId};
    use vds_domain::metrics::MetricValue;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn healthy_state() -> ServerRuntimeState {
        let mut state = ServerRuntimeState::unknown(ServerId::new());
        state.status = Status::Healthy;
        state.cpu_percent = MetricValue::Available(12.0);
        state.memory_percent = MetricValue::Available(40.0);
        state.disk_percent = MetricValue::Available(55.0);
        state.uptime_secs = Some(86_400);
        state
    }

    #[test]
    fn a_single_failure_does_not_mean_offline() {
        // The whole point: one lost packet must not page anyone.
        let detector = OfflineDetector::new(3);
        let mut state = healthy_state();

        let transition = detector.record_server_failure(&mut state, "timeout", at(0));
        assert_eq!(transition.to, Status::Unknown);
        assert_eq!(state.consecutive_failures, 1);
        assert_eq!(detector.failures_remaining(state.consecutive_failures), 2);
    }

    #[test]
    fn the_nth_consecutive_failure_declares_offline() {
        let detector = OfflineDetector::new(3);
        let mut state = healthy_state();

        detector.record_server_failure(&mut state, "timeout", at(0));
        detector.record_server_failure(&mut state, "timeout", at(30));
        assert_eq!(state.status, Status::Unknown);

        let transition = detector.record_server_failure(&mut state, "timeout", at(60));
        assert_eq!(transition.to, Status::Offline);
        assert!(transition.changed());
        assert_eq!(state.consecutive_failures, 3);
    }

    #[test]
    fn the_default_threshold_is_three() {
        assert_eq!(OfflineDetector::default().threshold(), 3);
    }

    #[test]
    fn a_threshold_of_one_declares_offline_immediately() {
        let detector = OfflineDetector::new(1);
        let mut state = healthy_state();
        assert_eq!(
            detector
                .record_server_failure(&mut state, "refused", at(0))
                .to,
            Status::Offline
        );
    }

    #[test]
    fn a_threshold_of_zero_is_treated_as_one_rather_than_marking_everything_offline() {
        let detector = OfflineDetector::new(0);
        assert_eq!(detector.threshold(), 1);
    }

    #[test]
    fn one_success_clears_the_whole_streak() {
        let detector = OfflineDetector::new(3);
        let mut state = healthy_state();

        detector.record_server_failure(&mut state, "timeout", at(0));
        detector.record_server_failure(&mut state, "timeout", at(30));

        let transition = detector.record_server_success(&mut state, Status::Healthy, at(60));
        assert_eq!(transition.from, Status::Unknown);
        assert_eq!(transition.to, Status::Healthy);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.last_error, None);
        assert_eq!(state.last_success, Some(at(60)));
    }

    #[test]
    fn recovery_from_offline_is_reported_as_a_change() {
        let detector = OfflineDetector::new(2);
        let mut state = healthy_state();
        detector.record_server_failure(&mut state, "down", at(0));
        detector.record_server_failure(&mut state, "down", at(30));
        assert_eq!(state.status, Status::Offline);

        let transition = detector.record_server_success(&mut state, Status::Healthy, at(60));
        assert_eq!(transition.from, Status::Offline);
        assert_eq!(transition.to, Status::Healthy);
        assert!(transition.changed());
    }

    #[test]
    fn stale_metrics_are_cleared_once_a_server_is_offline() {
        // A dashboard showing "CPU 12%" for a machine unreachable for an hour is worse
        // than showing nothing.
        let detector = OfflineDetector::new(2);
        let mut state = healthy_state();

        detector.record_server_failure(&mut state, "timeout", at(0));
        // Not offline yet, so the last reading is still on screen — but as Unknown.
        assert_eq!(state.cpu_percent, MetricValue::Available(12.0));

        detector.record_server_failure(&mut state, "timeout", at(30));
        assert_eq!(state.status, Status::Offline);
        assert_eq!(state.cpu_percent, MetricValue::NotAvailable);
        assert_eq!(state.memory_percent, MetricValue::NotAvailable);
        assert_eq!(state.disk_percent, MetricValue::NotAvailable);
        assert_eq!(state.uptime_secs, None);
    }

    #[test]
    fn a_reachable_but_overloaded_server_is_warning_not_healthy() {
        let detector = OfflineDetector::new(3);
        let mut state = healthy_state();
        let transition = detector.record_server_success(&mut state, Status::Critical, at(0));
        assert_eq!(transition.to, Status::Critical);
        assert_eq!(state.status, Status::Critical);
        // Still reachable, so no failure was recorded.
        assert_eq!(state.consecutive_failures, 0);
    }

    #[test]
    fn an_unchanged_status_is_reported_as_unchanged() {
        // The caller uses this to decide whether to publish an event; publishing on
        // every check would flood the event log.
        let detector = OfflineDetector::new(3);
        let mut state = healthy_state();
        let transition = detector.record_server_success(&mut state, Status::Healthy, at(0));
        assert!(!transition.changed());
    }

    #[test]
    fn a_long_outage_does_not_overflow_the_failure_counter() {
        let detector = OfflineDetector::new(3);
        let mut state = healthy_state();
        state.consecutive_failures = u32::MAX;
        detector.record_server_failure(&mut state, "still down", at(0));
        assert_eq!(state.consecutive_failures, u32::MAX);
        assert_eq!(state.status, Status::Offline);
    }

    #[test]
    fn websites_follow_the_same_rule() {
        let detector = OfflineDetector::new(2);
        let mut state = WebsiteRuntimeState::unknown(WebsiteId::new());
        state.status = Status::Healthy;
        state.response_ms = Some(120);
        state.http_status = Some(200);

        assert_eq!(
            detector.record_website_failure(&mut state, "dns", at(0)).to,
            Status::Unknown
        );
        assert_eq!(state.response_ms, Some(120));

        assert_eq!(
            detector
                .record_website_failure(&mut state, "dns", at(60))
                .to,
            Status::Offline
        );
        assert_eq!(state.response_ms, None);
        assert_eq!(state.http_status, None);
    }

    #[test]
    fn a_website_recovering_clears_its_error() {
        let detector = OfflineDetector::new(1);
        let mut state = WebsiteRuntimeState::unknown(WebsiteId::new());
        detector.record_website_failure(&mut state, "500", at(0));
        assert!(state.last_error.is_some());

        detector.record_website_success(&mut state, Status::Healthy, at(60));
        assert_eq!(state.last_error, None);
        assert_eq!(state.consecutive_failures, 0);
    }
}
