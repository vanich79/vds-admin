//! Conversions between domain types and their stored representation.
//!
//! Two rules, applied everywhere:
//!
//! * timestamps are stored as milliseconds since the Unix epoch, because seconds lose
//!   ordering within a collection cycle and RFC-3339 text costs four times the space
//!   across millions of rows;
//! * a row that cannot be interpreted produces [`RepositoryError::Corrupt`] naming the
//!   column, never a panic and never a silent default.

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Row, ToSql};
use serde::Serialize;
use serde::de::DeserializeOwned;
use vds_domain::ids::*;
use vds_domain::ports::RepositoryError;

/// Milliseconds since the Unix epoch.
pub fn to_millis(value: DateTime<Utc>) -> i64 {
    value.timestamp_millis()
}

/// Reconstructs a timestamp from stored milliseconds.
pub fn from_millis(millis: i64) -> Result<DateTime<Utc>, RepositoryError> {
    DateTime::from_timestamp_millis(millis)
        .ok_or_else(|| RepositoryError::Corrupt(format!("{millis} is not a valid timestamp")))
}

/// Reads a nullable timestamp column.
pub fn optional_millis(value: Option<i64>) -> Result<Option<DateTime<Utc>>, RepositoryError> {
    value.map(from_millis).transpose()
}

/// Serialises a value to JSON for storage.
pub fn to_json<T: Serialize>(value: &T) -> Result<String, rusqlite::Error> {
    serde_json::to_string(value).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
    })
}

/// Parses a JSON column, naming the column when it fails.
pub fn from_json<T: DeserializeOwned>(column: &str, raw: &str) -> Result<T, RepositoryError> {
    serde_json::from_str(raw)
        .map_err(|e| RepositoryError::Corrupt(format!("column {column} is not valid JSON: {e}")))
}

/// Parses a UUID column, naming the column when it fails.
pub fn parse_uuid(column: &str, raw: &str) -> Result<uuid::Uuid, RepositoryError> {
    uuid::Uuid::parse_str(raw)
        .map_err(|_| RepositoryError::Corrupt(format!("column {column} is not a UUID: {raw:?}")))
}

/// Formats a date for storage.
///
/// Analytics ranges are stored as text because they are *dates*, not instants: storing
/// them as timestamps invites timezone arithmetic that shifts traffic reports by a day.
pub fn format_date(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

/// Turns a domain-level conversion failure back into a driver error.
///
/// Row readers run inside `rusqlite`'s callback, which can only return
/// `rusqlite::Error`. Wrapping it as a conversion failure means
/// [`crate::connection::map_error`] classifies it as
/// [`RepositoryError::Corrupt`] on the way back out, so the original meaning survives
/// the round trip.
pub fn corrupt(error: RepositoryError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::other(error.to_string())),
    )
}

/// Makes typed identifiers usable directly as query parameters and column values.
macro_rules! sql_id {
    ($name:ident) => {
        impl ToSql for Sql<$name> {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.0.to_string()))
            }
        }

        impl FromSql for Sql<$name> {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let text = value.as_str()?;
                uuid::Uuid::parse_str(text)
                    .map(|uuid| Sql($name::from_uuid(uuid)))
                    .map_err(|e| FromSqlError::Other(Box::new(e)))
            }
        }
    };
}

/// Newtype that carries a domain id across the SQL boundary.
///
/// Exists because the orphan rule forbids implementing `rusqlite`'s traits directly on
/// the domain's id types — and that is exactly right: the domain must not know SQL
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sql<T>(pub T);

impl<T> Sql<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

sql_id!(ServerId);
sql_id!(WebsiteId);
sql_id!(IntegrationId);
sql_id!(AlertRuleId);
sql_id!(IncidentId);
sql_id!(EventId);
sql_id!(CredentialRef);

/// Reads a typed id from a column.
pub fn id_column<T>(row: &Row<'_>, index: usize) -> Result<T, rusqlite::Error>
where
    Sql<T>: FromSql,
{
    row.get::<_, Sql<T>>(index).map(Sql::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip_with_millisecond_precision() {
        let original = DateTime::from_timestamp_millis(1_700_000_000_123).expect("valid");
        assert_eq!(from_millis(to_millis(original)).expect("valid"), original);
    }

    #[test]
    fn sub_second_ordering_survives_storage() {
        // Two samples from the same collection cycle must remain distinguishable.
        let first = DateTime::from_timestamp_millis(1_700_000_000_100).expect("valid");
        let second = DateTime::from_timestamp_millis(1_700_000_000_900).expect("valid");
        assert!(to_millis(first) < to_millis(second));
    }

    #[test]
    fn an_impossible_timestamp_is_corruption_not_a_panic() {
        let err = from_millis(i64::MAX).expect_err("must fail");
        assert!(matches!(err, RepositoryError::Corrupt(_)));
    }

    #[test]
    fn nullable_timestamps_stay_null() {
        assert_eq!(optional_millis(None).expect("valid"), None);
        assert!(optional_millis(Some(0)).expect("valid").is_some());
    }

    #[test]
    fn malformed_json_names_the_column_it_came_from() {
        let err = from_json::<Vec<String>>("tags_json", "not json").expect_err("must fail");
        assert!(err.to_string().contains("tags_json"), "message was: {err}");
        assert!(matches!(err, RepositoryError::Corrupt(_)));
    }

    #[test]
    fn malformed_uuids_name_their_column() {
        let err = parse_uuid("server_id", "nope").expect_err("must fail");
        assert!(err.to_string().contains("server_id"), "message was: {err}");
    }

    #[test]
    fn dates_are_stored_in_sortable_iso_form() {
        // Sortable matters: range columns are compared as text in the WHERE clause.
        let date = NaiveDate::from_ymd_opt(2026, 8, 26).expect("valid");
        assert_eq!(format_date(date), "2026-08-26");
        assert!(
            format_date(NaiveDate::from_ymd_opt(2026, 1, 2).expect("valid")) < format_date(date)
        );
    }

    #[test]
    fn a_domain_error_survives_the_round_trip_through_the_driver() {
        let wrapped = corrupt(RepositoryError::Corrupt(
            "tags_json is not valid JSON".into(),
        ));
        let back = crate::connection::map_error(wrapped);
        assert!(matches!(back, RepositoryError::Corrupt(_)), "got {back:?}");
        assert!(
            back.to_string().contains("tags_json"),
            "message was: {back}"
        );
    }

    #[test]
    fn ids_round_trip_through_their_sql_wrapper() {
        let connection = rusqlite::Connection::open_in_memory().expect("opens");
        connection
            .execute_batch("CREATE TABLE t (id TEXT)")
            .expect("created");

        let id = ServerId::new();
        connection
            .execute("INSERT INTO t (id) VALUES (?1)", [Sql(id)])
            .expect("inserted");

        let read: ServerId = connection
            .query_row("SELECT id FROM t", [], |row| id_column(row, 0))
            .expect("readable");
        assert_eq!(read, id);
    }

    #[test]
    fn a_non_uuid_id_column_is_an_error_not_a_default_id() {
        let connection = rusqlite::Connection::open_in_memory().expect("opens");
        connection
            .execute_batch("CREATE TABLE t (id TEXT)")
            .expect("created");
        connection
            .execute("INSERT INTO t (id) VALUES ('garbage')", [])
            .expect("inserted");

        let result: Result<ServerId, _> =
            connection.query_row("SELECT id FROM t", [], |row| id_column(row, 0));
        assert!(result.is_err());
    }
}
