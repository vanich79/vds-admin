//! The website monitoring use case.
//!
//! A check is performed by an infrastructure adapter (`vds-infra-web`); this module owns
//! what the result *means*: how it changes the site's status, what gets stored, and what
//! the rest of the system is told about it.

use super::offline::{OfflineDetector, Transition};
use crate::metrics::samples::samples_from_check;
use crate::scheduler::JobOutcome;
use async_trait::async_trait;
use std::sync::Arc;
use vds_domain::Status;
use vds_domain::events::DomainEvent;
use vds_domain::ids::WebsiteId;
use vds_domain::ports::{
    Clock, EventPublisher, MetricsRepository, RepositoryError, WebsiteRepository,
};
use vds_domain::website::{Website, WebsiteCheck, evaluate_check};

/// Performs the network side of a website check.
///
/// A port rather than a concrete client, so the monitor can be tested against scripted
/// responses — including expired certificates and DNS failures — without a network.
#[async_trait]
pub trait WebsiteChecker: Send + Sync {
    async fn check(&self, website: &Website, at: chrono::DateTime<chrono::Utc>) -> WebsiteCheck;
}

/// How many days before expiry to start warning about a certificate.
///
/// Separate from the per-website `Status` threshold: this governs the *event*, which
/// feeds alert rules, while the threshold governs the colour on the dashboard.
const SSL_WARNING_DAYS: i64 = 30;

/// Runs website checks and records what follows from them.
pub struct WebsiteMonitor {
    checker: Arc<dyn WebsiteChecker>,
    websites: Arc<dyn WebsiteRepository>,
    metrics: Arc<dyn MetricsRepository>,
    events: Arc<dyn EventPublisher>,
    clock: Arc<dyn Clock>,
}

impl WebsiteMonitor {
    pub fn new(
        checker: Arc<dyn WebsiteChecker>,
        websites: Arc<dyn WebsiteRepository>,
        metrics: Arc<dyn MetricsRepository>,
        events: Arc<dyn EventPublisher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            checker,
            websites,
            metrics,
            events,
            clock,
        }
    }

    /// Checks one website.
    pub async fn check(&self, website_id: WebsiteId) -> JobOutcome {
        let website = match self.websites.get(website_id).await {
            Ok(website) => website,
            Err(RepositoryError::NotFound { .. }) => return JobOutcome::Skipped,
            Err(err) => return JobOutcome::Retry(format!("could not load website: {err}")),
        };

        if !website.enabled {
            return JobOutcome::Skipped;
        }

        let now = self.clock.now();
        let check = self.checker.check(&website, now).await;
        self.record(&website, check, now).await
    }

    /// Applies a completed check. Separated from [`WebsiteMonitor::check`] so the
    /// interpretation can be tested independently of the transport.
    pub async fn record(
        &self,
        website: &Website,
        check: WebsiteCheck,
        now: chrono::DateTime<chrono::Utc>,
    ) -> JobOutcome {
        let detector = OfflineDetector::new(website.offline_after_failures);
        let health = evaluate_check(website, &check, now);

        let mut state = self
            .websites
            .load_state(website.id)
            .await
            .unwrap_or_else(|_| vds_domain::website::WebsiteRuntimeState::unknown(website.id));

        let transition = if check.is_success() {
            let transition = detector.record_website_success(&mut state, health, now);
            state.response_ms = check.response_ms;
            state.http_status = check.http_status;
            state.ssl_days_remaining = check.ssl.as_ref().map(|s| s.days_remaining(now));
            transition
        } else {
            let reason = check
                .failure
                .as_ref()
                .map(|f| format!("{}: {}", f.stage.as_str(), f.message))
                .unwrap_or_else(|| "check failed".to_owned());

            // A site that answered with the wrong status is *not* offline: the host is
            // up. Record it as a failure for the streak, but let the evaluated status
            // (Critical) stand rather than being overwritten by Offline.
            let mut transition = detector.record_website_failure(&mut state, reason, now);
            if health == Status::Critical {
                state.status = Status::Critical;
                state.http_status = check.http_status;
                state.response_ms = check.response_ms;
                transition.to = Status::Critical;
            }
            transition
        };

        // The check record is stored regardless of outcome: uptime percentages need the
        // failures as much as the successes.
        if let Err(err) = self.websites.record_check(&check).await {
            tracing::warn!(website = %website.id, error = %err, "could not store check");
        }

        if let Some(server_id) = website.server_id {
            let samples = samples_from_check(&check, server_id, now);
            if !samples.is_empty()
                && let Err(err) = self.metrics.record_samples(&samples).await
            {
                tracing::warn!(website = %website.id, error = %err, "could not store samples");
            }
        }

        if let Err(err) = self.websites.save_state(&state).await {
            return JobOutcome::Retry(format!("could not save website state: {err}"));
        }

        self.publish(website.id, &transition, &check, now);

        if check.is_success() {
            JobOutcome::Success
        } else {
            JobOutcome::Retry("check failed".into())
        }
    }

    fn publish(
        &self,
        website_id: WebsiteId,
        transition: &Transition,
        check: &WebsiteCheck,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        if transition.changed() {
            self.events.publish(DomainEvent::WebsiteStatusChanged {
                website_id,
                from: transition.from,
                to: transition.to,
                reason: transition.reason.clone(),
            });
        }

        self.events.publish(DomainEvent::WebsiteChecked {
            website_id,
            status: transition.to,
            response_ms: check.response_ms,
        });

        if let Some(ssl) = &check.ssl {
            let days = ssl.days_remaining(now);
            if days <= SSL_WARNING_DAYS {
                self.events.publish(DomainEvent::SslExpiringSoon {
                    website_id,
                    days_remaining: days,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMetricsRepository, FakeWebsiteRepository};
    use chrono::{DateTime, Utc};
    use parking_lot::Mutex;
    use vds_domain::ports::{FixedClock, RecordingEventPublisher};
    use vds_domain::website::{CheckStage, SslInfo};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    /// Returns scripted check results.
    #[derive(Default)]
    struct ScriptedChecker {
        response: Mutex<Option<WebsiteCheck>>,
    }

    impl ScriptedChecker {
        fn respond(&self, check: WebsiteCheck) {
            *self.response.lock() = Some(check);
        }
    }

    #[async_trait]
    impl WebsiteChecker for ScriptedChecker {
        async fn check(&self, website: &Website, at: DateTime<Utc>) -> WebsiteCheck {
            self.response.lock().clone().unwrap_or_else(|| {
                WebsiteCheck::failed(website.id, at, CheckStage::HttpRequest, "unscripted")
            })
        }
    }

    struct Harness {
        monitor: WebsiteMonitor,
        websites: Arc<FakeWebsiteRepository>,
        metrics: Arc<FakeMetricsRepository>,
        events: Arc<RecordingEventPublisher>,
        checker: Arc<ScriptedChecker>,
        website: Website,
    }

    fn harness() -> Harness {
        let mut website = Website::new("Example", "https://example.com/", at(0));
        website.offline_after_failures = 2;
        website.server_id = Some(vds_domain::ids::ServerId::new());

        let websites = Arc::new(FakeWebsiteRepository::new());
        websites.insert(website.clone());
        let metrics = Arc::new(FakeMetricsRepository::new());
        let events = Arc::new(RecordingEventPublisher::new());
        let checker = Arc::new(ScriptedChecker::default());
        let clock = FixedClock::new(at(1_000));

        let monitor = WebsiteMonitor::new(
            Arc::clone(&checker) as Arc<dyn WebsiteChecker>,
            Arc::clone(&websites) as Arc<dyn WebsiteRepository>,
            Arc::clone(&metrics) as Arc<dyn MetricsRepository>,
            Arc::clone(&events) as Arc<dyn EventPublisher>,
            Arc::new(clock),
        );

        Harness {
            monitor,
            websites,
            metrics,
            events,
            checker,
            website,
        }
    }

    fn ok_check(id: WebsiteId, response_ms: u32) -> WebsiteCheck {
        WebsiteCheck {
            website_id: id,
            checked_at: at(1_000),
            status: Status::Healthy,
            resolved_addresses: vec!["93.184.216.34".into()],
            dns_ms: Some(5),
            connect_ms: Some(20),
            response_ms: Some(response_ms),
            http_status: Some(200),
            final_url: None,
            ssl: None,
            failure: None,
        }
    }

    #[tokio::test]
    async fn a_healthy_check_marks_the_site_online_and_stores_the_result() {
        let h = harness();
        h.checker.respond(ok_check(h.website.id, 142));

        assert_eq!(h.monitor.check(h.website.id).await, JobOutcome::Success);

        let state = h.websites.state(h.website.id).expect("state saved");
        assert_eq!(state.status, Status::Healthy);
        assert_eq!(state.response_ms, Some(142));
        assert_eq!(state.http_status, Some(200));
        assert_eq!(h.websites.checks().len(), 1);
        assert!(
            h.metrics
                .samples()
                .iter()
                .any(|s| s.kind == vds_domain::metrics::MetricKind::ResponseTimeMs)
        );
    }

    #[tokio::test]
    async fn a_slow_response_is_a_warning_but_still_a_success() {
        let h = harness();
        h.checker.respond(ok_check(h.website.id, 1_500));
        assert_eq!(h.monitor.check(h.website.id).await, JobOutcome::Success);
        assert_eq!(
            h.websites.state(h.website.id).expect("state").status,
            Status::Warning
        );
    }

    #[tokio::test]
    async fn connection_failures_need_the_threshold_before_offline() {
        let h = harness();
        h.checker.respond(WebsiteCheck::failed(
            h.website.id,
            at(1_000),
            CheckStage::TcpConnection,
            "connection refused",
        ));

        h.monitor.check(h.website.id).await;
        assert_eq!(
            h.websites.state(h.website.id).expect("state").status,
            Status::Unknown
        );

        h.monitor.check(h.website.id).await;
        assert_eq!(
            h.websites.state(h.website.id).expect("state").status,
            Status::Offline
        );
    }

    #[tokio::test]
    async fn a_wrong_http_status_is_critical_immediately_not_offline_eventually() {
        // The host answered, so it is not down — the application is broken, and that is
        // worth saying straight away rather than after two more checks.
        let h = harness();
        let mut check = WebsiteCheck::failed(
            h.website.id,
            at(1_000),
            CheckStage::Expectation,
            "expected 200, got 503",
        );
        check.http_status = Some(503);
        check.response_ms = Some(88);
        h.checker.respond(check);

        h.monitor.check(h.website.id).await;

        let state = h.websites.state(h.website.id).expect("state");
        assert_eq!(state.status, Status::Critical);
        assert_eq!(state.http_status, Some(503));
    }

    #[tokio::test]
    async fn an_expiring_certificate_publishes_a_warning_event() {
        let h = harness();
        let mut check = ok_check(h.website.id, 100);
        check.ssl = Some(SslInfo {
            subject: "CN=example.com".into(),
            issuer: "CN=Test CA".into(),
            not_before: at(0),
            not_after: at(1_000 + 86_400 * 12),
            fingerprint: "ab".into(),
            san: vec![],
        });
        h.checker.respond(check);

        h.monitor.check(h.website.id).await;

        assert!(h.events.contains(|e| matches!(
            e,
            DomainEvent::SslExpiringSoon { days_remaining, .. } if *days_remaining == 12
        )));
        assert_eq!(
            h.websites
                .state(h.website.id)
                .expect("state")
                .ssl_days_remaining,
            Some(12)
        );
    }

    #[tokio::test]
    async fn a_certificate_with_plenty_of_life_left_publishes_nothing() {
        let h = harness();
        let mut check = ok_check(h.website.id, 100);
        check.ssl = Some(SslInfo {
            subject: "CN=example.com".into(),
            issuer: "CN=Test CA".into(),
            not_before: at(0),
            not_after: at(1_000 + 86_400 * 200),
            fingerprint: "ab".into(),
            san: vec![],
        });
        h.checker.respond(check);

        h.monitor.check(h.website.id).await;
        assert!(!h.events.contains(|e| e.kind() == "ssl_expiring_soon"));
    }

    #[tokio::test]
    async fn failed_checks_are_stored_so_uptime_can_be_computed() {
        // Storing only successes would make every site show 100% uptime.
        let h = harness();
        h.checker.respond(WebsiteCheck::failed(
            h.website.id,
            at(1_000),
            CheckStage::DnsResolution,
            "NXDOMAIN",
        ));
        h.monitor.check(h.website.id).await;

        let checks = h.websites.checks();
        assert_eq!(checks.len(), 1);
        assert!(!checks[0].is_success());
    }

    #[tokio::test]
    async fn a_status_change_is_published_once_not_on_every_check() {
        let h = harness();
        h.checker.respond(ok_check(h.website.id, 100));
        h.monitor.check(h.website.id).await;
        h.events.clear();

        h.monitor.check(h.website.id).await;
        assert!(!h.events.contains(|e| e.kind() == "website_status_changed"));
        // The per-check event still fires, for the UI's live view.
        assert!(h.events.contains(|e| e.kind() == "website_checked"));
    }

    #[tokio::test]
    async fn a_disabled_website_is_skipped() {
        let h = harness();
        let mut website = h.website.clone();
        website.enabled = false;
        h.websites.insert(website);

        assert_eq!(h.monitor.check(h.website.id).await, JobOutcome::Skipped);
        assert!(h.websites.checks().is_empty());
    }

    #[tokio::test]
    async fn a_deleted_website_is_skipped_rather_than_failing() {
        let h = harness();
        h.websites.clear();
        assert_eq!(h.monitor.check(h.website.id).await, JobOutcome::Skipped);
    }

    #[tokio::test]
    async fn a_website_not_linked_to_a_server_stores_no_server_metrics() {
        let h = harness();
        let mut website = h.website.clone();
        website.server_id = None;
        h.websites.insert(website);

        h.checker.respond(ok_check(h.website.id, 120));
        h.monitor.check(h.website.id).await;

        assert!(h.metrics.samples().is_empty());
        // The check itself is still recorded.
        assert_eq!(h.websites.checks().len(), 1);
    }
}
