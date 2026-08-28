//! Retention: deleting data once it has served its purpose.
//!
//! Two rules protect the user's history from a configuration mistake:
//!
//! * a tier is never pruned before the tier that is built *from* it has caught up, so
//!   deleting raw data cannot destroy samples that were never rolled up;
//! * retention windows are validated for monotonicity when configuration is loaded, so
//!   a coarser tier can never be kept for less time than a finer one.

use crate::config::RetentionSettings;
use crate::scheduler::JobOutcome;
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use vds_domain::metrics::Resolution;
use vds_domain::ports::{
    AlertRepository, AnalyticsRepository, Clock, EventRepository, MetricsRepository,
    WebsiteRepository,
};

/// What one retention run deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RetentionReport {
    pub raw_samples: u64,
    pub five_minute_rollups: u64,
    pub hourly_rollups: u64,
    pub daily_rollups: u64,
    pub events: u64,
    pub incidents: u64,
    pub website_checks: u64,
    pub analytics: u64,
}

impl RetentionReport {
    pub fn total(&self) -> u64 {
        self.raw_samples
            + self.five_minute_rollups
            + self.hourly_rollups
            + self.daily_rollups
            + self.events
            + self.incidents
            + self.website_checks
            + self.analytics
    }
}

/// Applies retention policy across every store.
pub struct RetentionService {
    metrics: Arc<dyn MetricsRepository>,
    events: Arc<dyn EventRepository>,
    alerts: Arc<dyn AlertRepository>,
    websites: Arc<dyn WebsiteRepository>,
    analytics: Arc<dyn AnalyticsRepository>,
    clock: Arc<dyn Clock>,
    settings: RetentionSettings,
}

impl RetentionService {
    pub fn new(
        metrics: Arc<dyn MetricsRepository>,
        events: Arc<dyn EventRepository>,
        alerts: Arc<dyn AlertRepository>,
        websites: Arc<dyn WebsiteRepository>,
        analytics: Arc<dyn AnalyticsRepository>,
        clock: Arc<dyn Clock>,
        settings: RetentionSettings,
    ) -> Self {
        Self {
            metrics,
            events,
            alerts,
            websites,
            analytics,
            clock,
            settings,
        }
    }

    pub fn settings(&self) -> &RetentionSettings {
        &self.settings
    }

    /// Runs retention once.
    pub async fn run(&self) -> RetentionReport {
        let now = self.clock.now();

        let mut report = RetentionReport {
            raw_samples: self
                .prune_tier(Resolution::Raw, self.settings.raw_days, now)
                .await,
            five_minute_rollups: self
                .prune_tier(Resolution::FiveMinutes, self.settings.five_minute_days, now)
                .await,
            hourly_rollups: self
                .prune_tier(Resolution::OneHour, self.settings.hourly_days, now)
                .await,
            ..Default::default()
        };

        // Zero means keep forever.
        if self.settings.daily_days > 0 {
            report.daily_rollups = self
                .prune_tier(Resolution::OneDay, self.settings.daily_days, now)
                .await;
        }

        report.events = self
            .attempt(
                "events",
                self.events.prune(cutoff(now, self.settings.events_days)),
            )
            .await;
        report.incidents = self
            .attempt(
                "incidents",
                self.alerts
                    .prune_incidents(cutoff(now, self.settings.incidents_days)),
            )
            .await;
        report.website_checks = self
            .attempt(
                "website checks",
                self.websites
                    .prune_checks(cutoff(now, self.settings.website_checks_days)),
            )
            .await;
        report.analytics = self
            .attempt(
                "analytics",
                self.analytics
                    .prune(cutoff(now, self.settings.analytics_days)),
            )
            .await;

        report
    }

    /// Prunes one metric tier, but never past the point its successor has aggregated.
    ///
    /// Without this guard, a run that happened while aggregation was lagging — after a
    /// long shutdown, say — would delete raw samples that had never been rolled up,
    /// silently punching a permanent hole in the history.
    async fn prune_tier(&self, resolution: Resolution, days: u32, now: DateTime<Utc>) -> u64 {
        let mut cutoff = cutoff(now, days);

        if let Some(successor) = successor_of(resolution) {
            match self.metrics.last_rollup_bucket(successor).await {
                Ok(Some(last_bucket)) => {
                    // Do not delete anything the successor tier has not consumed yet.
                    cutoff = cutoff.min(last_bucket);
                }
                Ok(None) => {
                    // The successor tier has never run. Deleting the source now would
                    // lose the data permanently.
                    tracing::debug!(
                        resolution = resolution.as_str(),
                        "skipping prune: the next tier has not been built yet"
                    );
                    return 0;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "could not check aggregation progress; skipping prune");
                    return 0;
                }
            }
        }

        self.attempt(resolution.as_str(), self.metrics.prune(resolution, cutoff))
            .await
    }

    /// Runs one prune, logging rather than propagating failures.
    ///
    /// Retention is housekeeping: a failure means data sticks around a while longer,
    /// which is never worth interrupting monitoring for.
    async fn attempt<E: std::fmt::Display>(
        &self,
        what: &str,
        operation: impl std::future::Future<Output = Result<u64, E>>,
    ) -> u64 {
        match operation.await {
            Ok(count) => count,
            Err(err) => {
                tracing::warn!(target_store = what, error = %err, "retention pass failed");
                0
            }
        }
    }

    /// Runs as a scheduled job.
    pub async fn run_as_job(&self) -> JobOutcome {
        self.run().await;
        JobOutcome::Success
    }
}

/// The tier built from this one.
fn successor_of(resolution: Resolution) -> Option<Resolution> {
    match resolution {
        Resolution::Raw => Some(Resolution::FiveMinutes),
        Resolution::FiveMinutes => Some(Resolution::OneHour),
        Resolution::OneHour => Some(Resolution::OneDay),
        Resolution::OneDay => None,
    }
}

fn cutoff(now: DateTime<Utc>, days: u32) -> DateTime<Utc> {
    now - Duration::days(i64::from(days))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        FakeAlertRepository, FakeAnalyticsRepository, FakeEventRepository, FakeMetricsRepository,
        FakeWebsiteRepository,
    };
    use vds_domain::events::{DomainEvent, EventEnvelope};
    use vds_domain::ids::ServerId;
    use vds_domain::metrics::{MetricKind, MetricSample, Resolution, TimeWindow};
    use vds_domain::ports::FixedClock;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    struct Harness {
        service: RetentionService,
        metrics: Arc<FakeMetricsRepository>,
        events: Arc<FakeEventRepository>,
    }

    fn harness(now: DateTime<Utc>, settings: RetentionSettings) -> Harness {
        let metrics = Arc::new(FakeMetricsRepository::new());
        let events = Arc::new(FakeEventRepository::new());
        let alerts = Arc::new(FakeAlertRepository::new());
        let websites = Arc::new(FakeWebsiteRepository::new());
        let analytics = Arc::new(FakeAnalyticsRepository::new());

        let service = RetentionService::new(
            Arc::clone(&metrics) as Arc<dyn MetricsRepository>,
            Arc::clone(&events) as Arc<dyn EventRepository>,
            alerts,
            websites,
            analytics,
            Arc::new(FixedClock::new(now)),
            settings,
        );

        Harness {
            service,
            metrics,
            events,
        }
    }

    /// Value used by `mark_aggregated`, kept distinct from any value a test asserts on.
    const AGGREGATION_MARKER: f64 = -12_345.0;

    /// Marks the successor tier as having caught up to `bucket`.
    async fn mark_aggregated(
        metrics: &FakeMetricsRepository,
        resolution: Resolution,
        server: ServerId,
        bucket: DateTime<Utc>,
    ) {
        metrics
            .record_samples(&[MetricSample {
                server_id: server,
                kind: MetricKind::CpuUsage,
                value: AGGREGATION_MARKER,
                timestamp: bucket,
            }])
            .await
            .expect("stored");
        metrics
            .build_rollups(
                resolution,
                TimeWindow::new(bucket - Duration::hours(1), bucket + Duration::hours(1)),
            )
            .await
            .expect("rollups built");
    }

    #[tokio::test]
    async fn old_raw_samples_are_deleted_once_they_have_been_rolled_up() {
        let now = at(86_400 * 30);
        let h = harness(
            now,
            RetentionSettings {
                raw_days: 7,
                ..Default::default()
            },
        );
        let server = ServerId::new();

        // Aggregation has caught up to now.
        mark_aggregated(&h.metrics, Resolution::FiveMinutes, server, now).await;

        h.metrics
            .record_samples(&[
                MetricSample {
                    server_id: server,
                    kind: MetricKind::CpuUsage,
                    value: 1.0,
                    timestamp: now - Duration::days(20),
                },
                MetricSample {
                    server_id: server,
                    kind: MetricKind::CpuUsage,
                    value: 2.0,
                    timestamp: now - Duration::days(2),
                },
            ])
            .await
            .expect("stored");

        let report = h.service.run().await;
        assert!(report.raw_samples >= 1);

        let remaining = h.metrics.samples();
        assert!(
            remaining
                .iter()
                .all(|s| s.timestamp >= now - Duration::days(7))
        );
        assert!(
            remaining
                .iter()
                .any(|s| s.timestamp == now - Duration::days(2))
        );
    }

    #[tokio::test]
    async fn raw_data_is_not_deleted_before_it_has_been_aggregated() {
        // The dangerous case: the app was shut down for a fortnight, aggregation has not
        // run, and retention fires first. Deleting here loses the data forever.
        let now = at(86_400 * 30);
        let h = harness(
            now,
            RetentionSettings {
                raw_days: 7,
                ..Default::default()
            },
        );
        let server = ServerId::new();

        h.metrics
            .record_samples(&[MetricSample {
                server_id: server,
                kind: MetricKind::CpuUsage,
                value: 1.0,
                timestamp: now - Duration::days(20),
            }])
            .await
            .expect("stored");

        let report = h.service.run().await;
        assert_eq!(
            report.raw_samples, 0,
            "nothing may be pruned before aggregation runs"
        );
        assert_eq!(h.metrics.samples().len(), 1);
    }

    #[tokio::test]
    async fn pruning_stops_at_the_point_aggregation_reached() {
        let now = at(86_400 * 30);
        let h = harness(
            now,
            RetentionSettings {
                raw_days: 7,
                ..Default::default()
            },
        );
        let server = ServerId::new();

        // Aggregation only caught up to 15 days ago.
        mark_aggregated(
            &h.metrics,
            Resolution::FiveMinutes,
            server,
            now - Duration::days(15),
        )
        .await;

        h.metrics
            .record_samples(&[
                MetricSample {
                    server_id: server,
                    kind: MetricKind::CpuUsage,
                    value: 1.0,
                    timestamp: now - Duration::days(20),
                },
                MetricSample {
                    server_id: server,
                    kind: MetricKind::CpuUsage,
                    value: 2.0,
                    timestamp: now - Duration::days(10),
                },
            ])
            .await
            .expect("stored");

        h.service.run().await;

        let remaining = h.metrics.samples();
        // The 20-day-old sample is aggregated and past retention: gone.
        assert!(
            !remaining
                .iter()
                .any(|s| s.timestamp == now - Duration::days(20))
        );
        // The 10-day-old one is past retention but *not* aggregated: kept.
        assert!(
            remaining
                .iter()
                .any(|s| s.timestamp == now - Duration::days(10))
        );
    }

    #[tokio::test]
    async fn daily_rollups_are_kept_forever_when_configured_as_zero() {
        let now = at(86_400 * 800);
        let h = harness(
            now,
            RetentionSettings {
                daily_days: 0,
                ..Default::default()
            },
        );
        let report = h.service.run().await;
        assert_eq!(report.daily_rollups, 0);
    }

    #[tokio::test]
    async fn old_events_are_pruned() {
        let now = at(86_400 * 200);
        let h = harness(
            now,
            RetentionSettings {
                events_days: 90,
                ..Default::default()
            },
        );

        h.events
            .append(&EventEnvelope::new(
                DomainEvent::ScreenshotUpdated {
                    website_id: vds_domain::ids::WebsiteId::new(),
                },
                now - Duration::days(120),
            ))
            .await
            .expect("appended");
        h.events
            .append(&EventEnvelope::new(
                DomainEvent::ScreenshotUpdated {
                    website_id: vds_domain::ids::WebsiteId::new(),
                },
                now - Duration::days(10),
            ))
            .await
            .expect("appended");

        let report = h.service.run().await;
        assert_eq!(report.events, 1);
        assert_eq!(h.events.all().len(), 1);
    }

    #[tokio::test]
    async fn a_fresh_installation_prunes_nothing_and_does_not_fail() {
        let h = harness(at(1_000), RetentionSettings::default());
        let report = h.service.run().await;
        assert_eq!(report.total(), 0);
    }

    #[tokio::test]
    async fn the_job_wrapper_always_succeeds_because_retention_is_housekeeping() {
        let h = harness(at(1_000), RetentionSettings::default());
        assert_eq!(h.service.run_as_job().await, JobOutcome::Success);
    }

    #[test]
    fn every_tier_except_the_coarsest_has_a_successor() {
        assert_eq!(successor_of(Resolution::Raw), Some(Resolution::FiveMinutes));
        assert_eq!(
            successor_of(Resolution::FiveMinutes),
            Some(Resolution::OneHour)
        );
        assert_eq!(successor_of(Resolution::OneHour), Some(Resolution::OneDay));
        assert_eq!(successor_of(Resolution::OneDay), None);
    }
}
