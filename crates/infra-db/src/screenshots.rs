//! SQLite implementation of [`ScreenshotRepository`].
//!
//! Only metadata lives here. The image bytes are files on disk: storing megabytes of PNG
//! in SQLite would bloat the database, slow every unrelated query that has to skip past
//! those pages, and make the whole thing painful to back up.

use crate::connection::Database;
use crate::convert::*;
use async_trait::async_trait;
use rusqlite::Row;
use vds_domain::ids::{ProviderId, WebsiteId};
use vds_domain::ports::{RepositoryError, ScreenshotRepository};
use vds_domain::screenshot::{Screenshot, ScreenshotStatus};

/// Stores screenshot metadata.
#[derive(Debug, Clone)]
pub struct SqliteScreenshotRepository {
    database: Database,
}

impl SqliteScreenshotRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

const COLUMNS: &str =
    "website_id, provider, path, thumbnail_path, captured_at, status_json, hash, width, height";

fn read_screenshot(row: &Row<'_>) -> Result<Screenshot, rusqlite::Error> {
    let provider: String = row.get(1)?;
    let path: String = row.get(2)?;
    let thumbnail_path: Option<String> = row.get(3)?;
    let captured_at: i64 = row.get(4)?;
    let status_json: String = row.get(5)?;
    let hash: String = row.get(6)?;
    let width: i64 = row.get(7)?;
    let height: i64 = row.get(8)?;

    Ok(Screenshot {
        website_id: id_column(row, 0)?,
        provider: ProviderId::new(provider),
        path,
        thumbnail_path,
        captured_at: from_millis(captured_at).map_err(corrupt)?,
        status: from_json::<ScreenshotStatus>("screenshots.status_json", &status_json)
            .map_err(corrupt)?,
        hash,
        width: width.max(0) as u32,
        height: height.max(0) as u32,
    })
}

#[async_trait]
impl ScreenshotRepository for SqliteScreenshotRepository {
    async fn get(&self, website: WebsiteId) -> Result<Option<Screenshot>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!("SELECT {COLUMNS} FROM screenshots WHERE website_id = ?1");
                let mut statement = connection.prepare(&sql)?;
                statement
                    .query_map([Sql(website)], read_screenshot)?
                    .next()
                    .transpose()
            })
            .await
    }

    async fn save(&self, screenshot: &Screenshot) -> Result<(), RepositoryError> {
        let screenshot = screenshot.clone();
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO screenshots (website_id, provider, path, thumbnail_path,
                         captured_at, status_json, hash, width, height)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     ON CONFLICT(website_id) DO UPDATE SET
                         provider = excluded.provider,
                         path = excluded.path,
                         thumbnail_path = excluded.thumbnail_path,
                         captured_at = excluded.captured_at,
                         status_json = excluded.status_json,
                         hash = excluded.hash,
                         width = excluded.width,
                         height = excluded.height",
                    rusqlite::params![
                        Sql(screenshot.website_id),
                        screenshot.provider.as_str(),
                        screenshot.path,
                        screenshot.thumbnail_path,
                        to_millis(screenshot.captured_at),
                        to_json(&screenshot.status)?,
                        screenshot.hash,
                        i64::from(screenshot.width),
                        i64::from(screenshot.height),
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn list(&self) -> Result<Vec<Screenshot>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!("SELECT {COLUMNS} FROM screenshots ORDER BY captured_at DESC");
                let mut statement = connection.prepare(&sql)?;
                statement.query_map([], read_screenshot)?.collect()
            })
            .await
    }

    async fn delete(&self, website: WebsiteId) -> Result<(), RepositoryError> {
        self.database
            .call(move |connection| {
                connection.execute(
                    "DELETE FROM screenshots WHERE website_id = ?1",
                    [Sql(website)],
                )?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    struct Harness {
        repository: SqliteScreenshotRepository,
        website: WebsiteId,
    }

    async fn harness() -> Harness {
        let database = Database::open_in_memory().await.expect("opens");
        let website = WebsiteId::new();
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
            repository: SqliteScreenshotRepository::new(database),
            website,
        }
    }

    fn screenshot(website: WebsiteId, status: ScreenshotStatus) -> Screenshot {
        Screenshot {
            website_id: website,
            provider: ProviderId::new("chromium_cli"),
            path: "abc.png".into(),
            thumbnail_path: Some("abc.thumb.png".into()),
            captured_at: at(1_000),
            status,
            hash: "deadbeefdeadbeef".into(),
            width: 1_280,
            height: 800,
        }
    }

    #[tokio::test]
    async fn a_screenshot_round_trips() {
        let h = harness().await;
        let shot = screenshot(h.website, ScreenshotStatus::Captured);
        h.repository.save(&shot).await.expect("saved");

        let loaded = h
            .repository
            .get(h.website)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(loaded, shot);
    }

    #[tokio::test]
    async fn a_failure_reason_survives_storage() {
        // The UI needs the reason to offer a meaningful retry, not just "failed".
        let h = harness().await;
        let shot = screenshot(
            h.website,
            ScreenshotStatus::Failed {
                reason: "navigation timed out".into(),
            },
        );
        h.repository.save(&shot).await.expect("saved");

        let loaded = h
            .repository
            .get(h.website)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(
            loaded.status,
            ScreenshotStatus::Failed {
                reason: "navigation timed out".into()
            }
        );
        assert!(!loaded.status.is_captured());
    }

    #[tokio::test]
    async fn every_status_variant_round_trips() {
        let h = harness().await;
        for status in [
            ScreenshotStatus::Captured,
            ScreenshotStatus::WebsiteOffline,
            ScreenshotStatus::Unavailable,
            ScreenshotStatus::Failed {
                reason: "no browser".into(),
            },
        ] {
            let shot = screenshot(h.website, status.clone());
            h.repository.save(&shot).await.expect("saved");
            assert_eq!(
                h.repository
                    .get(h.website)
                    .await
                    .expect("read")
                    .expect("present")
                    .status,
                status
            );
        }
    }

    #[tokio::test]
    async fn saving_again_replaces_rather_than_duplicating() {
        let h = harness().await;
        h.repository
            .save(&screenshot(h.website, ScreenshotStatus::Captured))
            .await
            .expect("saved");

        let mut updated = screenshot(h.website, ScreenshotStatus::Captured);
        updated.captured_at = at(9_000);
        updated.hash = "0123456789abcdef".into();
        h.repository.save(&updated).await.expect("saved");

        assert_eq!(h.repository.list().await.expect("listed").len(), 1);
        let loaded = h
            .repository
            .get(h.website)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(loaded.captured_at, at(9_000));
        assert_eq!(loaded.hash, "0123456789abcdef");
    }

    #[tokio::test]
    async fn a_website_with_no_capture_yet_is_absent_not_an_error() {
        let h = harness().await;
        assert_eq!(h.repository.get(h.website).await.expect("read"), None);
    }

    #[tokio::test]
    async fn deleting_a_website_removes_its_screenshot_record() {
        let h = harness().await;
        h.repository
            .save(&screenshot(h.website, ScreenshotStatus::Captured))
            .await
            .expect("saved");

        let website = h.website;
        h.repository
            .database
            .call(move |c| c.execute("DELETE FROM websites WHERE id = ?1", [Sql(website)]))
            .await
            .expect("deleted");

        assert!(h.repository.list().await.expect("listed").is_empty());
    }

    #[tokio::test]
    async fn only_metadata_is_stored_never_the_image_bytes() {
        // A database that fills with PNGs slows every unrelated query.
        let h = harness().await;
        h.repository
            .save(&screenshot(h.website, ScreenshotStatus::Captured))
            .await
            .expect("saved");

        let columns: Vec<String> = h
            .repository
            .database
            .call(|c| {
                let mut s = c.prepare("SELECT name FROM pragma_table_info('screenshots')")?;
                s.query_map([], |row| row.get::<_, String>(0))?.collect()
            })
            .await
            .expect("readable");

        assert!(columns.iter().any(|c| c == "path"));
        assert!(
            !columns
                .iter()
                .any(|c| c.contains("blob") || c.contains("bytes"))
        );
    }
}
