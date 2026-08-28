//! SQLite implementation of [`AnalyticsRepository`].
//!
//! The schema is provider-neutral: there is no `yandex_*` table anywhere. Provider
//! identity is a string column and provider-specific configuration is a versioned JSON
//! blob, which is what makes adding Google Analytics a matter of rows rather than
//! columns. See `docs/adr/003-analytics-provider-architecture.md`.

use crate::connection::Database;
use crate::convert::*;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::Row;
use std::collections::BTreeMap;
use vds_domain::analytics::{
    AnalyticsIntegration, AnalyticsInterval, AnalyticsMetric, AnalyticsPoint, AnalyticsSnapshot,
    AnalyticsTimeSeries, DateRange, ProviderSettings,
};
use vds_domain::ids::{CredentialRef, IntegrationId, ProviderId, WebsiteId};
use vds_domain::metrics::MetricValue;
use vds_domain::ports::{AnalyticsRepository, RepositoryError};

/// Stores analytics integrations and their cached results.
#[derive(Debug, Clone)]
pub struct SqliteAnalyticsRepository {
    database: Database,
}

impl SqliteAnalyticsRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

const INTEGRATION_COLUMNS: &str = "id, website_id, provider, external_id, credential_ref, \
     enabled, refresh_interval_mins, settings_version, settings_json, created_at";

fn read_integration(row: &Row<'_>) -> Result<AnalyticsIntegration, rusqlite::Error> {
    let id: String = row.get(0)?;
    let website_id: String = row.get(1)?;
    let provider: String = row.get(2)?;
    let external_id: String = row.get(3)?;
    let credential_ref: String = row.get(4)?;
    let enabled: i64 = row.get(5)?;
    let refresh_interval_mins: i64 = row.get(6)?;
    let settings_version: i64 = row.get(7)?;
    let settings_json: String = row.get(8)?;
    let created_at: i64 = row.get(9)?;

    Ok(AnalyticsIntegration {
        id: IntegrationId::from_uuid(
            parse_uuid("analytics_integrations.id", &id).map_err(corrupt)?,
        ),
        website_id: WebsiteId::from_uuid(
            parse_uuid("analytics_integrations.website_id", &website_id).map_err(corrupt)?,
        ),
        provider: ProviderId::new(provider),
        external_id,
        credential_ref: CredentialRef::from_uuid(
            parse_uuid("analytics_integrations.credential_ref", &credential_ref)
                .map_err(corrupt)?,
        ),
        enabled: enabled != 0,
        refresh_interval_mins: refresh_interval_mins.max(1) as u32,
        settings: ProviderSettings {
            version: settings_version.max(0) as u32,
            values: from_json("analytics_integrations.settings_json", &settings_json)
                .map_err(corrupt)?,
        },
        created_at: from_millis(created_at).map_err(corrupt)?,
    })
}

/// The stored form of a snapshot's metrics.
///
/// A map keyed by the metric's stable string name, so adding a metric never disturbs
/// stored rows and an unknown one from a newer build is skipped rather than fatal.
type StoredMetrics = BTreeMap<String, Option<f64>>;

fn encode_metrics(snapshot: &AnalyticsSnapshot) -> StoredMetrics {
    snapshot
        .iter()
        .map(|(metric, value)| (metric.as_str().to_owned(), value.value()))
        .collect()
}

fn decode_metrics(snapshot: &mut AnalyticsSnapshot, stored: StoredMetrics) {
    for (name, value) in stored {
        // A metric this build does not know about is ignored, not an error: it means the
        // database was written by a newer version.
        let Some(metric) = AnalyticsMetric::parse(&name) else {
            continue;
        };
        // `None` on the wire means the provider could not serve it. That must come back
        // as NotAvailable, never as zero.
        snapshot.set(
            metric,
            value.map_or(MetricValue::NotAvailable, MetricValue::available),
        );
    }
}

#[async_trait]
impl AnalyticsRepository for SqliteAnalyticsRepository {
    async fn list_integrations(&self) -> Result<Vec<AnalyticsIntegration>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!(
                    "SELECT {INTEGRATION_COLUMNS} FROM analytics_integrations ORDER BY created_at"
                );
                let mut statement = connection.prepare(&sql)?;
                statement.query_map([], read_integration)?.collect()
            })
            .await
    }

    async fn list_integrations_for_website(
        &self,
        website: WebsiteId,
    ) -> Result<Vec<AnalyticsIntegration>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!(
                    "SELECT {INTEGRATION_COLUMNS} FROM analytics_integrations
                     WHERE website_id = ?1 ORDER BY created_at"
                );
                let mut statement = connection.prepare(&sql)?;
                statement
                    .query_map([Sql(website)], read_integration)?
                    .collect()
            })
            .await
    }

    async fn get_integration(
        &self,
        id: IntegrationId,
    ) -> Result<AnalyticsIntegration, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!(
                    "SELECT {INTEGRATION_COLUMNS} FROM analytics_integrations WHERE id = ?1"
                );
                connection.query_row(&sql, [Sql(id)], read_integration)
            })
            .await
            .map_err(|err| match err {
                RepositoryError::NotFound { .. } => RepositoryError::not_found("integration", id),
                other => other,
            })
    }

    async fn save_integration(
        &self,
        integration: &AnalyticsIntegration,
    ) -> Result<(), RepositoryError> {
        let integration = integration.clone();
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO analytics_integrations (id, website_id, provider, external_id,
                         credential_ref, enabled, refresh_interval_mins, settings_version,
                         settings_json, created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                     ON CONFLICT(id) DO UPDATE SET
                         website_id = excluded.website_id,
                         provider = excluded.provider,
                         external_id = excluded.external_id,
                         credential_ref = excluded.credential_ref,
                         enabled = excluded.enabled,
                         refresh_interval_mins = excluded.refresh_interval_mins,
                         settings_version = excluded.settings_version,
                         settings_json = excluded.settings_json",
                    rusqlite::params![
                        Sql(integration.id),
                        Sql(integration.website_id),
                        integration.provider.as_str(),
                        integration.external_id,
                        Sql(integration.credential_ref),
                        i64::from(integration.enabled),
                        i64::from(integration.refresh_interval_mins),
                        i64::from(integration.settings.version),
                        to_json(&integration.settings.values)?,
                        to_millis(integration.created_at),
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn delete_integration(&self, id: IntegrationId) -> Result<(), RepositoryError> {
        self.database
            .call(move |connection| {
                connection.execute(
                    "DELETE FROM analytics_integrations WHERE id = ?1",
                    [Sql(id)],
                )?;
                Ok(())
            })
            .await
    }

    async fn save_snapshot(&self, snapshot: &AnalyticsSnapshot) -> Result<(), RepositoryError> {
        let metrics = encode_metrics(snapshot);
        let snapshot = snapshot.clone();
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO analytics_snapshots
                        (website_id, provider, range_from, range_to, fetched_at, metrics_json)
                     VALUES (?1,?2,?3,?4,?5,?6)
                     ON CONFLICT(website_id, provider, range_from, range_to) DO UPDATE SET
                         fetched_at = excluded.fetched_at,
                         metrics_json = excluded.metrics_json",
                    rusqlite::params![
                        Sql(snapshot.website_id),
                        snapshot.provider.as_str(),
                        format_date(snapshot.range.from),
                        format_date(snapshot.range.to),
                        to_millis(snapshot.fetched_at),
                        to_json(&metrics)?,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn snapshot(
        &self,
        website: WebsiteId,
        provider: &ProviderId,
        range: DateRange,
    ) -> Result<Option<AnalyticsSnapshot>, RepositoryError> {
        let provider = provider.clone();
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT fetched_at, metrics_json FROM analytics_snapshots
                     WHERE website_id = ?1 AND provider = ?2 AND range_from = ?3 AND range_to = ?4",
                )?;
                let mut rows = statement.query_map(
                    rusqlite::params![
                        Sql(website),
                        provider.as_str(),
                        format_date(range.from),
                        format_date(range.to)
                    ],
                    |row| {
                        let fetched_at: i64 = row.get(0)?;
                        let metrics_json: String = row.get(1)?;
                        let mut snapshot = AnalyticsSnapshot::new(
                            website,
                            provider.clone(),
                            range,
                            from_millis(fetched_at).map_err(corrupt)?,
                        );
                        decode_metrics(
                            &mut snapshot,
                            from_json::<StoredMetrics>(
                                "analytics_snapshots.metrics_json",
                                &metrics_json,
                            )
                            .map_err(corrupt)?,
                        );
                        Ok(snapshot)
                    },
                )?;
                rows.next().transpose()
            })
            .await
    }

    async fn save_time_series(&self, series: &AnalyticsTimeSeries) -> Result<(), RepositoryError> {
        let series = series.clone();
        self.database
            .call(move |connection| {
                let points: Vec<(i64, f64)> = series
                    .points
                    .iter()
                    .map(|p| (to_millis(p.timestamp), p.value))
                    .collect();
                connection.execute(
                    "INSERT INTO analytics_time_series
                        (website_id, provider, metric, interval, range_from, range_to,
                         fetched_at, points_json)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                     ON CONFLICT(website_id, provider, metric, interval, range_from, range_to)
                     DO UPDATE SET
                         fetched_at = excluded.fetched_at,
                         points_json = excluded.points_json",
                    rusqlite::params![
                        Sql(series.website_id),
                        series.provider.as_str(),
                        series.metric.as_str(),
                        series.interval.as_str(),
                        format_date(series.range.from),
                        format_date(series.range.to),
                        to_millis(series.fetched_at),
                        to_json(&points)?,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn time_series(
        &self,
        website: WebsiteId,
        provider: &ProviderId,
        metric: AnalyticsMetric,
        interval: AnalyticsInterval,
        range: DateRange,
    ) -> Result<Option<AnalyticsTimeSeries>, RepositoryError> {
        let provider = provider.clone();
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT fetched_at, points_json FROM analytics_time_series
                     WHERE website_id = ?1 AND provider = ?2 AND metric = ?3
                       AND interval = ?4 AND range_from = ?5 AND range_to = ?6",
                )?;
                let mut rows = statement.query_map(
                    rusqlite::params![
                        Sql(website),
                        provider.as_str(),
                        metric.as_str(),
                        interval.as_str(),
                        format_date(range.from),
                        format_date(range.to)
                    ],
                    |row| {
                        let fetched_at: i64 = row.get(0)?;
                        let points_json: String = row.get(1)?;
                        let raw = from_json::<Vec<(i64, f64)>>(
                            "analytics_time_series.points_json",
                            &points_json,
                        )
                        .map_err(corrupt)?;

                        let mut points = Vec::with_capacity(raw.len());
                        for (millis, value) in raw {
                            points.push(AnalyticsPoint {
                                timestamp: from_millis(millis).map_err(corrupt)?,
                                value,
                            });
                        }

                        Ok(AnalyticsTimeSeries {
                            website_id: website,
                            provider: provider.clone(),
                            metric,
                            interval,
                            range,
                            fetched_at: from_millis(fetched_at).map_err(corrupt)?,
                            points,
                        })
                    },
                )?;
                rows.next().transpose()
            })
            .await
    }

    async fn prune(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError> {
        let cutoff = to_millis(before);
        self.database
            .transaction(move |transaction| {
                let snapshots = transaction.execute(
                    "DELETE FROM analytics_snapshots WHERE fetched_at < ?1",
                    [cutoff],
                )?;
                let series = transaction.execute(
                    "DELETE FROM analytics_time_series WHERE fetched_at < ?1",
                    [cutoff],
                )?;
                Ok((snapshots + series) as u64)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
    }

    fn range() -> DateRange {
        DateRange::new(day(2026, 7, 28), day(2026, 8, 26))
    }

    struct Harness {
        repository: SqliteAnalyticsRepository,
        website: WebsiteId,
    }

    async fn harness() -> Harness {
        let database = Database::open_in_memory().await.expect("opens");
        let website = WebsiteId::new();
        // The integration table has a foreign key onto websites.
        let for_insert = website;
        database
            .call(move |c| {
                c.execute(
                    "INSERT INTO websites VALUES
                        (?1,'Example','https://example.com',NULL,1,60,15,'{}',2,'{}','{}',1,'[]',0)",
                    [Sql(for_insert)],
                )
            })
            .await
            .expect("website inserted");

        Harness {
            repository: SqliteAnalyticsRepository::new(database),
            website,
        }
    }

    fn integration(website: WebsiteId, provider: &str) -> AnalyticsIntegration {
        AnalyticsIntegration::new(
            website,
            ProviderId::new(provider),
            "12345",
            CredentialRef::new(),
            at(1_000),
        )
    }

    #[tokio::test]
    async fn an_integration_round_trips() {
        let h = harness().await;
        let mut integration = integration(h.website, "yandex_metrica");
        integration.settings = ProviderSettings::new(serde_json::json!({ "goal_id": "42" }));
        h.repository
            .save_integration(&integration)
            .await
            .expect("saved");

        let loaded = h
            .repository
            .get_integration(integration.id)
            .await
            .expect("loaded");
        assert_eq!(loaded, integration);
        assert_eq!(loaded.settings.string("goal_id"), Some("42"));
    }

    #[tokio::test]
    async fn the_schema_has_no_provider_specific_tables() {
        // The architectural guarantee from ADR-003, asserted rather than assumed.
        let h = harness().await;
        let tables: Vec<String> = h
            .repository
            .database
            .call(|c| {
                let mut s = c.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
                s.query_map([], |row| row.get::<_, String>(0))?.collect()
            })
            .await
            .expect("readable");

        for table in &tables {
            let lower = table.to_lowercase();
            assert!(
                !lower.contains("yandex"),
                "provider-specific table: {table}"
            );
            assert!(
                !lower.contains("metrica"),
                "provider-specific table: {table}"
            );
            assert!(
                !lower.contains("google"),
                "provider-specific table: {table}"
            );
        }
    }

    #[tokio::test]
    async fn two_providers_can_coexist_for_one_website() {
        // Adding a second provider must be rows, not schema.
        let h = harness().await;
        h.repository
            .save_integration(&integration(h.website, "yandex_metrica"))
            .await
            .expect("saved");
        h.repository
            .save_integration(&integration(h.website, "plausible"))
            .await
            .expect("saved");

        let found = h
            .repository
            .list_integrations_for_website(h.website)
            .await
            .expect("listed");
        assert_eq!(found.len(), 2);
    }

    #[tokio::test]
    async fn a_snapshot_round_trips_with_its_metrics() {
        let h = harness().await;
        let provider = ProviderId::new("yandex_metrica");
        let snapshot = AnalyticsSnapshot::new(h.website, provider.clone(), range(), at(5_000))
            .with(AnalyticsMetric::Visitors, MetricValue::Available(24_821.0))
            .with(AnalyticsMetric::BounceRate, MetricValue::Available(42.5));

        h.repository.save_snapshot(&snapshot).await.expect("saved");

        let loaded = h
            .repository
            .snapshot(h.website, &provider, range())
            .await
            .expect("read")
            .expect("present");
        assert_eq!(
            loaded.get(AnalyticsMetric::Visitors),
            MetricValue::Available(24_821.0)
        );
        assert_eq!(
            loaded.get(AnalyticsMetric::BounceRate),
            MetricValue::Available(42.5)
        );
        assert_eq!(loaded.fetched_at, at(5_000));
    }

    #[tokio::test]
    async fn an_unavailable_metric_comes_back_unavailable_not_as_zero() {
        // The single most important property of the analytics model.
        let h = harness().await;
        let provider = ProviderId::new("yandex_metrica");
        let snapshot = AnalyticsSnapshot::new(h.website, provider.clone(), range(), at(5_000))
            .with(AnalyticsMetric::Visitors, MetricValue::Available(100.0))
            .with(AnalyticsMetric::PagesPerSession, MetricValue::NotAvailable);

        h.repository.save_snapshot(&snapshot).await.expect("saved");

        let loaded = h
            .repository
            .snapshot(h.website, &provider, range())
            .await
            .expect("read")
            .expect("present");
        assert_eq!(
            loaded.get(AnalyticsMetric::PagesPerSession),
            MetricValue::NotAvailable
        );
    }

    #[tokio::test]
    async fn a_metric_written_by_a_newer_build_is_skipped_rather_than_fatal() {
        let h = harness().await;
        let website = h.website;
        h.repository
            .database
            .call(move |c| {
                c.execute(
                    "INSERT INTO analytics_snapshots VALUES
                        (?1,'yandex_metrica','2026-07-28','2026-08-26',0,
                         '{\"visitors\": 10.0, \"quantum_engagement\": 42.0}')",
                    [Sql(website)],
                )
            })
            .await
            .expect("inserted");

        let loaded = h
            .repository
            .snapshot(h.website, &ProviderId::new("yandex_metrica"), range())
            .await
            .expect("read")
            .expect("present");
        assert_eq!(
            loaded.get(AnalyticsMetric::Visitors),
            MetricValue::Available(10.0)
        );
    }

    #[tokio::test]
    async fn snapshots_are_keyed_by_range_so_periods_do_not_overwrite_each_other() {
        let h = harness().await;
        let provider = ProviderId::new("yandex_metrica");
        let today = DateRange::new(day(2026, 8, 26), day(2026, 8, 26));

        h.repository
            .save_snapshot(
                &AnalyticsSnapshot::new(h.website, provider.clone(), range(), at(1))
                    .with(AnalyticsMetric::Visitors, MetricValue::Available(30_000.0)),
            )
            .await
            .expect("saved");
        h.repository
            .save_snapshot(
                &AnalyticsSnapshot::new(h.website, provider.clone(), today, at(1))
                    .with(AnalyticsMetric::Visitors, MetricValue::Available(1_000.0)),
            )
            .await
            .expect("saved");

        let month = h
            .repository
            .snapshot(h.website, &provider, range())
            .await
            .expect("read")
            .expect("present");
        assert_eq!(
            month.get(AnalyticsMetric::Visitors),
            MetricValue::Available(30_000.0)
        );
    }

    #[tokio::test]
    async fn re_saving_a_snapshot_updates_it_in_place() {
        let h = harness().await;
        let provider = ProviderId::new("yandex_metrica");

        for (value, fetched) in [(100.0, at(1)), (200.0, at(2))] {
            h.repository
                .save_snapshot(
                    &AnalyticsSnapshot::new(h.website, provider.clone(), range(), fetched)
                        .with(AnalyticsMetric::Visitors, MetricValue::Available(value)),
                )
                .await
                .expect("saved");
        }

        let loaded = h
            .repository
            .snapshot(h.website, &provider, range())
            .await
            .expect("read")
            .expect("present");
        assert_eq!(
            loaded.get(AnalyticsMetric::Visitors),
            MetricValue::Available(200.0)
        );
        assert_eq!(loaded.fetched_at, at(2));
    }

    #[tokio::test]
    async fn a_time_series_round_trips() {
        let h = harness().await;
        let provider = ProviderId::new("yandex_metrica");
        let series = AnalyticsTimeSeries {
            website_id: h.website,
            provider: provider.clone(),
            metric: AnalyticsMetric::Visitors,
            interval: AnalyticsInterval::Day,
            range: range(),
            fetched_at: at(9_000),
            points: vec![
                AnalyticsPoint {
                    timestamp: at(0),
                    value: 100.0,
                },
                AnalyticsPoint {
                    timestamp: at(86_400),
                    value: 250.5,
                },
            ],
        };

        h.repository.save_time_series(&series).await.expect("saved");

        let loaded = h
            .repository
            .time_series(
                h.website,
                &provider,
                AnalyticsMetric::Visitors,
                AnalyticsInterval::Day,
                range(),
            )
            .await
            .expect("read")
            .expect("present");
        assert_eq!(loaded, series);
        assert_eq!(loaded.total(), 350.5);
    }

    #[tokio::test]
    async fn a_missing_snapshot_is_absent_not_an_error() {
        let h = harness().await;
        assert_eq!(
            h.repository
                .snapshot(h.website, &ProviderId::new("nobody"), range())
                .await
                .expect("read"),
            None
        );
    }

    #[tokio::test]
    async fn deleting_a_website_removes_its_integrations() {
        let h = harness().await;
        h.repository
            .save_integration(&integration(h.website, "yandex_metrica"))
            .await
            .expect("saved");

        let website = h.website;
        h.repository
            .database
            .call(move |c| c.execute("DELETE FROM websites WHERE id = ?1", [Sql(website)]))
            .await
            .expect("deleted");

        assert!(
            h.repository
                .list_integrations()
                .await
                .expect("listed")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn pruning_removes_stale_snapshots_and_series() {
        let h = harness().await;
        let provider = ProviderId::new("yandex_metrica");
        h.repository
            .save_snapshot(&AnalyticsSnapshot::new(
                h.website,
                provider.clone(),
                range(),
                at(1_000),
            ))
            .await
            .expect("saved");
        h.repository
            .save_time_series(&AnalyticsTimeSeries {
                website_id: h.website,
                provider: provider.clone(),
                metric: AnalyticsMetric::Visits,
                interval: AnalyticsInterval::Day,
                range: range(),
                fetched_at: at(1_000),
                points: vec![],
            })
            .await
            .expect("saved");

        assert_eq!(h.repository.prune(at(5_000)).await.expect("pruned"), 2);
        assert_eq!(
            h.repository
                .snapshot(h.website, &provider, range())
                .await
                .expect("read"),
            None
        );
    }

    #[tokio::test]
    async fn the_same_counter_cannot_be_registered_twice_for_a_website() {
        let h = harness().await;
        let first = integration(h.website, "yandex_metrica");
        h.repository.save_integration(&first).await.expect("saved");

        // A different integration id but the same website/provider/counter triple.
        let duplicate = integration(h.website, "yandex_metrica");
        let err = h
            .repository
            .save_integration(&duplicate)
            .await
            .expect_err("must fail");
        assert!(matches!(err, RepositoryError::Conflict(_)), "got {err:?}");
    }
}
