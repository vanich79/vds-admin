//! SQLite implementation of [`ServerRepository`].

use crate::connection::Database;
use crate::convert::*;
use async_trait::async_trait;
use rusqlite::Row;
use vds_domain::Status;
use vds_domain::ids::ServerId;
use vds_domain::metrics::MetricValue;
use vds_domain::ports::{RepositoryError, ServerRepository};
use vds_domain::server::{ConnectionSettings, MonitoringThresholds, Server, ServerRuntimeState};

/// Stores servers and their derived runtime state.
#[derive(Debug, Clone)]
pub struct SqliteServerRepository {
    database: Database,
}

impl SqliteServerRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

const SERVER_COLUMNS: &str = "id, name, host, port, connection_mode, connection_json, enabled, \
     poll_interval_secs, offline_after_failures, timeout_secs, thresholds_json, tags_json, \
     created_at";

/// Reconstructs a server from a row.
///
/// Raw columns are read first with the driver's own error type; only then are they
/// converted, so a malformed value is reported as corruption naming its column rather
/// than as an opaque driver failure.
fn read_server(row: &Row<'_>) -> Result<Server, rusqlite::Error> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let host: String = row.get(2)?;
    let port: i64 = row.get(3)?;
    let connection_json: String = row.get(5)?;
    let enabled: i64 = row.get(6)?;
    let poll_interval_secs: i64 = row.get(7)?;
    let offline_after_failures: i64 = row.get(8)?;
    let timeout_secs: i64 = row.get(9)?;
    let thresholds_json: String = row.get(10)?;
    let tags_json: String = row.get(11)?;
    let created_at: i64 = row.get(12)?;

    Ok(Server {
        id: ServerId::from_uuid(parse_uuid("servers.id", &id).map_err(corrupt)?),
        name,
        host,
        port: u16::try_from(port).map_err(|_| {
            corrupt(RepositoryError::Corrupt(format!(
                "servers.port is out of range: {port}"
            )))
        })?,
        connection: from_json::<ConnectionSettings>("servers.connection_json", &connection_json)
            .map_err(corrupt)?,
        enabled: enabled != 0,
        poll_interval_secs: poll_interval_secs.max(1) as u32,
        offline_after_failures: offline_after_failures.max(1) as u32,
        timeout_secs: timeout_secs.max(1) as u32,
        thresholds: from_json::<MonitoringThresholds>("servers.thresholds_json", &thresholds_json)
            .map_err(corrupt)?,
        tags: from_json::<Vec<String>>("servers.tags_json", &tags_json).map_err(corrupt)?,
        created_at: from_millis(created_at).map_err(corrupt)?,
    })
}

fn read_state(row: &Row<'_>) -> Result<ServerRuntimeState, rusqlite::Error> {
    let server_id: String = row.get(0)?;
    let status: String = row.get(1)?;
    let last_check: Option<i64> = row.get(2)?;
    let last_success: Option<i64> = row.get(3)?;
    let consecutive_failures: i64 = row.get(4)?;
    let last_error: Option<String> = row.get(5)?;
    let uptime_secs: Option<i64> = row.get(6)?;
    let cpu_percent: Option<f64> = row.get(7)?;
    let memory_percent: Option<f64> = row.get(8)?;
    let disk_percent: Option<f64> = row.get(9)?;
    let last_error_kind: Option<String> = row.get(10)?;

    Ok(ServerRuntimeState {
        server_id: ServerId::from_uuid(
            parse_uuid("server_state.server_id", &server_id).map_err(corrupt)?,
        ),
        // A status written by a newer build degrades to Unknown rather than making the
        // row unreadable.
        status: Status::from_str_lenient(&status),
        last_check: optional_millis(last_check).map_err(corrupt)?,
        last_success: optional_millis(last_success).map_err(corrupt)?,
        consecutive_failures: consecutive_failures.max(0) as u32,
        last_error,
        // An unrecognised code — written by a newer build — becomes `None` rather than a
        // wrong kind, and the interface falls back to showing the message alone.
        last_error_kind: last_error_kind
            .as_deref()
            .and_then(vds_domain::ports::TransportErrorKind::parse),
        uptime_secs: uptime_secs.map(|v| v.max(0) as u64),
        cpu_percent: cpu_percent.into(),
        memory_percent: memory_percent.into(),
        disk_percent: disk_percent.into(),
    })
}

/// A metric value as a nullable column: unavailable is NULL, never zero.
fn metric_column(value: MetricValue) -> Option<f64> {
    value.value()
}

#[async_trait]
impl ServerRepository for SqliteServerRepository {
    async fn list(&self) -> Result<Vec<Server>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!("SELECT {SERVER_COLUMNS} FROM servers ORDER BY name");
                let mut statement = connection.prepare(&sql)?;
                let rows = statement.query_map([], read_server)?;
                rows.collect()
            })
            .await
    }

    async fn get(&self, id: ServerId) -> Result<Server, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!("SELECT {SERVER_COLUMNS} FROM servers WHERE id = ?1");
                connection.query_row(&sql, [Sql(id)], read_server)
            })
            .await
            .map_err(|err| match err {
                RepositoryError::NotFound { .. } => RepositoryError::not_found("server", id),
                other => other,
            })
    }

    async fn save(&self, server: &Server) -> Result<(), RepositoryError> {
        let server = server.clone();
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO servers (id, name, host, port, connection_mode, connection_json,
                         enabled, poll_interval_secs, offline_after_failures, timeout_secs,
                         thresholds_json, tags_json, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(id) DO UPDATE SET
                         name = excluded.name,
                         host = excluded.host,
                         port = excluded.port,
                         connection_mode = excluded.connection_mode,
                         connection_json = excluded.connection_json,
                         enabled = excluded.enabled,
                         poll_interval_secs = excluded.poll_interval_secs,
                         offline_after_failures = excluded.offline_after_failures,
                         timeout_secs = excluded.timeout_secs,
                         thresholds_json = excluded.thresholds_json,
                         tags_json = excluded.tags_json",
                    rusqlite::params![
                        Sql(server.id),
                        server.name,
                        server.host,
                        server.port as i64,
                        server.connection.mode().as_str(),
                        to_json(&server.connection)?,
                        i64::from(server.enabled),
                        i64::from(server.poll_interval_secs),
                        i64::from(server.offline_after_failures),
                        i64::from(server.timeout_secs),
                        to_json(&server.thresholds)?,
                        to_json(&server.tags)?,
                        to_millis(server.created_at),
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn delete(&self, id: ServerId) -> Result<(), RepositoryError> {
        self.database
            .call(move |connection| {
                connection.execute("DELETE FROM servers WHERE id = ?1", [Sql(id)])?;
                Ok(())
            })
            .await
    }

    async fn load_state(&self, id: ServerId) -> Result<ServerRuntimeState, RepositoryError> {
        let found = self
            .database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT server_id, status, last_check, last_success, consecutive_failures,
                            last_error, uptime_secs, cpu_percent, memory_percent, disk_percent,
                            last_error_kind
                     FROM server_state WHERE server_id = ?1",
                )?;
                let mut rows = statement.query_map([Sql(id)], read_state)?;
                rows.next().transpose()
            })
            .await?;

        // A server that has never been checked has no stored state, and that is not an
        // error — it is a server we know nothing about yet.
        Ok(found.unwrap_or_else(|| ServerRuntimeState::unknown(id)))
    }

    async fn save_state(&self, state: &ServerRuntimeState) -> Result<(), RepositoryError> {
        let state = state.clone();
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO server_state (server_id, status, last_check, last_success,
                         consecutive_failures, last_error, uptime_secs, cpu_percent,
                         memory_percent, disk_percent, last_error_kind)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT(server_id) DO UPDATE SET
                         status = excluded.status,
                         last_check = excluded.last_check,
                         last_success = excluded.last_success,
                         consecutive_failures = excluded.consecutive_failures,
                         last_error = excluded.last_error,
                         uptime_secs = excluded.uptime_secs,
                         cpu_percent = excluded.cpu_percent,
                         memory_percent = excluded.memory_percent,
                         disk_percent = excluded.disk_percent,
                         last_error_kind = excluded.last_error_kind",
                    rusqlite::params![
                        Sql(state.server_id),
                        state.status.as_str(),
                        state.last_check.map(to_millis),
                        state.last_success.map(to_millis),
                        i64::from(state.consecutive_failures),
                        state.last_error,
                        state.uptime_secs.map(|v| v as i64),
                        metric_column(state.cpu_percent),
                        metric_column(state.memory_percent),
                        metric_column(state.disk_percent),
                        state.last_error_kind.map(|kind| kind.as_str()),
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn list_states(&self) -> Result<Vec<ServerRuntimeState>, RepositoryError> {
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT server_id, status, last_check, last_success, consecutive_failures,
                            last_error, uptime_secs, cpu_percent, memory_percent, disk_percent,
                            last_error_kind
                     FROM server_state",
                )?;
                let rows = statement.query_map([], read_state)?;
                rows.collect()
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use vds_domain::ids::CredentialRef;
    use vds_domain::ports::TransportErrorKind;
    use vds_domain::server::{SshAuthKind, SshSettings};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn sample_server(name: &str) -> Server {
        let mut server = Server::new(
            name,
            "10.0.0.5",
            ConnectionSettings::Ssh(SshSettings {
                username: "root".into(),
                auth_kind: SshAuthKind::EncryptedPrivateKey,
                credential_ref: CredentialRef::new(),
            }),
            at(1_000),
        );
        server.tags = vec!["production".into(), "eu-west".into()];
        server.poll_interval_secs = 15;
        server
    }

    async fn repository() -> SqliteServerRepository {
        let database = Database::open_in_memory().await.expect("opens");
        SqliteServerRepository::new(database)
    }

    #[tokio::test]
    async fn a_server_round_trips_completely() {
        let repository = repository().await;
        let server = sample_server("prod-01");
        repository.save(&server).await.expect("saved");

        let loaded = repository.get(server.id).await.expect("loaded");
        assert_eq!(loaded, server);
    }

    #[tokio::test]
    async fn connection_settings_survive_storage_including_the_credential_handle() {
        let repository = repository().await;
        let server = sample_server("prod-01");
        repository.save(&server).await.expect("saved");

        let loaded = repository.get(server.id).await.expect("loaded");
        assert_eq!(
            loaded.connection.credential_ref(),
            server.connection.credential_ref()
        );
        match loaded.connection {
            ConnectionSettings::Ssh(settings) => {
                assert_eq!(settings.username, "root");
                assert_eq!(settings.auth_kind, SshAuthKind::EncryptedPrivateKey);
            }
            other => panic!("expected SSH settings, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn no_secret_material_is_stored_anywhere_in_the_row() {
        // The database holds an opaque handle; the secret lives in the OS keystore.
        let repository = repository().await;
        let server = sample_server("prod-01");
        repository.save(&server).await.expect("saved");

        let json: String = repository
            .database
            .call(|c| c.query_row("SELECT connection_json FROM servers", [], |r| r.get(0)))
            .await
            .expect("readable");

        assert!(json.contains("credential_ref"));
        assert!(!json.contains("password"), "stored row was: {json}");
        assert!(!json.contains("BEGIN"), "stored row was: {json}");
    }

    #[tokio::test]
    async fn saving_twice_updates_rather_than_duplicating() {
        let repository = repository().await;
        let mut server = sample_server("prod-01");
        repository.save(&server).await.expect("saved");

        server.name = "prod-01-renamed".into();
        server.poll_interval_secs = 60;
        repository.save(&server).await.expect("updated");

        assert_eq!(repository.list().await.expect("listed").len(), 1);
        let loaded = repository.get(server.id).await.expect("loaded");
        assert_eq!(loaded.name, "prod-01-renamed");
        assert_eq!(loaded.poll_interval_secs, 60);
    }

    #[tokio::test]
    async fn an_update_preserves_the_original_creation_time() {
        let repository = repository().await;
        let mut server = sample_server("prod-01");
        repository.save(&server).await.expect("saved");

        server.created_at = at(999_999);
        repository.save(&server).await.expect("updated");

        assert_eq!(
            repository.get(server.id).await.expect("loaded").created_at,
            at(1_000)
        );
    }

    #[tokio::test]
    async fn a_missing_server_reports_not_found_naming_the_entity() {
        let repository = repository().await;
        let err = repository
            .get(ServerId::new())
            .await
            .expect_err("must fail");
        assert!(
            matches!(
                err,
                RepositoryError::NotFound {
                    entity: "server",
                    ..
                }
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn servers_are_listed_alphabetically() {
        let repository = repository().await;
        for name in ["zulu", "alpha", "mike"] {
            repository.save(&sample_server(name)).await.expect("saved");
        }
        let names: Vec<String> = repository
            .list()
            .await
            .expect("listed")
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["alpha", "mike", "zulu"]);
    }

    #[tokio::test]
    async fn a_server_never_checked_has_unknown_state_rather_than_an_error() {
        let repository = repository().await;
        let server = sample_server("prod-01");
        repository.save(&server).await.expect("saved");

        let state = repository.load_state(server.id).await.expect("loaded");
        assert_eq!(state.status, Status::Unknown);
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.cpu_percent, MetricValue::NotAvailable);
    }

    #[tokio::test]
    async fn runtime_state_round_trips() {
        let repository = repository().await;
        let server = sample_server("prod-01");
        repository.save(&server).await.expect("saved");

        let mut state = ServerRuntimeState::unknown(server.id);
        state.status = Status::Warning;
        state.last_check = Some(at(2_000));
        state.last_success = Some(at(1_900));
        state.consecutive_failures = 2;
        state.last_error = Some("timeout".into());
        state.uptime_secs = Some(123_456);
        state.cpu_percent = MetricValue::Available(87.5);
        state.memory_percent = MetricValue::Available(61.25);

        repository.save_state(&state).await.expect("saved");
        assert_eq!(
            repository.load_state(server.id).await.expect("loaded"),
            state
        );
    }

    #[tokio::test]
    async fn an_unavailable_metric_is_stored_as_null_not_as_zero() {
        // Storing 0.0 would make an unmeasured server look idle on every chart.
        let repository = repository().await;
        let server = sample_server("prod-01");
        repository.save(&server).await.expect("saved");

        let state = ServerRuntimeState::unknown(server.id);
        repository.save_state(&state).await.expect("saved");

        let stored: Option<f64> = repository
            .database
            .call(|c| c.query_row("SELECT cpu_percent FROM server_state", [], |r| r.get(0)))
            .await
            .expect("readable");
        assert_eq!(stored, None);
        assert_eq!(
            repository
                .load_state(server.id)
                .await
                .expect("loaded")
                .cpu_percent,
            MetricValue::NotAvailable
        );
    }

    #[tokio::test]
    async fn deleting_a_server_removes_its_state_too() {
        let repository = repository().await;
        let server = sample_server("prod-01");
        repository.save(&server).await.expect("saved");
        repository
            .save_state(&ServerRuntimeState::unknown(server.id))
            .await
            .expect("saved");

        repository.delete(server.id).await.expect("deleted");

        assert!(repository.list().await.expect("listed").is_empty());
        assert!(repository.list_states().await.expect("listed").is_empty());
    }

    #[tokio::test]
    async fn deleting_a_server_that_does_not_exist_is_harmless() {
        let repository = repository().await;
        assert!(repository.delete(ServerId::new()).await.is_ok());
    }

    #[tokio::test]
    async fn a_corrupt_row_reports_corruption_rather_than_panicking() {
        let repository = repository().await;
        repository
            .database
            .call(|c| {
                c.execute(
                    "INSERT INTO servers VALUES
                        ('not-a-uuid','n','h',22,'ssh','{}',1,30,3,20,'{}','[]',0)",
                    [],
                )
            })
            .await
            .expect("inserted");

        let err = repository.list().await.expect_err("must fail");
        assert!(matches!(err, RepositoryError::Corrupt(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn state_persists_across_reopening_the_database() {
        // The five-minute alert hold and the offline failure streak both depend on this.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("vds.db");

        let server = sample_server("prod-01");
        {
            let repository =
                SqliteServerRepository::new(Database::open(&path).await.expect("opens"));
            repository.save(&server).await.expect("saved");
            let mut state = ServerRuntimeState::unknown(server.id);
            state.consecutive_failures = 2;
            state.status = Status::Unknown;
            repository.save_state(&state).await.expect("saved");
        }

        let repository = SqliteServerRepository::new(Database::open(&path).await.expect("reopens"));
        let state = repository.load_state(server.id).await.expect("loaded");
        assert_eq!(
            state.consecutive_failures, 2,
            "a restart must not forgive an outage"
        );
    }

    #[tokio::test]
    async fn the_failure_kind_survives_a_round_trip() {
        // The whole point of storing it: the interface needs the kind to translate, and
        // reads it back from here on every start.
        let repository = repository().await;
        let server = sample_server("web-01");
        repository.save(&server).await.expect("saved");

        let mut state = ServerRuntimeState::unknown(server.id);
        state.last_error = Some("authentication failed: bad key".into());
        state.last_error_kind = Some(TransportErrorKind::Authentication);
        repository.save_state(&state).await.expect("saved");

        let loaded = repository.load_state(server.id).await.expect("loaded");
        assert_eq!(
            loaded.last_error_kind,
            Some(TransportErrorKind::Authentication)
        );
        assert_eq!(
            loaded.last_error.as_deref(),
            Some("authentication failed: bad key")
        );
    }

    #[tokio::test]
    async fn a_state_without_a_kind_reads_back_as_none() {
        // Rows written before this column existed, and every successful check.
        let repository = repository().await;
        let server = sample_server("web-01");
        repository.save(&server).await.expect("saved");

        let state = ServerRuntimeState::unknown(server.id);
        repository.save_state(&state).await.expect("saved");

        let loaded = repository.load_state(server.id).await.expect("loaded");
        assert_eq!(loaded.last_error_kind, None);
    }

    #[tokio::test]
    async fn listing_states_returns_the_kind_too() {
        // A regression guard: the single-row query and the list query read the same
        // columns, and updating only one of them emptied the whole server list.
        let repository = repository().await;
        let server = sample_server("web-01");
        repository.save(&server).await.expect("saved");

        let mut state = ServerRuntimeState::unknown(server.id);
        state.last_error_kind = Some(TransportErrorKind::Timeout);
        repository.save_state(&state).await.expect("saved");

        let all = repository.list_states().await.expect("listed");
        assert_eq!(all.len(), 1, "the list query returned nothing");
        assert_eq!(all[0].last_error_kind, Some(TransportErrorKind::Timeout));
    }
}
