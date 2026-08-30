//! Tests for [`FileService`], against a browser that records what it was asked.
//!
//! The point of these is not that the commands work — that is settled in
//! `vds-infra-collectors::files` against captured output. It is that this layer never
//! hands the transport something different from what the user asked for, and never
//! changes a file without saying so.

use super::*;
use crate::testing::FakeServerRepository;
use async_trait::async_trait;
use parking_lot::Mutex;
use std::collections::HashMap;
use vds_domain::ids::CredentialRef;
use vds_domain::ports::{EntryKind, RecordingEventPublisher};
use vds_domain::server::{ConnectionSettings, SshAuthKind, SshSettings};

/// A browser that answers from a script and remembers every path it was given.
#[derive(Default)]
struct SpyBrowser {
    listings: Mutex<HashMap<String, Vec<DirectoryEntry>>>,
    files: Mutex<HashMap<String, String>>,
    calls: Mutex<Vec<(&'static str, String)>>,
}

impl SpyBrowser {
    fn with_directory(self, path: &str, names: &[(&str, EntryKind)]) -> Self {
        let entries = names
            .iter()
            .map(|(name, kind)| DirectoryEntry {
                name: (*name).to_owned(),
                kind: *kind,
                size_bytes: 0,
                modified: None,
                mode: "rw-r--r--".into(),
                owner: "root".into(),
                group: "root".into(),
                target: None,
            })
            .collect();
        self.listings.lock().insert(path.to_owned(), entries);
        self
    }

    fn with_file(self, path: &str, contents: &str) -> Self {
        self.files
            .lock()
            .insert(path.to_owned(), contents.to_owned());
        self
    }

    fn paths_for(&self, operation: &str) -> Vec<String> {
        self.calls
            .lock()
            .iter()
            .filter(|(op, _)| *op == operation)
            .map(|(_, path)| path.clone())
            .collect()
    }

    fn record(&self, operation: &'static str, path: &str) {
        self.calls.lock().push((operation, path.to_owned()));
    }
}

#[async_trait]
impl FileBrowser for SpyBrowser {
    async fn list(&self, _: &Server, path: &str) -> Result<Vec<DirectoryEntry>, FileError> {
        self.record("list", path);
        self.listings
            .lock()
            .get(path)
            .cloned()
            .ok_or_else(|| FileError::NotFound(path.to_owned()))
    }

    async fn read_bytes(
        &self,
        _: &Server,
        path: &str,
        _: u64,
    ) -> Result<vds_domain::ports::FileBytes, FileError> {
        self.record("read", path);
        let text = self
            .files
            .lock()
            .get(path)
            .cloned()
            .ok_or_else(|| FileError::NotFound(path.to_owned()))?;
        Ok(vds_domain::ports::FileBytes {
            size_bytes: text.len() as u64,
            truncated: false,
            bytes: text.into_bytes(),
        })
    }

    async fn read(&self, _: &Server, path: &str, _: u64) -> Result<FileContents, FileError> {
        self.record("read", path);
        let text = self
            .files
            .lock()
            .get(path)
            .cloned()
            .ok_or_else(|| FileError::NotFound(path.to_owned()))?;
        Ok(FileContents {
            size_bytes: text.len() as u64,
            truncated: false,
            text,
        })
    }

    async fn write(&self, _: &Server, path: &str, contents: &str) -> Result<(), FileError> {
        self.record("write", path);
        self.files
            .lock()
            .insert(path.to_owned(), contents.to_owned());
        Ok(())
    }

    async fn delete(&self, _: &Server, path: &str) -> Result<(), FileError> {
        self.record("delete", path);
        Ok(())
    }

    async fn create_directory(&self, _: &Server, path: &str) -> Result<(), FileError> {
        self.record("mkdir", path);
        Ok(())
    }
}

struct Fixture {
    service: FileService,
    browser: Arc<SpyBrowser>,
    events: Arc<RecordingEventPublisher>,
    server_id: ServerId,
}

fn fixture(browser: SpyBrowser) -> Fixture {
    let servers = Arc::new(FakeServerRepository::new());
    let server = Server::new(
        "web-01",
        "10.0.0.5",
        ConnectionSettings::Ssh(SshSettings {
            username: "root".into(),
            auth_kind: SshAuthKind::PrivateKey,
            credential_ref: CredentialRef::new(),
        }),
        chrono::Utc::now(),
    );
    let server_id = server.id;
    servers.insert(server);

    let browser = Arc::new(browser);
    let events = Arc::new(RecordingEventPublisher::new());
    Fixture {
        service: FileService::new(
            Arc::clone(&browser) as Arc<dyn FileBrowser>,
            servers,
            Arc::clone(&events) as Arc<dyn EventPublisher>,
        ),
        browser,
        events,
        server_id,
    }
}

#[tokio::test]
async fn a_path_is_normalised_before_it_reaches_the_transport() {
    // What the transport is asked for must be what the application believes it asked
    // for, not a string a shell will reinterpret.
    let f = fixture(SpyBrowser::default().with_directory("/var/log", &[]));

    let listing = f
        .service
        .list(f.server_id, "/var/www/../log/")
        .await
        .expect("lists");

    assert_eq!(listing.path, "/var/log");
    assert_eq!(f.browser.paths_for("list"), ["/var/log"]);
}

#[tokio::test]
async fn a_change_is_recorded_where_it_can_be_reviewed_afterwards() {
    // This is the only part of the product that alters a server. If a stolen credential
    // is ever used through it, the event log is the record of what it touched.
    let f = fixture(SpyBrowser::default().with_file("/etc/nginx/nginx.conf", "old"));

    f.service
        .write(f.server_id, "/etc/nginx/nginx.conf", "new")
        .await
        .expect("writes");
    f.service
        .delete(f.server_id, "/tmp/old.log")
        .await
        .expect("deletes");
    f.service
        .create_directory(f.server_id, "/var/www/new")
        .await
        .expect("creates");

    let recorded: Vec<(String, FileAction)> = f
        .events
        .events()
        .into_iter()
        .filter_map(|event| match event {
            DomainEvent::FileChanged { path, action, .. } => Some((path, action)),
            _ => None,
        })
        .collect();

    assert_eq!(
        recorded,
        [
            ("/etc/nginx/nginx.conf".to_owned(), FileAction::Written),
            ("/tmp/old.log".to_owned(), FileAction::Deleted),
            ("/var/www/new".to_owned(), FileAction::DirectoryCreated),
        ]
    );
}

#[tokio::test]
async fn nothing_is_recorded_when_the_change_did_not_happen() {
    // An audit trail that logs attempts as if they were changes is worse than none.
    let f = fixture(SpyBrowser::default());

    let outcome = f.service.delete(f.server_id, "/").await;

    assert!(outcome.is_err());
    assert!(f.events.is_empty(), "{:?}", f.events.events());
    assert!(
        f.browser.paths_for("delete").is_empty(),
        "it reached the server"
    );
}

#[tokio::test]
async fn the_root_directory_is_not_deletable() {
    // `/` is what an empty field, a stray `..` or a blank name all normalise to.
    let f = fixture(SpyBrowser::default());

    for path in ["/", "", "/var/www/../..", "/.."] {
        let outcome = f.service.delete(f.server_id, path).await;
        assert!(outcome.is_err(), "{path:?} was allowed through");
    }
}

#[tokio::test]
async fn the_folders_of_the_sites_are_read_from_the_servers_own_configuration() {
    let f = fixture(
        SpyBrowser::default()
            .with_directory(
                "/etc/nginx/sites-enabled",
                &[
                    ("example", EntryKind::File),
                    ("archive", EntryKind::Directory),
                ],
            )
            .with_file(
                "/etc/nginx/sites-enabled/example",
                "server { server_name example.ru; root /home/deploy/example/public; }",
            ),
    );

    let roots = f.service.site_roots(f.server_id).await.expect("discovers");

    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, "/home/deploy/example/public");
    assert_eq!(roots[0].label(), "example.ru");
    assert_eq!(roots[0].source, "/etc/nginx/sites-enabled/example");
    // A directory inside `sites-enabled` is not a configuration file and is not opened.
    assert!(
        !f.browser
            .paths_for("read")
            .contains(&"/etc/nginx/sites-enabled/archive".to_owned())
    );
}

#[tokio::test]
async fn a_server_without_a_web_server_reports_no_folders_rather_than_failing() {
    // Every configuration directory is missing here. Failing would make the feature
    // unusable on any machine that is not the one distribution it was written against.
    let f = fixture(SpyBrowser::default());

    assert_eq!(
        f.service.site_roots(f.server_id).await.expect("succeeds"),
        []
    );
}

#[tokio::test]
async fn a_missing_server_is_reported_before_anything_is_attempted() {
    let f = fixture(SpyBrowser::default());

    let outcome = f.service.list(ServerId::new(), "/var/www").await;

    assert!(outcome.is_err());
    assert!(f.browser.paths_for("list").is_empty());
}
