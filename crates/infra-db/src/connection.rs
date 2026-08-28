//! Database handle and the async/blocking bridge.
//!
//! `rusqlite` is synchronous, so every call is wrapped in `spawn_blocking`. That keeps
//! the async runtime's worker threads free — a slow query stalls one blocking thread,
//! not the whole monitoring loop.

use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use vds_domain::ports::RepositoryError;

/// A handle to the application's SQLite database.
///
/// SQLite allows exactly one writer at a time regardless of how many connections exist,
/// so a single connection behind a mutex is not the bottleneck it looks like: it makes
/// the serialisation explicit instead of leaving it to lock contention inside SQLite,
/// and it removes a whole class of `SQLITE_BUSY` handling. WAL mode is what keeps this
/// acceptable — see `docs/adr/005-metrics-storage.md`.
#[derive(Clone)]
pub struct Database {
    connection: Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
}

impl Database {
    /// Opens (or creates) a database file and applies migrations.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, RepositoryError> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                RepositoryError::Backend(format!("could not create {parent:?}: {e}"))
            })?;
        }

        let for_thread = path.clone();
        let connection = tokio::task::spawn_blocking(move || {
            let connection = Connection::open(&for_thread)
                .map_err(|e| RepositoryError::Backend(format!("could not open database: {e}")))?;
            configure(&connection)?;
            Ok::<Connection, RepositoryError>(connection)
        })
        .await
        .map_err(|e| RepositoryError::Backend(format!("database task failed: {e}")))??;

        let database = Self {
            connection: Arc::new(Mutex::new(connection)),
            path: Some(path.clone()),
        };
        crate::migrations::apply(&database).await?;
        Ok(database)
    }

    /// Opens an in-memory database, for tests.
    pub async fn open_in_memory() -> Result<Self, RepositoryError> {
        let connection = Connection::open_in_memory()
            .map_err(|e| RepositoryError::Backend(format!("could not open database: {e}")))?;
        configure(&connection)?;

        let database = Self {
            connection: Arc::new(Mutex::new(connection)),
            path: None,
        };
        crate::migrations::apply(&database).await?;
        Ok(database)
    }

    /// The file backing this database, if it is not in memory.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Runs a closure against the connection on a blocking thread.
    pub(crate) async fn call<T, F>(&self, operation: F) -> Result<T, RepositoryError>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let guard = connection.lock();
            operation(&guard).map_err(map_error)
        })
        .await
        .map_err(|e| RepositoryError::Backend(format!("database task failed: {e}")))?
    }

    /// Runs a closure inside a transaction, rolling back on error.
    pub(crate) async fn transaction<T, F>(&self, operation: F) -> Result<T, RepositoryError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let mut guard = connection.lock();
            let transaction = guard.transaction().map_err(map_error)?;
            let value = operation(&transaction).map_err(map_error)?;
            transaction.commit().map_err(map_error)?;
            Ok(value)
        })
        .await
        .map_err(|e| RepositoryError::Backend(format!("database task failed: {e}")))?
    }

    /// Copies the database to another file, for pre-migration backups.
    pub async fn backup_to(&self, destination: PathBuf) -> Result<(), RepositoryError> {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let guard = connection.lock();
            guard
                .backup("main", &destination, None)
                .map_err(|e| RepositoryError::Backend(format!("backup failed: {e}")))
        })
        .await
        .map_err(|e| RepositoryError::Backend(format!("backup task failed: {e}")))?
    }

    /// Reclaims free pages. Runs as part of maintenance, never on the hot path.
    pub async fn vacuum(&self) -> Result<(), RepositoryError> {
        self.call(|connection| connection.execute_batch("VACUUM"))
            .await
    }

    /// Total size on disk in bytes, for the settings screen.
    pub async fn size_bytes(&self) -> Result<u64, RepositoryError> {
        self.call(|connection| {
            let page_count: i64 =
                connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
            let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
            Ok((page_count * page_size).max(0) as u64)
        })
        .await
    }
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.path)
            .finish()
    }
}

/// Applies the pragmas every connection needs.
fn configure(connection: &Connection) -> Result<(), RepositoryError> {
    // WAL lets readers proceed while a write is in flight, which is what makes a single
    // writer acceptable. `NORMAL` synchronous is the standard WAL pairing: it can lose
    // the last transaction on a power cut, which for monitoring history is a fine trade
    // against fsyncing on every batch.
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA temp_store = MEMORY;",
        )
        .map_err(|e| RepositoryError::Backend(format!("could not configure database: {e}")))
}

/// Translates a driver error into a domain-level one.
///
/// The application layer must never see `rusqlite::Error`; that is what makes a
/// PostgreSQL implementation possible without touching anything above the port.
pub(crate) fn map_error(error: rusqlite::Error) -> RepositoryError {
    match error {
        rusqlite::Error::QueryReturnedNoRows => RepositoryError::NotFound {
            entity: "row",
            id: String::new(),
        },
        rusqlite::Error::SqliteFailure(inner, ref message) => match inner.code {
            rusqlite::ErrorCode::ConstraintViolation => RepositoryError::Conflict(
                message
                    .clone()
                    .unwrap_or_else(|| "constraint violated".to_owned()),
            ),
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
                RepositoryError::Corrupt(error.to_string())
            }
            _ => RepositoryError::Backend(error.to_string()),
        },
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..) => {
            RepositoryError::Corrupt(error.to_string())
        }
        other => RepositoryError::Backend(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_in_memory_database_opens_and_migrates() {
        let database = Database::open_in_memory().await.expect("opens");
        assert!(database.path().is_none());

        let version = crate::migrations::current_version(&database)
            .await
            .expect("readable");
        assert_eq!(version, crate::migrations::SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn a_file_database_is_created_along_with_its_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join("vds.db");

        let database = Database::open(&path).await.expect("opens");
        assert_eq!(database.path(), Some(path.as_path()));
        assert!(path.exists());
    }

    #[tokio::test]
    async fn write_ahead_logging_is_enabled() {
        // Without WAL, a single writer connection would block every reader.
        let dir = tempfile::tempdir().expect("temp dir");
        let database = Database::open(dir.path().join("vds.db"))
            .await
            .expect("opens");

        let mode: String = database
            .call(|c| c.query_row("PRAGMA journal_mode", [], |row| row.get(0)))
            .await
            .expect("readable");
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced() {
        let database = Database::open_in_memory().await.expect("opens");
        let enabled: i64 = database
            .call(|c| c.query_row("PRAGMA foreign_keys", [], |row| row.get(0)))
            .await
            .expect("readable");
        assert_eq!(enabled, 1);
    }

    #[tokio::test]
    async fn a_failing_transaction_rolls_back() {
        let database = Database::open_in_memory().await.expect("opens");
        database
            .call(|c| c.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)"))
            .await
            .expect("created");

        let result: Result<(), RepositoryError> = database
            .transaction(|tx| {
                tx.execute("INSERT INTO t (id) VALUES (1)", [])?;
                // Duplicate primary key: the whole transaction must be discarded.
                tx.execute("INSERT INTO t (id) VALUES (1)", [])?;
                Ok(())
            })
            .await;

        assert!(
            matches!(result, Err(RepositoryError::Conflict(_))),
            "got {result:?}"
        );

        let count: i64 = database
            .call(|c| c.query_row("SELECT COUNT(*) FROM t", [], |row| row.get(0)))
            .await
            .expect("readable");
        assert_eq!(count, 0, "the successful insert must have been rolled back");
    }

    #[tokio::test]
    async fn driver_errors_are_translated_into_domain_errors() {
        // The application layer must never be able to match on rusqlite's error type.
        let database = Database::open_in_memory().await.expect("opens");
        let err = database
            .call(|c| c.execute("SELECT * FROM table_that_does_not_exist", []))
            .await
            .expect_err("must fail");
        assert!(matches!(err, RepositoryError::Backend(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn the_database_reports_its_size() {
        let database = Database::open_in_memory().await.expect("opens");
        assert!(database.size_bytes().await.expect("readable") > 0);
    }

    #[tokio::test]
    async fn a_database_can_be_backed_up_to_a_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let database = Database::open_in_memory().await.expect("opens");
        let destination = dir.path().join("backup.db");

        database
            .backup_to(destination.clone())
            .await
            .expect("backed up");
        assert!(destination.exists());

        // The copy must be a usable database, not just bytes on disk.
        let restored = Database::open(&destination).await.expect("reopens");
        assert_eq!(
            crate::migrations::current_version(&restored)
                .await
                .expect("readable"),
            crate::migrations::SCHEMA_VERSION
        );
    }

    #[tokio::test]
    async fn the_handle_is_cheap_to_clone_and_shares_one_database() {
        let database = Database::open_in_memory().await.expect("opens");
        let clone = database.clone();

        database
            .call(|c| c.execute_batch("CREATE TABLE shared (id INTEGER)"))
            .await
            .expect("created");
        // The clone sees it, so both handles are talking to the same connection.
        clone
            .call(|c| c.execute("INSERT INTO shared (id) VALUES (1)", []))
            .await
            .expect("inserted");
    }
}
