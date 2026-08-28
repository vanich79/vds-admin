//! Rolling raw samples up into coarser tiers.
//!
//! Rollups cascade — raw → 5-minute → hourly → daily — so the cost of aggregation stays
//! constant as history grows, instead of rescanning years of raw data to build a daily
//! bucket. See `docs/adr/005-metrics-storage.md`.

use crate::scheduler::JobOutcome;
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use vds_domain::metrics::{Resolution, TimeWindow};
use vds_domain::ports::{Clock, MetricsRepository};

/// The tiers built, finest first. Order matters: each feeds the next.
const TIERS: &[Resolution] = &[
    Resolution::FiveMinutes,
    Resolution::OneHour,
    Resolution::OneDay,
];

/// How far back to look when a tier has never been built.
///
/// Bounded so that a first run on a database with a year of raw data does not attempt
/// the whole year in one transaction.
const INITIAL_LOOKBACK: Duration = Duration::days(2);

/// Extra margin added to the start of each aggregation window.
///
/// Re-computing the most recent buckets is cheap and idempotent, and it repairs the
/// partial bucket that the previous run necessarily left behind at its boundary.
const OVERLAP: Duration = Duration::hours(1);

/// Builds rollups on a schedule.
pub struct MetricsAggregationService {
    metrics: Arc<dyn MetricsRepository>,
    clock: Arc<dyn Clock>,
}

/// What one aggregation run produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AggregationReport {
    pub five_minute_buckets: u64,
    pub hourly_buckets: u64,
    pub daily_buckets: u64,
}

impl AggregationReport {
    pub fn total(&self) -> u64 {
        self.five_minute_buckets + self.hourly_buckets + self.daily_buckets
    }

    fn record(&mut self, resolution: Resolution, count: u64) {
        match resolution {
            Resolution::FiveMinutes => self.five_minute_buckets += count,
            Resolution::OneHour => self.hourly_buckets += count,
            Resolution::OneDay => self.daily_buckets += count,
            Resolution::Raw => {}
        }
    }
}

impl MetricsAggregationService {
    pub fn new(metrics: Arc<dyn MetricsRepository>, clock: Arc<dyn Clock>) -> Self {
        Self { metrics, clock }
    }

    /// Builds every tier once.
    pub async fn run(&self) -> Result<AggregationReport, String> {
        let now = self.clock.now();
        let mut report = AggregationReport::default();

        // Finest first: the hourly tier reads what the five-minute tier just wrote.
        for resolution in TIERS {
            let window = self.window_for(*resolution, now).await;
            match self.metrics.build_rollups(*resolution, window).await {
                Ok(count) => report.record(*resolution, count),
                Err(err) => {
                    // One tier failing should not abandon the others; the finer tiers
                    // are the ones charts depend on most.
                    tracing::warn!(
                        resolution = resolution.as_str(),
                        error = %err,
                        "rollup tier failed"
                    );
                }
            }
        }

        Ok(report)
    }

    /// The window to aggregate for a tier: from just before the last completed bucket up
    /// to now.
    async fn window_for(&self, resolution: Resolution, now: DateTime<Utc>) -> TimeWindow {
        let last = self
            .metrics
            .last_rollup_bucket(resolution)
            .await
            .ok()
            .flatten();
        let from = match last {
            Some(bucket) => bucket - OVERLAP,
            None => now - INITIAL_LOOKBACK,
        };
        // Never look further back than the initial lookback, so a database restored from
        // an old backup does not trigger an enormous first run.
        let floor = now - INITIAL_LOOKBACK;
        TimeWindow::new(from.max(floor), now)
    }

    /// Runs as a scheduled job.
    pub async fn run_as_job(&self) -> JobOutcome {
        match self.run().await {
            Ok(_) => JobOutcome::Success,
            Err(err) => JobOutcome::Retry(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeMetricsRepository;
    use vds_domain::ids::ServerId;
    use vds_domain::metrics::{MetricKind, MetricSample};
    use vds_domain::ports::FixedClock;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    /// Samples every 30 seconds for `minutes`, with a spike partway through.
    fn samples(server: ServerId, start: DateTime<Utc>, minutes: i64) -> Vec<MetricSample> {
        let mut samples = Vec::new();
        let count = minutes * 2;
        for i in 0..count {
            let value = if i == 3 { 100.0 } else { 10.0 };
            samples.push(MetricSample {
                server_id: server,
                kind: MetricKind::CpuUsage,
                value,
                timestamp: start + Duration::seconds(i * 30),
            });
        }
        samples
    }

    async fn service_with(
        now: DateTime<Utc>,
    ) -> (MetricsAggregationService, Arc<FakeMetricsRepository>) {
        let metrics = Arc::new(FakeMetricsRepository::new());
        let clock = FixedClock::new(now);
        let service = MetricsAggregationService::new(
            Arc::clone(&metrics) as Arc<dyn MetricsRepository>,
            Arc::new(clock),
        );
        (service, metrics)
    }

    #[tokio::test]
    async fn raw_samples_become_five_minute_buckets() {
        let now = at(3_600);
        let (service, metrics) = service_with(now).await;
        let server = ServerId::new();
        metrics
            .record_samples(&samples(server, at(0), 30))
            .await
            .expect("stored");

        let report = service.run().await.expect("aggregation succeeds");
        assert!(report.five_minute_buckets > 0);

        // 30 minutes of data at 5-minute buckets.
        let rollups = metrics
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(0), at(3_600)),
            )
            .await
            .expect("rollups readable");
        assert_eq!(rollups.len(), 6);
    }

    #[tokio::test]
    async fn a_spike_survives_aggregation_in_the_max_column() {
        // The reason rollups store min/max: a 100% spike averaged into a 5-minute bucket
        // of 10% readings would otherwise vanish entirely from every long-range chart.
        let now = at(3_600);
        let (service, metrics) = service_with(now).await;
        let server = ServerId::new();
        metrics
            .record_samples(&samples(server, at(0), 30))
            .await
            .expect("stored");
        service.run().await.expect("aggregation succeeds");

        let rollups = metrics
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(0), at(3_600)),
            )
            .await
            .expect("rollups readable");

        let first = &rollups[0];
        assert_eq!(first.max, 100.0);
        assert_eq!(first.min, 10.0);
        assert!(first.avg > 10.0 && first.avg < 100.0);
    }

    #[tokio::test]
    async fn tiers_cascade_so_hourly_is_built_from_five_minute_not_from_raw() {
        let now = at(86_400);
        let (service, metrics) = service_with(now).await;
        let server = ServerId::new();
        metrics
            .record_samples(&samples(server, now - Duration::hours(3), 180))
            .await
            .expect("stored");

        let report = service.run().await.expect("aggregation succeeds");
        assert!(report.five_minute_buckets > 0);
        assert!(
            report.hourly_buckets > 0,
            "hourly tier must be built in the same run"
        );

        let hourly = metrics
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::OneHour,
                TimeWindow::new(now - Duration::hours(4), now),
            )
            .await
            .expect("rollups readable");
        assert!(!hourly.is_empty());
        // The spike must still be visible three tiers up.
        assert!(hourly.iter().any(|r| r.max == 100.0));
    }

    #[tokio::test]
    async fn aggregation_is_idempotent() {
        // The overlap window deliberately re-computes recent buckets; running twice must
        // not double-count them.
        let now = at(7_200);
        let (service, metrics) = service_with(now).await;
        let server = ServerId::new();
        metrics
            .record_samples(&samples(server, at(3_600), 30))
            .await
            .expect("stored");

        service.run().await.expect("first run");
        let after_first = metrics
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(0), at(7_200)),
            )
            .await
            .expect("rollups");

        service.run().await.expect("second run");
        let after_second = metrics
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(0), at(7_200)),
            )
            .await
            .expect("rollups");

        assert_eq!(after_first.len(), after_second.len());
        assert_eq!(after_first[0].count, after_second[0].count);
        assert_eq!(after_first[0].avg, after_second[0].avg);
    }

    #[tokio::test]
    async fn a_bucket_left_partial_by_the_previous_run_is_repaired() {
        // Run once mid-bucket, then again after more samples arrive: the bucket must end
        // up complete rather than frozen at its partial value.
        let server = ServerId::new();
        let metrics = Arc::new(FakeMetricsRepository::new());
        let clock = FixedClock::new(at(3_600 + 60));
        let service = MetricsAggregationService::new(
            Arc::clone(&metrics) as Arc<dyn MetricsRepository>,
            Arc::new(clock.clone()),
        );

        // One sample in the 3600..3900 bucket.
        metrics
            .record_samples(&[MetricSample {
                server_id: server,
                kind: MetricKind::CpuUsage,
                value: 10.0,
                timestamp: at(3_610),
            }])
            .await
            .expect("stored");
        service.run().await.expect("first run");

        // A second sample lands in the same bucket.
        metrics
            .record_samples(&[MetricSample {
                server_id: server,
                kind: MetricKind::CpuUsage,
                value: 90.0,
                timestamp: at(3_700),
            }])
            .await
            .expect("stored");
        clock.set(at(3_600 + 400));
        service.run().await.expect("second run");

        let rollups = metrics
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(3_600), at(3_900)),
            )
            .await
            .expect("rollups");
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].count, 2, "the partial bucket must be recomputed");
        assert_eq!(rollups[0].max, 90.0);
        assert_eq!(rollups[0].avg, 50.0);
    }

    #[tokio::test]
    async fn an_empty_database_aggregates_nothing_without_failing() {
        let (service, _) = service_with(at(10_000)).await;
        let report = service.run().await.expect("aggregation succeeds");
        assert_eq!(report.total(), 0);
    }

    #[tokio::test]
    async fn a_first_run_over_a_huge_backlog_is_bounded() {
        // Restoring a year-old backup must not attempt a year of aggregation at once.
        let now = at(365 * 86_400);
        let (service, metrics) = service_with(now).await;
        let server = ServerId::new();

        // Samples from a year ago and from today.
        metrics
            .record_samples(&[
                MetricSample {
                    server_id: server,
                    kind: MetricKind::CpuUsage,
                    value: 50.0,
                    timestamp: at(0),
                },
                MetricSample {
                    server_id: server,
                    kind: MetricKind::CpuUsage,
                    value: 60.0,
                    timestamp: now - Duration::minutes(10),
                },
            ])
            .await
            .expect("stored");

        service.run().await.expect("aggregation succeeds");

        let ancient = metrics
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(0), at(86_400)),
            )
            .await
            .expect("rollups");
        assert!(
            ancient.is_empty(),
            "the first run must not sweep the entire history"
        );

        let recent = metrics
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(now - Duration::hours(1), now),
            )
            .await
            .expect("rollups");
        assert_eq!(recent.len(), 1);
    }

    #[tokio::test]
    async fn the_job_wrapper_reports_success() {
        let (service, _) = service_with(at(10_000)).await;
        assert_eq!(service.run_as_job().await, JobOutcome::Success);
    }
}
