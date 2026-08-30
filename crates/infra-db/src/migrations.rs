//! Schema migrations.
//!
//! Numbered, embedded in the binary, applied in a transaction, and tracked with
//! `PRAGMA user_version`. Nothing alters the schema implicitly at startup — see
//! `docs/adr/005-metrics-storage.md`.
//!
//! Rules for adding a migration:
//!
//! * append to [`MIGRATIONS`]; never edit an existing entry, because it has already run
//!   on other people's databases;
//! * bump [`SCHEMA_VERSION`];
//! * mark it `destructive` if it drops or rewrites data, which triggers a backup first.

use crate::connection::Database;
use vds_domain::ports::RepositoryError;

/// The schema version this build expects.
pub const SCHEMA_VERSION: u32 = 2;

/// One migration step.
pub struct Migration {
    /// The version this migration produces.
    pub version: u32,
    pub description: &'static str,
    /// Whether it can lose data if interrupted, which triggers a backup first.
    pub destructive: bool,
    pub sql: &'static str,
}

/// Every migration, in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        description: "initial schema",
        destructive: false,
        sql: V1,
    },
    Migration {
        version: 2,
        description: "record why a collection failed, not just what it said",
        destructive: false,
        sql: V2,
    },
];

/// Adds the failure kind beside the failure message.
///
/// `last_error` holds a sentence produced by `TransportError`'s `Display`, which is
/// English and cannot be translated after the fact. The kind is a stable code the
/// interface turns into the user's language, while the original text stays as the
/// technical detail.
///
/// Nullable and added without a default: existing rows keep their message and gain no
/// kind, which the interface already handles — it falls back to showing the detail alone.
/// That is why this migration is not destructive and needs no backup.
const V2: &str = "ALTER TABLE server_state ADD COLUMN last_error_kind TEXT;";

/// The initial schema.
///
/// Complex value objects — connection settings, thresholds, SSL details, analytics
/// metrics — are stored as JSON rather than being exploded into columns. That is a
/// deliberate trade: it keeps the schema stable as the domain grows, and none of those
/// fields is ever queried on. Everything that *is* queried or filtered on gets a real
/// column and an index.
const V1: &str = r#"
CREATE TABLE servers (
    id                      TEXT PRIMARY KEY,
    name                    TEXT NOT NULL,
    host                    TEXT NOT NULL,
    port                    INTEGER NOT NULL,
    connection_mode         TEXT NOT NULL,
    connection_json         TEXT NOT NULL,
    enabled                 INTEGER NOT NULL,
    poll_interval_secs      INTEGER NOT NULL,
    offline_after_failures  INTEGER NOT NULL,
    timeout_secs            INTEGER NOT NULL,
    thresholds_json         TEXT NOT NULL,
    tags_json               TEXT NOT NULL,
    created_at              INTEGER NOT NULL
);

CREATE TABLE server_state (
    server_id            TEXT PRIMARY KEY REFERENCES servers(id) ON DELETE CASCADE,
    status               TEXT NOT NULL,
    last_check           INTEGER,
    last_success         INTEGER,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    last_error           TEXT,
    uptime_secs          INTEGER,
    cpu_percent          REAL,
    memory_percent       REAL,
    disk_percent         REAL
);

CREATE TABLE websites (
    id                       TEXT PRIMARY KEY,
    name                     TEXT NOT NULL,
    url                      TEXT NOT NULL,
    server_id                TEXT REFERENCES servers(id) ON DELETE SET NULL,
    enabled                  INTEGER NOT NULL,
    poll_interval_secs       INTEGER NOT NULL,
    timeout_secs             INTEGER NOT NULL,
    expectation_json         TEXT NOT NULL,
    offline_after_failures   INTEGER NOT NULL,
    response_threshold_json  TEXT NOT NULL,
    ssl_threshold_json       TEXT NOT NULL,
    follow_redirects         INTEGER NOT NULL,
    tags_json                TEXT NOT NULL,
    created_at               INTEGER NOT NULL
);
CREATE INDEX idx_websites_server ON websites(server_id);

CREATE TABLE website_state (
    website_id           TEXT PRIMARY KEY REFERENCES websites(id) ON DELETE CASCADE,
    status               TEXT NOT NULL,
    last_check           INTEGER,
    last_success         INTEGER,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    response_ms          INTEGER,
    http_status          INTEGER,
    ssl_days_remaining   INTEGER,
    last_error           TEXT
);

CREATE TABLE website_checks (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    website_id    TEXT NOT NULL,
    checked_at    INTEGER NOT NULL,
    status        TEXT NOT NULL,
    success       INTEGER NOT NULL,
    dns_ms        INTEGER,
    connect_ms    INTEGER,
    response_ms   INTEGER,
    http_status   INTEGER,
    final_url     TEXT,
    addresses_json TEXT NOT NULL,
    ssl_json      TEXT,
    failure_json  TEXT
);
CREATE INDEX idx_checks_website_time ON website_checks(website_id, checked_at DESC);
CREATE INDEX idx_checks_time ON website_checks(checked_at);

-- Raw time-series. WITHOUT ROWID because the primary key *is* the row: it halves the
-- storage and removes an index lookup on the read path, which matters at millions of
-- rows.
CREATE TABLE metric_samples (
    server_id TEXT NOT NULL,
    kind      TEXT NOT NULL,
    ts        INTEGER NOT NULL,
    value     REAL NOT NULL,
    PRIMARY KEY (server_id, kind, ts)
) WITHOUT ROWID;
CREATE INDEX idx_samples_time ON metric_samples(ts);

CREATE TABLE metric_rollups (
    server_id    TEXT NOT NULL,
    kind         TEXT NOT NULL,
    bucket       TEXT NOT NULL,
    bucket_start INTEGER NOT NULL,
    min_value    REAL NOT NULL,
    max_value    REAL NOT NULL,
    avg_value    REAL NOT NULL,
    sum_value    REAL NOT NULL,
    sample_count INTEGER NOT NULL,
    PRIMARY KEY (server_id, kind, bucket, bucket_start)
) WITHOUT ROWID;
CREATE INDEX idx_rollups_bucket ON metric_rollups(bucket, bucket_start);

CREATE TABLE analytics_integrations (
    id                    TEXT PRIMARY KEY,
    website_id            TEXT NOT NULL REFERENCES websites(id) ON DELETE CASCADE,
    provider              TEXT NOT NULL,
    external_id           TEXT NOT NULL,
    credential_ref        TEXT NOT NULL,
    enabled               INTEGER NOT NULL,
    refresh_interval_mins INTEGER NOT NULL,
    settings_version      INTEGER NOT NULL,
    settings_json         TEXT NOT NULL,
    created_at            INTEGER NOT NULL,
    UNIQUE (website_id, provider, external_id)
);
CREATE INDEX idx_integrations_website ON analytics_integrations(website_id);

CREATE TABLE analytics_snapshots (
    website_id   TEXT NOT NULL,
    provider     TEXT NOT NULL,
    range_from   TEXT NOT NULL,
    range_to     TEXT NOT NULL,
    fetched_at   INTEGER NOT NULL,
    metrics_json TEXT NOT NULL,
    PRIMARY KEY (website_id, provider, range_from, range_to)
);
CREATE INDEX idx_snapshots_fetched ON analytics_snapshots(fetched_at);

CREATE TABLE analytics_time_series (
    website_id  TEXT NOT NULL,
    provider    TEXT NOT NULL,
    metric      TEXT NOT NULL,
    interval    TEXT NOT NULL,
    range_from  TEXT NOT NULL,
    range_to    TEXT NOT NULL,
    fetched_at  INTEGER NOT NULL,
    points_json TEXT NOT NULL,
    PRIMARY KEY (website_id, provider, metric, interval, range_from, range_to)
);
CREATE INDEX idx_series_fetched ON analytics_time_series(fetched_at);

CREATE TABLE screenshots (
    website_id     TEXT PRIMARY KEY REFERENCES websites(id) ON DELETE CASCADE,
    provider       TEXT NOT NULL,
    path           TEXT NOT NULL,
    thumbnail_path TEXT,
    captured_at    INTEGER NOT NULL,
    status_json    TEXT NOT NULL,
    hash           TEXT NOT NULL,
    width          INTEGER NOT NULL,
    height         INTEGER NOT NULL
);

CREATE TABLE alert_rules (
    id                  TEXT PRIMARY KEY,
    name                TEXT NOT NULL,
    enabled             INTEGER NOT NULL,
    condition_json      TEXT NOT NULL,
    scope_json          TEXT NOT NULL,
    for_duration_secs   INTEGER NOT NULL,
    severity            TEXT NOT NULL,
    renotify_after_secs INTEGER NOT NULL,
    notify_via_json     TEXT NOT NULL,
    created_at          INTEGER NOT NULL
);

CREATE TABLE alert_rule_state (
    rule_id          TEXT NOT NULL REFERENCES alert_rules(id) ON DELETE CASCADE,
    subject_kind     TEXT NOT NULL,
    subject_id       TEXT NOT NULL,
    state            TEXT NOT NULL,
    since            INTEGER,
    incident_id      TEXT,
    last_notified_at INTEGER,
    PRIMARY KEY (rule_id, subject_kind, subject_id)
);

CREATE TABLE incidents (
    id           TEXT PRIMARY KEY,
    rule_id      TEXT NOT NULL,
    subject_kind TEXT NOT NULL,
    subject_id   TEXT NOT NULL,
    severity     TEXT NOT NULL,
    summary      TEXT NOT NULL,
    opened_at    INTEGER NOT NULL,
    resolved_at  INTEGER,
    acknowledged INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_incidents_open ON incidents(resolved_at, opened_at DESC);

CREATE TABLE events (
    id           TEXT PRIMARY KEY,
    occurred_at  INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    severity     TEXT NOT NULL,
    subject_kind TEXT,
    subject_id   TEXT,
    payload_json TEXT NOT NULL
);
CREATE INDEX idx_events_time ON events(occurred_at DESC);
CREATE INDEX idx_events_subject ON events(subject_kind, subject_id, occurred_at DESC);
"#;

/// Reads the schema version recorded in the database.
pub async fn current_version(database: &Database) -> Result<u32, RepositoryError> {
    database
        .call(|connection| {
            let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            Ok(version.max(0) as u32)
        })
        .await
}

/// Brings the database up to [`SCHEMA_VERSION`].
///
/// Each step runs in its own transaction, so an interrupted upgrade leaves the database
/// at a valid intermediate version rather than half-migrated.
pub async fn apply(database: &Database) -> Result<(), RepositoryError> {
    let from = current_version(database).await?;

    if from > SCHEMA_VERSION {
        return Err(RepositoryError::Migration(format!(
            "database is at schema version {from}, but this build only understands {SCHEMA_VERSION}; \
             upgrade the application"
        )));
    }

    if from == SCHEMA_VERSION {
        return Ok(());
    }

    let pending: Vec<&Migration> = MIGRATIONS
        .iter()
        .filter(|migration| migration.version > from)
        .collect();

    // A destructive step gets a backup first, so a failed upgrade is recoverable.
    if pending.iter().any(|migration| migration.destructive)
        && let Some(path) = database.path()
    {
        let backup = path.with_extension(format!("pre-v{SCHEMA_VERSION}.bak"));
        tracing::info!(backup = ?backup, "backing up before a destructive migration");
        database.backup_to(backup).await?;
    }

    for migration in pending {
        tracing::info!(
            version = migration.version,
            description = migration.description,
            "applying migration"
        );

        let sql = migration.sql;
        let version = migration.version;
        database
            .transaction(move |transaction| {
                transaction.execute_batch(sql)?;
                // `user_version` takes no parameters, hence the format. `version` is a
                // compile-time constant from MIGRATIONS, not user input.
                transaction.pragma_update(None, "user_version", version as i64)?;
                Ok(())
            })
            .await
            .map_err(|err| {
                RepositoryError::Migration(format!(
                    "migration {} ({}) failed: {err}",
                    migration.version, migration.description
                ))
            })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_numbered_consecutively_from_one() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                migration.version,
                index as u32 + 1,
                "migration {} is out of order",
                migration.description
            );
        }
    }

    #[test]
    fn the_schema_version_matches_the_last_migration() {
        // Bumping one without the other would either skip a migration or loop forever.
        let last = MIGRATIONS.last().map(|m| m.version).unwrap_or(0);
        assert_eq!(last, SCHEMA_VERSION);
    }

    #[test]
    fn every_migration_has_a_description() {
        assert!(MIGRATIONS.iter().all(|m| !m.description.trim().is_empty()));
        assert!(MIGRATIONS.iter().all(|m| !m.sql.trim().is_empty()));
    }

    #[tokio::test]
    async fn a_fresh_database_reaches_the_current_version() {
        let database = Database::open_in_memory().await.expect("opens");
        assert_eq!(
            current_version(&database).await.expect("readable"),
            SCHEMA_VERSION
        );
    }

    #[tokio::test]
    async fn migrating_twice_is_a_no_op() {
        let database = Database::open_in_memory().await.expect("opens");
        apply(&database).await.expect("second run succeeds");
        assert_eq!(
            current_version(&database).await.expect("readable"),
            SCHEMA_VERSION
        );
    }

    #[tokio::test]
    async fn every_expected_table_exists_after_migration() {
        let database = Database::open_in_memory().await.expect("opens");
        let tables: Vec<String> = database
            .call(|connection| {
                let mut statement = connection
                    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                rows.collect::<Result<Vec<String>, _>>()
            })
            .await
            .expect("readable");

        for expected in [
            "alert_rule_state",
            "alert_rules",
            "analytics_integrations",
            "analytics_snapshots",
            "analytics_time_series",
            "events",
            "incidents",
            "metric_rollups",
            "metric_samples",
            "screenshots",
            "server_state",
            "servers",
            "website_checks",
            "website_state",
            "websites",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "table {expected} is missing"
            );
        }
    }

    #[tokio::test]
    async fn a_database_from_a_newer_build_is_refused_rather_than_corrupted() {
        // Opening a v9 database with a v1 binary and writing to it would destroy data.
        let database = Database::open_in_memory().await.expect("opens");
        database
            .call(|connection| connection.pragma_update(None, "user_version", 99_i64))
            .await
            .expect("bumped");

        let err = apply(&database).await.expect_err("must refuse");
        assert!(matches!(err, RepositoryError::Migration(_)), "got {err:?}");
        assert!(
            err.to_string().contains("upgrade the application"),
            "message was: {err}"
        );
    }

    #[tokio::test]
    async fn deleting_a_server_cascades_to_its_state() {
        let database = Database::open_in_memory().await.expect("opens");
        database
            .call(|connection| {
                connection.execute_batch(
                    "INSERT INTO servers VALUES ('s1','n','h',22,'ssh','{}',1,30,3,20,'{}','[]',0);
                     INSERT INTO server_state (server_id, status, consecutive_failures)
                        VALUES ('s1','healthy',0);
                     DELETE FROM servers WHERE id = 's1';",
                )
            })
            .await
            .expect("executed");

        let remaining: i64 = database
            .call(|c| c.query_row("SELECT COUNT(*) FROM server_state", [], |row| row.get(0)))
            .await
            .expect("readable");
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn deleting_a_server_leaves_its_websites_but_unlinks_them() {
        // Deleting a machine must not delete the record of the sites it hosted.
        let database = Database::open_in_memory().await.expect("opens");
        database
            .call(|connection| {
                connection.execute_batch(
                    "INSERT INTO servers VALUES ('s1','n','h',22,'ssh','{}',1,30,3,20,'{}','[]',0);
                     INSERT INTO websites VALUES
                        ('w1','site','https://x','s1',1,60,15,'{}',2,'{}','{}',1,'[]',0);
                     DELETE FROM servers WHERE id = 's1';",
                )
            })
            .await
            .expect("executed");

        let server_id: Option<String> = database
            .call(|c| {
                c.query_row("SELECT server_id FROM websites WHERE id='w1'", [], |r| {
                    r.get(0)
                })
            })
            .await
            .expect("readable");
        assert_eq!(server_id, None);
    }

    #[tokio::test]
    async fn an_existing_database_is_upgraded_without_losing_its_rows() {
        // The case that actually matters: this runs on a database that has been
        // collecting for days. A migration that dropped and recreated the table would
        // pass every other test here and lose the user's history.
        let database = Database::unmigrated_in_memory().expect("opens");

        // Bring it to v1 only, as an older build would have left it.
        database
            .call(|c| {
                c.execute_batch(V1)?;
                c.pragma_update(None, "user_version", 1)?;
                Ok(())
            })
            .await
            .expect("v1 applied");

        database
            .call(|c| {
                c.execute(
                    "INSERT INTO servers (id, name, host, port, connection_mode,
                         connection_json, enabled, poll_interval_secs,
                         offline_after_failures, timeout_secs, thresholds_json, tags_json,
                         created_at)
                     VALUES ('s1', 'web-01', '10.0.0.1', 22, 'ssh', '{}', 1, 30, 3, 20,
                             '{}', '[]', 0)",
                    [],
                )?;
                c.execute(
                    "INSERT INTO server_state (server_id, status, consecutive_failures, last_error)
                     VALUES ('s1', 'offline', 3, 'authentication failed: bad key')",
                    [],
                )?;
                Ok(())
            })
            .await
            .expect("rows inserted");

        apply(&database).await.expect("migrates");

        let (version, name, error, kind) = database
            .call(|c| {
                let version: u32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
                let name: String = c.query_row("SELECT name FROM servers", [], |r| r.get(0))?;
                let error: Option<String> =
                    c.query_row("SELECT last_error FROM server_state", [], |r| r.get(0))?;
                let kind: Option<String> =
                    c.query_row("SELECT last_error_kind FROM server_state", [], |r| r.get(0))?;
                Ok((version, name, error, kind))
            })
            .await
            .expect("read back");

        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(name, "web-01", "the server row was lost");
        assert_eq!(
            error.as_deref(),
            Some("authentication failed: bad key"),
            "the existing message was lost"
        );
        assert_eq!(kind, None, "an old row gains no kind, and that is fine");
    }

    #[tokio::test]
    async fn migrating_twice_changes_nothing() {
        // The registration pass and a restart both call this; it has to be idempotent.
        let database = Database::open_in_memory().await.expect("opens");
        apply(&database).await.expect("first");
        apply(&database).await.expect("second");

        let version: u32 = database
            .call(|c| c.query_row("PRAGMA user_version", [], |r| r.get(0)))
            .await
            .expect("read");
        assert_eq!(version, SCHEMA_VERSION);
    }
}
