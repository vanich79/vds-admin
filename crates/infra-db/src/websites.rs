//! SQLite implementation of [`WebsiteRepository`].

use crate::connection::Database;
use crate::convert::*;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::Row;
use vds_domain::Status;
use vds_domain::ids::{ServerId, WebsiteId};
use vds_domain::metrics::TimeWindow;
use vds_domain::ports::{RepositoryError, WebsiteRepository};
use vds_domain::status::Threshold;
use vds_domain::website::{
    CheckFailure, HttpExpectation, SslInfo, UptimeSummary, Website, WebsiteCheck,
    WebsiteRuntimeState,
};

/// Stores websites, their checks and their derived runtime state.
#[derive(Debug, Clone)]
pub struct SqliteWebsiteRepository {
    database: Database,
}

impl SqliteWebsiteRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

const WEBSITE_COLUMNS: &str = "id, name, url, server_id, enabled, poll_interval_secs, \
     timeout_secs, expectation_json, offline_after_failures, response_threshold_json, \
     ssl_threshold_json, follow_redirects, tags_json, created_at";

fn read_website(row: &Row<'_>) -> Result<Website, rusqlite::Error> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let url: String = row.get(2)?;
    let server_id: Option<String> = row.get(3)?;
    let enabled: i64 = row.get(4)?;
    let poll_interval_secs: i64 = row.get(5)?;
    let timeout_secs: i64 = row.get(6)?;
    let expectation_json: String = row.get(7)?;
    let offline_after_failures: i64 = row.get(8)?;
    let response_threshold_json: String = row.get(9)?;
    let ssl_threshold_json: String = row.get(10)?;
    let follow_redirects: i64 = row.get(11)?;
    let tags_json: String = row.get(12)?;
    let created_at: i64 = row.get(13)?;

    Ok(Website {
        id: WebsiteId::from_uuid(parse_uuid("websites.id", &id).map_err(corrupt)?),
        name,
        url,
        server_id: server_id
            .map(|raw| {
                parse_uuid("websites.server_id", &raw)
                    .map(ServerId::from_uuid)
                    .map_err(corrupt)
            })
            .transpose()?,
        enabled: enabled != 0,
        poll_interval_secs: poll_interval_secs.max(1) as u32,
        timeout_secs: timeout_secs.max(1) as u32,
        expectation: from_json::<HttpExpectation>("websites.expectation_json", &expectation_json)
            .map_err(corrupt)?,
        offline_after_failures: offline_after_failures.max(1) as u32,
        response_time_threshold: from_json::<Threshold>(
            "websites.response_threshold_json",
            &response_threshold_json,
        )
        .map_err(corrupt)?,
        ssl_expiry_threshold: from_json::<Threshold>(
            "websites.ssl_threshold_json",
            &ssl_threshold_json,
        )
        .map_err(corrupt)?,
        follow_redirects: follow_redirects != 0,
        tags: from_json::<Vec<String>>("websites.tags_json", &tags_json).map_err(corrupt)?,
        created_at: from_millis(created_at).map_err(corrupt)?,
    })
}

fn read_state(row: &Row<'_>) -> Result<WebsiteRuntimeState, rusqlite::Error> {
    let website_id: String = row.get(0)?;
    let status: String = row.get(1)?;
    let last_check: Option<i64> = row.get(2)?;
    let last_success: Option<i64> = row.get(3)?;
    let consecutive_failures: i64 = row.get(4)?;
    let response_ms: Option<i64> = row.get(5)?;
    let http_status: Option<i64> = row.get(6)?;
    let ssl_days_remaining: Option<i64> = row.get(7)?;
    let last_error: Option<String> = row.get(8)?;

    Ok(WebsiteRuntimeState {
        website_id: WebsiteId::from_uuid(
            parse_uuid("website_state.website_id", &website_id).map_err(corrupt)?,
        ),
        status: Status::from_str_lenient(&status),
        last_check: optional_millis(last_check).map_err(corrupt)?,
        last_success: optional_millis(last_success).map_err(corrupt)?,
        consecutive_failures: consecutive_failures.max(0) as u32,
        response_ms: response_ms.map(|v| v.clamp(0, i64::from(u32::MAX)) as u32),
        http_status: http_status.map(|v| v.clamp(0, i64::from(u16::MAX)) as u16),
        ssl_days_remaining,
        last_error,
    })
}

fn read_check(row: &Row<'_>) -> Result<WebsiteCheck, rusqlite::Error> {
    let website_id: String = row.get(0)?;
    let checked_at: i64 = row.get(1)?;
    let status: String = row.get(2)?;
    let dns_ms: Option<i64> = row.get(3)?;
    let connect_ms: Option<i64> = row.get(4)?;
    let response_ms: Option<i64> = row.get(5)?;
    let http_status: Option<i64> = row.get(6)?;
    let final_url: Option<String> = row.get(7)?;
    let addresses_json: String = row.get(8)?;
    let ssl_json: Option<String> = row.get(9)?;
    let failure_json: Option<String> = row.get(10)?;

    Ok(WebsiteCheck {
        website_id: WebsiteId::from_uuid(
            parse_uuid("website_checks.website_id", &website_id).map_err(corrupt)?,
        ),
        checked_at: from_millis(checked_at).map_err(corrupt)?,
        status: Status::from_str_lenient(&status),
        resolved_addresses: from_json::<Vec<String>>(
            "website_checks.addresses_json",
            &addresses_json,
        )
        .map_err(corrupt)?,
        dns_ms: dns_ms.map(|v| v.max(0) as u32),
        connect_ms: connect_ms.map(|v| v.max(0) as u32),
        response_ms: response_ms.map(|v| v.max(0) as u32),
        http_status: http_status.map(|v| v.clamp(0, i64::from(u16::MAX)) as u16),
        final_url,
        ssl: ssl_json
            .map(|raw| from_json::<SslInfo>("website_checks.ssl_json", &raw).map_err(corrupt))
            .transpose()?,
        failure: failure_json
            .map(|raw| {
                from_json::<CheckFailure>("website_checks.failure_json", &raw).map_err(corrupt)
            })
            .transpose()?,
    })
}

#[async_trait]
impl WebsiteRepository for SqliteWebsiteRepository {
    async fn list(&self) -> Result<Vec<Website>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!("SELECT {WEBSITE_COLUMNS} FROM websites ORDER BY name");
                let mut statement = connection.prepare(&sql)?;
                statement.query_map([], read_website)?.collect()
            })
            .await
    }

    async fn list_for_server(&self, server: ServerId) -> Result<Vec<Website>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!(
                    "SELECT {WEBSITE_COLUMNS} FROM websites WHERE server_id = ?1 ORDER BY name"
                );
                let mut statement = connection.prepare(&sql)?;
                statement.query_map([Sql(server)], read_website)?.collect()
            })
            .await
    }

    async fn get(&self, id: WebsiteId) -> Result<Website, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!("SELECT {WEBSITE_COLUMNS} FROM websites WHERE id = ?1");
                connection.query_row(&sql, [Sql(id)], read_website)
            })
            .await
            .map_err(|err| match err {
                RepositoryError::NotFound { .. } => RepositoryError::not_found("website", id),
                other => other,
            })
    }

    async fn save(&self, website: &Website) -> Result<(), RepositoryError> {
        let website = website.clone();
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO websites (id, name, url, server_id, enabled, poll_interval_secs,
                         timeout_secs, expectation_json, offline_after_failures,
                         response_threshold_json, ssl_threshold_json, follow_redirects,
                         tags_json, created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
                     ON CONFLICT(id) DO UPDATE SET
                         name = excluded.name,
                         url = excluded.url,
                         server_id = excluded.server_id,
                         enabled = excluded.enabled,
                         poll_interval_secs = excluded.poll_interval_secs,
                         timeout_secs = excluded.timeout_secs,
                         expectation_json = excluded.expectation_json,
                         offline_after_failures = excluded.offline_after_failures,
                         response_threshold_json = excluded.response_threshold_json,
                         ssl_threshold_json = excluded.ssl_threshold_json,
                         follow_redirects = excluded.follow_redirects,
                         tags_json = excluded.tags_json",
                    rusqlite::params![
                        Sql(website.id),
                        website.name,
                        website.url,
                        website.server_id.map(Sql),
                        i64::from(website.enabled),
                        i64::from(website.poll_interval_secs),
                        i64::from(website.timeout_secs),
                        to_json(&website.expectation)?,
                        i64::from(website.offline_after_failures),
                        to_json(&website.response_time_threshold)?,
                        to_json(&website.ssl_expiry_threshold)?,
                        i64::from(website.follow_redirects),
                        to_json(&website.tags)?,
                        to_millis(website.created_at),
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn delete(&self, id: WebsiteId) -> Result<(), RepositoryError> {
        self.database
            .transaction(move |transaction| {
                // Checks have no foreign key — they outlive the website by design for
                // reporting — so they are cleaned up explicitly.
                transaction.execute(
                    "DELETE FROM website_checks WHERE website_id = ?1",
                    [Sql(id)],
                )?;
                transaction.execute("DELETE FROM websites WHERE id = ?1", [Sql(id)])?;
                Ok(())
            })
            .await
    }

    async fn load_state(&self, id: WebsiteId) -> Result<WebsiteRuntimeState, RepositoryError> {
        let found = self
            .database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT website_id, status, last_check, last_success, consecutive_failures,
                            response_ms, http_status, ssl_days_remaining, last_error
                     FROM website_state WHERE website_id = ?1",
                )?;
                statement
                    .query_map([Sql(id)], read_state)?
                    .next()
                    .transpose()
            })
            .await?;

        Ok(found.unwrap_or_else(|| WebsiteRuntimeState::unknown(id)))
    }

    async fn save_state(&self, state: &WebsiteRuntimeState) -> Result<(), RepositoryError> {
        let state = state.clone();
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO website_state (website_id, status, last_check, last_success,
                         consecutive_failures, response_ms, http_status, ssl_days_remaining,
                         last_error)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     ON CONFLICT(website_id) DO UPDATE SET
                         status = excluded.status,
                         last_check = excluded.last_check,
                         last_success = excluded.last_success,
                         consecutive_failures = excluded.consecutive_failures,
                         response_ms = excluded.response_ms,
                         http_status = excluded.http_status,
                         ssl_days_remaining = excluded.ssl_days_remaining,
                         last_error = excluded.last_error",
                    rusqlite::params![
                        Sql(state.website_id),
                        state.status.as_str(),
                        state.last_check.map(to_millis),
                        state.last_success.map(to_millis),
                        i64::from(state.consecutive_failures),
                        state.response_ms.map(i64::from),
                        state.http_status.map(i64::from),
                        state.ssl_days_remaining,
                        state.last_error,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn list_states(&self) -> Result<Vec<WebsiteRuntimeState>, RepositoryError> {
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT website_id, status, last_check, last_success, consecutive_failures,
                            response_ms, http_status, ssl_days_remaining, last_error
                     FROM website_state",
                )?;
                statement.query_map([], read_state)?.collect()
            })
            .await
    }

    async fn record_check(&self, check: &WebsiteCheck) -> Result<(), RepositoryError> {
        let check = check.clone();
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO website_checks (website_id, checked_at, status, success,
                         dns_ms, connect_ms, response_ms, http_status, final_url,
                         addresses_json, ssl_json, failure_json)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    rusqlite::params![
                        Sql(check.website_id),
                        to_millis(check.checked_at),
                        check.status.as_str(),
                        i64::from(check.is_success()),
                        check.dns_ms.map(i64::from),
                        check.connect_ms.map(i64::from),
                        check.response_ms.map(i64::from),
                        check.http_status.map(i64::from),
                        check.final_url,
                        to_json(&check.resolved_addresses)?,
                        check.ssl.as_ref().map(to_json).transpose()?,
                        check.failure.as_ref().map(to_json).transpose()?,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn recent_checks(
        &self,
        id: WebsiteId,
        limit: u32,
    ) -> Result<Vec<WebsiteCheck>, RepositoryError> {
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT website_id, checked_at, status, dns_ms, connect_ms, response_ms,
                            http_status, final_url, addresses_json, ssl_json, failure_json
                     FROM website_checks WHERE website_id = ?1
                     ORDER BY checked_at DESC LIMIT ?2",
                )?;
                statement
                    .query_map(rusqlite::params![Sql(id), i64::from(limit)], read_check)?
                    .collect()
            })
            .await
    }

    async fn uptime(
        &self,
        id: WebsiteId,
        window: TimeWindow,
    ) -> Result<UptimeSummary, RepositoryError> {
        self.database
            .call(move |connection| {
                connection.query_row(
                    "SELECT COUNT(*), COALESCE(SUM(success), 0)
                     FROM website_checks
                     WHERE website_id = ?1 AND checked_at >= ?2 AND checked_at < ?3",
                    rusqlite::params![Sql(id), to_millis(window.from), to_millis(window.to)],
                    |row| {
                        Ok(UptimeSummary {
                            total_checks: row.get::<_, i64>(0)?.max(0) as u32,
                            successful_checks: row.get::<_, i64>(1)?.max(0) as u32,
                        })
                    },
                )
            })
            .await
    }

    async fn prune_checks(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError> {
        self.database
            .call(move |connection| {
                let deleted = connection.execute(
                    "DELETE FROM website_checks WHERE checked_at < ?1",
                    [to_millis(before)],
                )?;
                Ok(deleted as u64)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::website::CheckStage;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    async fn repository() -> SqliteWebsiteRepository {
        let database = Database::open_in_memory().await.expect("opens");
        SqliteWebsiteRepository::new(database)
    }

    fn sample_website(name: &str) -> Website {
        let mut website = Website::new(name, "https://example.com/health", at(1_000));
        website.expectation.body_contains = Some("ok".into());
        website.tags = vec!["public".into()];
        website
    }

    fn ok_check(id: WebsiteId, at_time: DateTime<Utc>) -> WebsiteCheck {
        WebsiteCheck {
            website_id: id,
            checked_at: at_time,
            status: Status::Healthy,
            resolved_addresses: vec!["93.184.216.34".into()],
            dns_ms: Some(5),
            connect_ms: Some(20),
            response_ms: Some(142),
            http_status: Some(200),
            final_url: Some("https://example.com/health".into()),
            ssl: Some(SslInfo {
                subject: "CN=example.com".into(),
                issuer: "CN=Test CA".into(),
                not_before: at(0),
                not_after: at(86_400 * 42),
                fingerprint: "aabbcc".into(),
                san: vec!["example.com".into(), "www.example.com".into()],
            }),
            failure: None,
        }
    }

    #[tokio::test]
    async fn a_website_round_trips_completely() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");
        assert_eq!(repository.get(website.id).await.expect("loaded"), website);
    }

    #[tokio::test]
    async fn a_website_can_be_linked_to_a_server_or_stand_alone() {
        let repository = repository().await;
        let mut website = sample_website("Example");
        website.server_id = None;
        repository.save(&website).await.expect("saved");
        assert_eq!(
            repository.get(website.id).await.expect("loaded").server_id,
            None
        );
    }

    #[tokio::test]
    async fn websites_can_be_listed_by_their_host_server() {
        let repository = repository().await;
        let database = repository.database.clone();
        let server = ServerId::new();
        database
            .call(move |c| {
                c.execute(
                    "INSERT INTO servers VALUES (?1,'n','h',22,'ssh','{}',1,30,3,20,'{}','[]',0)",
                    [Sql(server)],
                )
            })
            .await
            .expect("server inserted");

        let mut hosted = sample_website("Hosted");
        hosted.server_id = Some(server);
        repository.save(&hosted).await.expect("saved");
        repository
            .save(&sample_website("Independent"))
            .await
            .expect("saved");

        let found = repository.list_for_server(server).await.expect("listed");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Hosted");
    }

    #[tokio::test]
    async fn ssl_details_survive_storage() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");
        repository
            .record_check(&ok_check(website.id, at(2_000)))
            .await
            .expect("recorded");

        let checks = repository
            .recent_checks(website.id, 10)
            .await
            .expect("read");
        let ssl = checks[0].ssl.as_ref().expect("ssl present");
        assert_eq!(ssl.issuer, "CN=Test CA");
        assert_eq!(ssl.san.len(), 2);
        assert_eq!(ssl.not_after, at(86_400 * 42));
    }

    #[tokio::test]
    async fn checks_are_returned_newest_first() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");

        for seconds in [1_000, 2_000, 3_000] {
            repository
                .record_check(&ok_check(website.id, at(seconds)))
                .await
                .expect("recorded");
        }

        let checks = repository.recent_checks(website.id, 2).await.expect("read");
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].checked_at, at(3_000));
        assert_eq!(checks[1].checked_at, at(2_000));
    }

    #[tokio::test]
    async fn failed_checks_are_stored_and_counted_against_uptime() {
        // Storing only successes would make every site show 100% uptime forever.
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");

        for seconds in [1_000, 2_000, 3_000] {
            repository
                .record_check(&ok_check(website.id, at(seconds)))
                .await
                .expect("recorded");
        }
        repository
            .record_check(&WebsiteCheck::failed(
                website.id,
                at(4_000),
                CheckStage::TcpConnection,
                "connection refused",
            ))
            .await
            .expect("recorded");

        let uptime = repository
            .uptime(website.id, TimeWindow::new(at(0), at(10_000)))
            .await
            .expect("computed");
        assert_eq!(uptime.total_checks, 4);
        assert_eq!(uptime.successful_checks, 3);
        assert_eq!(uptime.percent(), Some(75.0));
    }

    #[tokio::test]
    async fn a_failure_reason_survives_storage() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");
        repository
            .record_check(&WebsiteCheck::failed(
                website.id,
                at(1_000),
                CheckStage::DnsResolution,
                "NXDOMAIN",
            ))
            .await
            .expect("recorded");

        let checks = repository.recent_checks(website.id, 1).await.expect("read");
        let failure = checks[0].failure.as_ref().expect("failure present");
        assert_eq!(failure.stage, CheckStage::DnsResolution);
        assert_eq!(failure.message, "NXDOMAIN");
        assert!(!checks[0].is_success());
    }

    #[tokio::test]
    async fn uptime_over_a_window_with_no_checks_is_unknown_not_perfect() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");

        let uptime = repository
            .uptime(website.id, TimeWindow::new(at(0), at(100)))
            .await
            .expect("computed");
        assert_eq!(uptime.total_checks, 0);
        assert_eq!(uptime.percent(), None);
    }

    #[tokio::test]
    async fn the_uptime_window_is_respected() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");

        repository
            .record_check(&ok_check(website.id, at(500)))
            .await
            .expect("recorded");
        repository
            .record_check(&ok_check(website.id, at(1_500)))
            .await
            .expect("recorded");

        let uptime = repository
            .uptime(website.id, TimeWindow::new(at(1_000), at(2_000)))
            .await
            .expect("computed");
        assert_eq!(uptime.total_checks, 1);
    }

    #[tokio::test]
    async fn pruning_removes_old_checks_only() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");

        repository
            .record_check(&ok_check(website.id, at(1_000)))
            .await
            .expect("recorded");
        repository
            .record_check(&ok_check(website.id, at(9_000)))
            .await
            .expect("recorded");

        let deleted = repository.prune_checks(at(5_000)).await.expect("pruned");
        assert_eq!(deleted, 1);

        let remaining = repository
            .recent_checks(website.id, 10)
            .await
            .expect("read");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].checked_at, at(9_000));
    }

    #[tokio::test]
    async fn deleting_a_website_removes_its_state_and_checks() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");
        repository
            .save_state(&WebsiteRuntimeState::unknown(website.id))
            .await
            .expect("saved");
        repository
            .record_check(&ok_check(website.id, at(1_000)))
            .await
            .expect("recorded");

        repository.delete(website.id).await.expect("deleted");

        assert!(repository.list().await.expect("listed").is_empty());
        assert!(repository.list_states().await.expect("listed").is_empty());
        assert!(
            repository
                .recent_checks(website.id, 10)
                .await
                .expect("read")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn runtime_state_round_trips() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");

        let mut state = WebsiteRuntimeState::unknown(website.id);
        state.status = Status::Warning;
        state.last_check = Some(at(2_000));
        state.response_ms = Some(1_234);
        state.http_status = Some(503);
        state.ssl_days_remaining = Some(-3);
        state.consecutive_failures = 1;
        state.last_error = Some("slow".into());

        repository.save_state(&state).await.expect("saved");
        assert_eq!(
            repository.load_state(website.id).await.expect("loaded"),
            state
        );
    }

    #[tokio::test]
    async fn a_negative_ssl_day_count_survives_storage() {
        // An already-expired certificate is exactly the case that matters most.
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");

        let mut state = WebsiteRuntimeState::unknown(website.id);
        state.ssl_days_remaining = Some(-14);
        repository.save_state(&state).await.expect("saved");

        assert_eq!(
            repository
                .load_state(website.id)
                .await
                .expect("loaded")
                .ssl_days_remaining,
            Some(-14)
        );
    }

    #[tokio::test]
    async fn a_website_never_checked_has_unknown_state() {
        let repository = repository().await;
        let website = sample_website("Example");
        repository.save(&website).await.expect("saved");
        assert_eq!(
            repository
                .load_state(website.id)
                .await
                .expect("loaded")
                .status,
            Status::Unknown
        );
    }

    #[tokio::test]
    async fn a_missing_website_reports_not_found() {
        let repository = repository().await;
        let err = repository
            .get(WebsiteId::new())
            .await
            .expect_err("must fail");
        assert!(matches!(
            err,
            RepositoryError::NotFound {
                entity: "website",
                ..
            }
        ));
    }
}
