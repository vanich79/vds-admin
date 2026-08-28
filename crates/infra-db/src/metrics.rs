//! SQLite implementation of [`MetricsRepository`], including the rollup cascade.
//!
//! The aggregation itself is done in SQL rather than by reading rows into Rust: a
//! `GROUP BY` over a bucket expression lets SQLite do the work in one pass without
//! materialising millions of samples in memory, which is the whole point of doing it in
//! the database.

use crate::connection::Database;
use crate::convert::*;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::Row;
use vds_domain::ids::ServerId;
use vds_domain::metrics::{
    MetricKind, MetricRollup, MetricSample, MetricSeries, Resolution, SeriesPoint, TimeWindow,
};
use vds_domain::ports::{MetricsRepository, RepositoryError};

/// Stores raw samples and their rollups.
#[derive(Debug, Clone)]
pub struct SqliteMetricsRepository {
    database: Database,
}

impl SqliteMetricsRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

/// Bucket width in milliseconds, for the SQL grouping expression.
fn bucket_millis(resolution: Resolution) -> Option<i64> {
    resolution
        .bucket_width()
        .map(|width| width.num_milliseconds().max(1))
}

fn read_rollup(row: &Row<'_>) -> Result<MetricRollup, rusqlite::Error> {
    let server_id: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let bucket: String = row.get(2)?;
    let bucket_start: i64 = row.get(3)?;

    Ok(MetricRollup {
        server_id: ServerId::from_uuid(
            parse_uuid("metric_rollups.server_id", &server_id).map_err(corrupt)?,
        ),
        kind: MetricKind::parse(&kind).ok_or_else(|| {
            corrupt(RepositoryError::Corrupt(format!(
                "unknown metric kind {kind:?}"
            )))
        })?,
        resolution: Resolution::parse(&bucket).ok_or_else(|| {
            corrupt(RepositoryError::Corrupt(format!(
                "unknown bucket {bucket:?}"
            )))
        })?,
        bucket_start: from_millis(bucket_start).map_err(corrupt)?,
        min: row.get(4)?,
        max: row.get(5)?,
        avg: row.get(6)?,
        sum: row.get(7)?,
        count: row.get::<_, i64>(8)?.max(0) as u32,
    })
}

#[async_trait]
impl MetricsRepository for SqliteMetricsRepository {
    async fn record_samples(&self, samples: &[MetricSample]) -> Result<(), RepositoryError> {
        if samples.is_empty() {
            return Ok(());
        }
        let samples = samples.to_vec();

        // One transaction for the whole batch. A transaction per sample would fsync
        // dozens of times per collection cycle, which is the difference between a
        // negligible write and a visible stall at fleet scale.
        self.database
            .transaction(move |transaction| {
                let mut statement = transaction.prepare_cached(
                    "INSERT INTO metric_samples (server_id, kind, ts, value)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(server_id, kind, ts) DO UPDATE SET value = excluded.value",
                )?;
                for sample in &samples {
                    statement.execute(rusqlite::params![
                        Sql(sample.server_id),
                        sample.kind.as_str(),
                        to_millis(sample.timestamp),
                        sample.value,
                    ])?;
                }
                Ok(())
            })
            .await
    }

    async fn series(
        &self,
        server: ServerId,
        kind: MetricKind,
        window: TimeWindow,
        resolution: Resolution,
    ) -> Result<MetricSeries, RepositoryError> {
        let points = self
            .database
            .call(move |connection| {
                if resolution == Resolution::Raw {
                    let mut statement = connection.prepare(
                        "SELECT ts, value FROM metric_samples
                         WHERE server_id = ?1 AND kind = ?2 AND ts >= ?3 AND ts < ?4
                         ORDER BY ts",
                    )?;
                    let rows = statement.query_map(
                        rusqlite::params![
                            Sql(server),
                            kind.as_str(),
                            to_millis(window.from),
                            to_millis(window.to)
                        ],
                        |row| {
                            let ts: i64 = row.get(0)?;
                            let value: f64 = row.get(1)?;
                            Ok(SeriesPoint::flat(from_millis(ts).map_err(corrupt)?, value))
                        },
                    )?;
                    rows.collect::<Result<Vec<SeriesPoint>, _>>()
                } else {
                    let mut statement = connection.prepare(
                        "SELECT bucket_start, avg_value, min_value, max_value FROM metric_rollups
                         WHERE server_id = ?1 AND kind = ?2 AND bucket = ?3
                           AND bucket_start >= ?4 AND bucket_start < ?5
                         ORDER BY bucket_start",
                    )?;
                    let rows = statement.query_map(
                        rusqlite::params![
                            Sql(server),
                            kind.as_str(),
                            resolution.as_str(),
                            to_millis(window.from),
                            to_millis(window.to)
                        ],
                        |row| {
                            let ts: i64 = row.get(0)?;
                            Ok(SeriesPoint {
                                timestamp: from_millis(ts).map_err(corrupt)?,
                                avg: row.get(1)?,
                                min: row.get(2)?,
                                max: row.get(3)?,
                            })
                        },
                    )?;
                    rows.collect::<Result<Vec<SeriesPoint>, _>>()
                }
            })
            .await?;

        Ok(MetricSeries {
            kind,
            resolution,
            window,
            points,
        })
    }

    async fn latest(
        &self,
        server: ServerId,
        kind: MetricKind,
    ) -> Result<Option<MetricSample>, RepositoryError> {
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT ts, value FROM metric_samples
                     WHERE server_id = ?1 AND kind = ?2
                     ORDER BY ts DESC LIMIT 1",
                )?;
                let mut rows =
                    statement.query_map(rusqlite::params![Sql(server), kind.as_str()], |row| {
                        let ts: i64 = row.get(0)?;
                        Ok(MetricSample {
                            server_id: server,
                            kind,
                            value: row.get(1)?,
                            timestamp: from_millis(ts).map_err(corrupt)?,
                        })
                    })?;
                rows.next().transpose()
            })
            .await
    }

    async fn build_rollups(
        &self,
        resolution: Resolution,
        window: TimeWindow,
    ) -> Result<u64, RepositoryError> {
        let Some(source) = resolution.source() else {
            // Raw data is not built from anything.
            return Ok(0);
        };
        let Some(width) = bucket_millis(resolution) else {
            return Ok(0);
        };

        let from = to_millis(window.from);
        let to = to_millis(window.to);
        let bucket = resolution.as_str();

        self.database
            .transaction(move |transaction| {
                // Bucketing is `ts - rem_euclid(ts, width)`, spelled out as
                // `ts - ((ts % width) + width) % width`. Two reasons for the long form:
                // SQLite's `%` follows the sign of the dividend, so plain `ts - ts % width`
                // rounds pre-epoch timestamps the wrong way; and `FLOOR` is only compiled
                // in when SQLITE_ENABLE_MATH_FUNCTIONS is set, which the vendored build
                // does not set. Integer arithmetic works everywhere.
                let written = if source == Resolution::Raw {
                    transaction.execute(
                        "INSERT INTO metric_rollups
                            (server_id, kind, bucket, bucket_start,
                             min_value, max_value, avg_value, sum_value, sample_count)
                         SELECT server_id, kind, ?1,
                                (ts - ((ts % ?2) + ?2) % ?2),
                                MIN(value), MAX(value), AVG(value), SUM(value), COUNT(*)
                         FROM metric_samples
                         WHERE ts >= ?3 AND ts < ?4
                         GROUP BY server_id, kind,
                                  (ts - ((ts % ?2) + ?2) % ?2)
                         ON CONFLICT(server_id, kind, bucket, bucket_start) DO UPDATE SET
                             min_value = excluded.min_value,
                             max_value = excluded.max_value,
                             avg_value = excluded.avg_value,
                             sum_value = excluded.sum_value,
                             sample_count = excluded.sample_count",
                        rusqlite::params![bucket, width, from, to],
                    )?
                } else {
                    // Building from the finer tier, not from raw: this is what keeps
                    // aggregation cost constant as history grows. The average is
                    // re-weighted by sample count so a bucket built from unequal
                    // sub-buckets is still correct.
                    transaction.execute(
                        "INSERT INTO metric_rollups
                            (server_id, kind, bucket, bucket_start,
                             min_value, max_value, avg_value, sum_value, sample_count)
                         SELECT server_id, kind, ?1,
                                (bucket_start - ((bucket_start % ?2) + ?2) % ?2),
                                MIN(min_value), MAX(max_value),
                                SUM(sum_value) / NULLIF(SUM(sample_count), 0),
                                SUM(sum_value), SUM(sample_count)
                         FROM metric_rollups
                         WHERE bucket = ?5 AND bucket_start >= ?3 AND bucket_start < ?4
                         GROUP BY server_id, kind,
                                  (bucket_start - ((bucket_start % ?2) + ?2) % ?2)
                         ON CONFLICT(server_id, kind, bucket, bucket_start) DO UPDATE SET
                             min_value = excluded.min_value,
                             max_value = excluded.max_value,
                             avg_value = excluded.avg_value,
                             sum_value = excluded.sum_value,
                             sample_count = excluded.sample_count",
                        rusqlite::params![bucket, width, from, to, source.as_str()],
                    )?
                };
                Ok(written as u64)
            })
            .await
    }

    async fn rollups(
        &self,
        server: ServerId,
        kind: MetricKind,
        resolution: Resolution,
        window: TimeWindow,
    ) -> Result<Vec<MetricRollup>, RepositoryError> {
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT server_id, kind, bucket, bucket_start, min_value, max_value,
                            avg_value, sum_value, sample_count
                     FROM metric_rollups
                     WHERE server_id = ?1 AND kind = ?2 AND bucket = ?3
                       AND bucket_start >= ?4 AND bucket_start < ?5
                     ORDER BY bucket_start",
                )?;
                statement
                    .query_map(
                        rusqlite::params![
                            Sql(server),
                            kind.as_str(),
                            resolution.as_str(),
                            to_millis(window.from),
                            to_millis(window.to)
                        ],
                        read_rollup,
                    )?
                    .collect()
            })
            .await
    }

    async fn last_rollup_bucket(
        &self,
        resolution: Resolution,
    ) -> Result<Option<DateTime<Utc>>, RepositoryError> {
        self.database
            .call(move |connection| {
                let latest: Option<i64> = connection.query_row(
                    "SELECT MAX(bucket_start) FROM metric_rollups WHERE bucket = ?1",
                    [resolution.as_str()],
                    |row| row.get(0),
                )?;
                latest
                    .map(|millis| from_millis(millis).map_err(corrupt))
                    .transpose()
            })
            .await
    }

    async fn prune(
        &self,
        resolution: Resolution,
        before: DateTime<Utc>,
    ) -> Result<u64, RepositoryError> {
        let cutoff = to_millis(before);
        self.database
            .call(move |connection| {
                let deleted = if resolution == Resolution::Raw {
                    connection.execute("DELETE FROM metric_samples WHERE ts < ?1", [cutoff])?
                } else {
                    connection.execute(
                        "DELETE FROM metric_rollups WHERE bucket = ?1 AND bucket_start < ?2",
                        rusqlite::params![resolution.as_str(), cutoff],
                    )?
                };
                Ok(deleted as u64)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    async fn repository() -> SqliteMetricsRepository {
        let database = Database::open_in_memory().await.expect("opens");
        SqliteMetricsRepository::new(database)
    }

    fn sample(
        server: ServerId,
        kind: MetricKind,
        value: f64,
        at_time: DateTime<Utc>,
    ) -> MetricSample {
        MetricSample {
            server_id: server,
            kind,
            value,
            timestamp: at_time,
        }
    }

    #[tokio::test]
    async fn samples_round_trip_as_a_raw_series() {
        let repository = repository().await;
        let server = ServerId::new();

        repository
            .record_samples(&[
                sample(server, MetricKind::CpuUsage, 12.5, at(100)),
                sample(server, MetricKind::CpuUsage, 40.0, at(200)),
            ])
            .await
            .expect("stored");

        let series = repository
            .series(
                server,
                MetricKind::CpuUsage,
                TimeWindow::new(at(0), at(1_000)),
                Resolution::Raw,
            )
            .await
            .expect("read");

        assert_eq!(series.points.len(), 2);
        assert_eq!(series.points[0].avg, 12.5);
        assert_eq!(series.points[1].avg, 40.0);
        assert_eq!(series.peak(), Some(40.0));
    }

    #[tokio::test]
    async fn an_empty_batch_is_a_no_op() {
        let repository = repository().await;
        assert!(repository.record_samples(&[]).await.is_ok());
    }

    #[tokio::test]
    async fn a_series_is_ordered_by_time_regardless_of_insert_order() {
        let repository = repository().await;
        let server = ServerId::new();
        repository
            .record_samples(&[
                sample(server, MetricKind::CpuUsage, 3.0, at(300)),
                sample(server, MetricKind::CpuUsage, 1.0, at(100)),
                sample(server, MetricKind::CpuUsage, 2.0, at(200)),
            ])
            .await
            .expect("stored");

        let series = repository
            .series(
                server,
                MetricKind::CpuUsage,
                TimeWindow::new(at(0), at(1_000)),
                Resolution::Raw,
            )
            .await
            .expect("read");
        let values: Vec<f64> = series.points.iter().map(|p| p.avg).collect();
        assert_eq!(values, vec![1.0, 2.0, 3.0]);
    }

    #[tokio::test]
    async fn series_are_isolated_per_server_and_per_metric() {
        let repository = repository().await;
        let a = ServerId::new();
        let b = ServerId::new();

        repository
            .record_samples(&[
                sample(a, MetricKind::CpuUsage, 10.0, at(100)),
                sample(b, MetricKind::CpuUsage, 90.0, at(100)),
                sample(a, MetricKind::MemoryUsage, 50.0, at(100)),
            ])
            .await
            .expect("stored");

        let series = repository
            .series(
                a,
                MetricKind::CpuUsage,
                TimeWindow::new(at(0), at(1_000)),
                Resolution::Raw,
            )
            .await
            .expect("read");
        assert_eq!(series.points.len(), 1);
        assert_eq!(series.points[0].avg, 10.0);
    }

    #[tokio::test]
    async fn re_recording_the_same_instant_updates_rather_than_failing() {
        // Two collections at the same millisecond happen when a manual refresh races the
        // scheduler. It must not blow up the batch.
        let repository = repository().await;
        let server = ServerId::new();
        repository
            .record_samples(&[sample(server, MetricKind::CpuUsage, 10.0, at(100))])
            .await
            .expect("stored");
        repository
            .record_samples(&[sample(server, MetricKind::CpuUsage, 20.0, at(100))])
            .await
            .expect("stored again");

        let series = repository
            .series(
                server,
                MetricKind::CpuUsage,
                TimeWindow::new(at(0), at(1_000)),
                Resolution::Raw,
            )
            .await
            .expect("read");
        assert_eq!(series.points.len(), 1);
        assert_eq!(series.points[0].avg, 20.0);
    }

    #[tokio::test]
    async fn the_latest_sample_is_the_newest_one() {
        let repository = repository().await;
        let server = ServerId::new();
        repository
            .record_samples(&[
                sample(server, MetricKind::CpuUsage, 10.0, at(100)),
                sample(server, MetricKind::CpuUsage, 20.0, at(500)),
            ])
            .await
            .expect("stored");

        let latest = repository
            .latest(server, MetricKind::CpuUsage)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(latest.value, 20.0);
        assert_eq!(latest.timestamp, at(500));
    }

    #[tokio::test]
    async fn latest_is_absent_when_nothing_was_ever_recorded() {
        let repository = repository().await;
        assert_eq!(
            repository
                .latest(ServerId::new(), MetricKind::CpuUsage)
                .await
                .expect("read"),
            None
        );
    }

    #[tokio::test]
    async fn raw_samples_roll_up_into_five_minute_buckets() {
        let repository = repository().await;
        let server = ServerId::new();

        // Ten minutes of samples every 30 seconds, with one spike.
        let mut samples = Vec::new();
        for i in 0..20 {
            let value = if i == 2 { 100.0 } else { 10.0 };
            samples.push(sample(server, MetricKind::CpuUsage, value, at(i * 30)));
        }
        repository.record_samples(&samples).await.expect("stored");

        let written = repository
            .build_rollups(Resolution::FiveMinutes, TimeWindow::new(at(0), at(600)))
            .await
            .expect("aggregated");
        assert_eq!(written, 2);

        let rollups = repository
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(0), at(600)),
            )
            .await
            .expect("read");
        assert_eq!(rollups.len(), 2);
        assert_eq!(rollups[0].count, 10);
        assert_eq!(rollups[0].max, 100.0, "the spike must survive aggregation");
        assert_eq!(rollups[0].min, 10.0);
        assert_eq!(rollups[1].max, 10.0);
    }

    #[tokio::test]
    async fn bucket_boundaries_align_to_the_epoch() {
        let repository = repository().await;
        let server = ServerId::new();
        // 00:07:30 belongs to the 00:05:00 bucket.
        repository
            .record_samples(&[sample(server, MetricKind::CpuUsage, 1.0, at(450))])
            .await
            .expect("stored");
        repository
            .build_rollups(Resolution::FiveMinutes, TimeWindow::new(at(0), at(600)))
            .await
            .expect("aggregated");

        let rollups = repository
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(0), at(600)),
            )
            .await
            .expect("read");
        assert_eq!(rollups[0].bucket_start, at(300));
    }

    #[tokio::test]
    async fn hourly_rollups_are_built_from_five_minute_ones_with_a_correct_weighted_average() {
        // Building from the finer tier is what keeps aggregation cheap; the weighting is
        // what keeps it correct when sub-buckets hold different numbers of samples.
        let repository = repository().await;
        let server = ServerId::new();

        // First 5-minute bucket: ten samples of 10. Second: two samples of 100.
        let mut samples = Vec::new();
        for i in 0..10 {
            samples.push(sample(server, MetricKind::CpuUsage, 10.0, at(i * 30)));
        }
        for i in 0..2 {
            samples.push(sample(
                server,
                MetricKind::CpuUsage,
                100.0,
                at(300 + i * 30),
            ));
        }
        repository.record_samples(&samples).await.expect("stored");

        repository
            .build_rollups(Resolution::FiveMinutes, TimeWindow::new(at(0), at(3_600)))
            .await
            .expect("aggregated");
        repository
            .build_rollups(Resolution::OneHour, TimeWindow::new(at(0), at(3_600)))
            .await
            .expect("aggregated");

        let hourly = repository
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::OneHour,
                TimeWindow::new(at(0), at(3_600)),
            )
            .await
            .expect("read");

        assert_eq!(hourly.len(), 1);
        assert_eq!(hourly[0].count, 12);
        assert_eq!(hourly[0].max, 100.0);
        assert_eq!(hourly[0].min, 10.0);
        // A naive mean of the two bucket averages would be (10 + 100) / 2 = 55.
        // The correct weighted value is (10*10 + 2*100) / 12 = 25.
        assert!(
            (hourly[0].avg - 25.0).abs() < 1e-9,
            "average was {}",
            hourly[0].avg
        );
    }

    #[tokio::test]
    async fn aggregation_is_idempotent() {
        let repository = repository().await;
        let server = ServerId::new();
        repository
            .record_samples(&[
                sample(server, MetricKind::CpuUsage, 10.0, at(0)),
                sample(server, MetricKind::CpuUsage, 30.0, at(60)),
            ])
            .await
            .expect("stored");

        for _ in 0..3 {
            repository
                .build_rollups(Resolution::FiveMinutes, TimeWindow::new(at(0), at(300)))
                .await
                .expect("aggregated");
        }

        let rollups = repository
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(0), at(300)),
            )
            .await
            .expect("read");
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].count, 2, "re-running must not double-count");
        assert_eq!(rollups[0].avg, 20.0);
    }

    #[tokio::test]
    async fn a_rollup_series_carries_the_min_max_band() {
        let repository = repository().await;
        let server = ServerId::new();
        repository
            .record_samples(&[
                sample(server, MetricKind::CpuUsage, 5.0, at(0)),
                sample(server, MetricKind::CpuUsage, 95.0, at(60)),
            ])
            .await
            .expect("stored");
        repository
            .build_rollups(Resolution::FiveMinutes, TimeWindow::new(at(0), at(300)))
            .await
            .expect("aggregated");

        let series = repository
            .series(
                server,
                MetricKind::CpuUsage,
                TimeWindow::new(at(0), at(300)),
                Resolution::FiveMinutes,
            )
            .await
            .expect("read");
        assert_eq!(series.points.len(), 1);
        assert_eq!(series.points[0].min, 5.0);
        assert_eq!(series.points[0].max, 95.0);
        assert_eq!(series.points[0].avg, 50.0);
    }

    #[tokio::test]
    async fn the_last_rollup_bucket_tracks_aggregation_progress() {
        let repository = repository().await;
        let server = ServerId::new();
        assert_eq!(
            repository
                .last_rollup_bucket(Resolution::FiveMinutes)
                .await
                .expect("read"),
            None
        );

        repository
            .record_samples(&[sample(server, MetricKind::CpuUsage, 1.0, at(900))])
            .await
            .expect("stored");
        repository
            .build_rollups(Resolution::FiveMinutes, TimeWindow::new(at(0), at(1_200)))
            .await
            .expect("aggregated");

        assert_eq!(
            repository
                .last_rollup_bucket(Resolution::FiveMinutes)
                .await
                .expect("read"),
            Some(at(900))
        );
    }

    #[tokio::test]
    async fn pruning_removes_only_the_requested_tier() {
        let repository = repository().await;
        let server = ServerId::new();
        repository
            .record_samples(&[sample(server, MetricKind::CpuUsage, 1.0, at(0))])
            .await
            .expect("stored");
        repository
            .build_rollups(Resolution::FiveMinutes, TimeWindow::new(at(0), at(300)))
            .await
            .expect("aggregated");

        let deleted = repository
            .prune(Resolution::Raw, at(1_000))
            .await
            .expect("pruned");
        assert_eq!(deleted, 1);

        // The rollup survives: it is what the raw data was aggregated *into*.
        let rollups = repository
            .rollups(
                server,
                MetricKind::CpuUsage,
                Resolution::FiveMinutes,
                TimeWindow::new(at(0), at(300)),
            )
            .await
            .expect("read");
        assert_eq!(rollups.len(), 1);
    }

    #[tokio::test]
    async fn pruning_a_rollup_tier_leaves_the_others_alone() {
        let repository = repository().await;
        let server = ServerId::new();
        repository
            .record_samples(&[sample(server, MetricKind::CpuUsage, 1.0, at(0))])
            .await
            .expect("stored");
        repository
            .build_rollups(Resolution::FiveMinutes, TimeWindow::new(at(0), at(3_600)))
            .await
            .expect("aggregated");
        repository
            .build_rollups(Resolution::OneHour, TimeWindow::new(at(0), at(3_600)))
            .await
            .expect("aggregated");

        repository
            .prune(Resolution::FiveMinutes, at(3_600))
            .await
            .expect("pruned");

        assert!(
            repository
                .rollups(
                    server,
                    MetricKind::CpuUsage,
                    Resolution::OneHour,
                    TimeWindow::new(at(0), at(3_600))
                )
                .await
                .expect("read")
                .len()
                == 1
        );
    }

    #[tokio::test]
    async fn building_raw_rollups_is_a_no_op_because_raw_has_no_source() {
        let repository = repository().await;
        assert_eq!(
            repository
                .build_rollups(Resolution::Raw, TimeWindow::new(at(0), at(300)))
                .await
                .expect("no-op"),
            0
        );
    }

    #[tokio::test]
    async fn a_window_with_no_data_yields_an_empty_series_not_an_error() {
        let repository = repository().await;
        let series = repository
            .series(
                ServerId::new(),
                MetricKind::CpuUsage,
                TimeWindow::new(at(0), at(100)),
                Resolution::Raw,
            )
            .await
            .expect("read");
        assert!(series.is_empty());
        assert_eq!(series.mean(), None);
    }

    #[tokio::test]
    async fn a_large_batch_is_written_in_one_transaction() {
        // Sanity check on the write path at something approaching real volume.
        let repository = repository().await;
        let server = ServerId::new();
        let samples: Vec<MetricSample> = (0..5_000)
            .map(|i| {
                sample(
                    server,
                    MetricKind::CpuUsage,
                    f64::from(i % 100),
                    at(i64::from(i)),
                )
            })
            .collect();

        repository.record_samples(&samples).await.expect("stored");

        let series = repository
            .series(
                server,
                MetricKind::CpuUsage,
                TimeWindow::new(at(0), at(10_000)),
                Resolution::Raw,
            )
            .await
            .expect("read");
        assert_eq!(series.points.len(), 5_000);
    }

    #[tokio::test]
    async fn a_daily_rollup_covers_a_whole_day() {
        let repository = repository().await;
        let server = ServerId::new();
        let day = 86_400;

        let samples: Vec<MetricSample> = (0..24)
            .map(|hour| {
                sample(
                    server,
                    MetricKind::CpuUsage,
                    f64::from(hour),
                    at(hour as i64 * 3_600),
                )
            })
            .collect();
        repository.record_samples(&samples).await.expect("stored");

        let window = TimeWindow::new(at(0), at(day));
        repository
            .build_rollups(Resolution::FiveMinutes, window)
            .await
            .expect("aggregated");
        repository
            .build_rollups(Resolution::OneHour, window)
            .await
            .expect("aggregated");
        repository
            .build_rollups(Resolution::OneDay, window)
            .await
            .expect("aggregated");

        let daily = repository
            .rollups(server, MetricKind::CpuUsage, Resolution::OneDay, window)
            .await
            .expect("read");
        assert_eq!(daily.len(), 1);
        assert_eq!(daily[0].count, 24);
        assert_eq!(daily[0].min, 0.0);
        assert_eq!(daily[0].max, 23.0);
        assert_eq!(daily[0].bucket_start, at(0));
        assert_eq!(Resolution::OneDay.bucket_width(), Some(Duration::days(1)));
    }
}
