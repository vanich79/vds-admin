//! Browsing and editing files on a monitored server.
//!
//! # What this layer adds over the port
//!
//! [`FileBrowser`] talks to one machine. This decides *which* machine, *where* browsing
//! is allowed to start, and *what gets recorded* — the three things that separate a file
//! browser from a remote shell.
//!
//! * **Paths are normalised before they leave.** `..` is resolved here, so what the
//!   application asks for is what it thinks it asked for. It is not forbidden — an
//!   administrator navigating out of `/var/www` is doing their job — but it is resolved
//!   rather than passed through.
//! * **Every change is published.** A write or a delete raises
//!   [`DomainEvent::FileChanged`]. This is the only part of the product that alters a
//!   server, and the event log is what makes that reviewable afterwards.
//! * **Roots are discovered, not typed.** [`FileService::site_roots`] reads the web
//!   server's own configuration, so "the folder of this site" means what nginx thinks it
//!   means rather than what someone remembered.

use std::sync::Arc;
use vds_domain::events::{DomainEvent, FileAction};
use vds_domain::ids::ServerId;
use vds_domain::ports::{
    DEFAULT_MAX_READ_BYTES, DirectoryEntry, EventPublisher, FileBrowser, FileContents, FileError,
    ServerRepository, shell_quote,
};
use vds_domain::server::Server;

mod preview;
mod roots;

pub use preview::{ImageFile, MAX_IMAGE_BYTES, MAX_TEXT_BYTES, Preview, image_format, read_budget};
pub use roots::{
    APACHE_CONFIG_DIRS, NGINX_CONFIG_DIRS, SiteRoot, parse_apache_roots, parse_nginx_roots,
};

/// Where browsing starts when nothing better is known.
///
/// Not `/`: the first thing an operator of a web server wants is the web root, and a
/// listing of `/` is a wall of system directories.
pub const DEFAULT_START_PATH: &str = "/var/www";

/// How much of a virtual-host file is read while looking for document roots.
///
/// A configuration this size is already unusual; one large enough to matter here is a
/// generated file that is not describing a site anyone browses.
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

/// Reading and changing files on the fleet.
pub struct FileService {
    browser: Arc<dyn FileBrowser>,
    servers: Arc<dyn ServerRepository>,
    events: Arc<dyn EventPublisher>,
}

impl FileService {
    pub fn new(
        browser: Arc<dyn FileBrowser>,
        servers: Arc<dyn ServerRepository>,
        events: Arc<dyn EventPublisher>,
    ) -> Self {
        Self {
            browser,
            servers,
            events,
        }
    }

    /// Lists a directory.
    pub async fn list(&self, server_id: ServerId, path: &str) -> Result<Listing, FileError> {
        let server = self.server(server_id).await?;
        let path = normalise(path);
        let entries = self.browser.list(&server, &path).await?;
        Ok(Listing { path, entries })
    }

    /// Reads a file as text.
    pub async fn read(&self, server_id: ServerId, path: &str) -> Result<FileContents, FileError> {
        let server = self.server(server_id).await?;
        self.browser
            .read(&server, &normalise(path), DEFAULT_MAX_READ_BYTES)
            .await
    }

    /// Opens a file, whatever it turns out to be.
    ///
    /// One round trip decides everything: a picture is previewed, text is edited, and
    /// anything else is reported by size. The alternative — asking what kind of file it
    /// is and then fetching it — is two round trips to answer a question the bytes
    /// already answer.
    pub async fn open(&self, server_id: ServerId, path: &str) -> Result<Preview, FileError> {
        let server = self.server(server_id).await?;
        let path = normalise(path);
        // The path picks the budget and nothing else; what the file *is* comes from the
        // bytes that arrive.
        let budget = preview::read_budget(&path);
        let raw = self.browser.read_bytes(&server, &path, budget).await?;
        Ok(preview::classify(raw, &path))
    }

    /// Replaces a file's contents.
    pub async fn write(
        &self,
        server_id: ServerId,
        path: &str,
        contents: &str,
    ) -> Result<(), FileError> {
        let server = self.server(server_id).await?;
        let path = normalise(path);
        self.browser.write(&server, &path, contents).await?;
        self.record(server_id, path, FileAction::Written);
        Ok(())
    }

    /// Deletes a file or an empty directory.
    pub async fn delete(&self, server_id: ServerId, path: &str) -> Result<(), FileError> {
        let server = self.server(server_id).await?;
        let path = normalise(path);

        // Refusing this is worth the line of code. `/` normalises from a surprising number
        // of mistakes — an empty field, a stray `..`, a path built from a name that turned
        // out to be blank — and no legitimate use of this tool deletes it.
        if path == "/" {
            return Err(FileError::Malformed(
                "refusing to delete the root directory".to_owned(),
            ));
        }

        self.browser.delete(&server, &path).await?;
        self.record(server_id, path, FileAction::Deleted);
        Ok(())
    }

    /// Creates a directory, including missing parents.
    pub async fn create_directory(&self, server_id: ServerId, path: &str) -> Result<(), FileError> {
        let server = self.server(server_id).await?;
        let path = normalise(path);
        self.browser.create_directory(&server, &path).await?;
        self.record(server_id, path, FileAction::DirectoryCreated);
        Ok(())
    }

    /// The document roots the server's own web server configuration declares.
    ///
    /// Reading the configuration rather than guessing means the listed folders are the
    /// ones actually being served, including the ones nobody remembers setting up. A
    /// machine with no web server, or one whose configuration this account may not read,
    /// simply has nothing to declare — that is not a failure, and browsing falls back to
    /// [`DEFAULT_START_PATH`].
    pub async fn site_roots(&self, server_id: ServerId) -> Result<Vec<SiteRoot>, FileError> {
        let server = self.server(server_id).await?;
        let mut found: Vec<SiteRoot> = Vec::new();

        for (directory, nginx) in NGINX_CONFIG_DIRS
            .iter()
            .map(|d| (*d, true))
            .chain(APACHE_CONFIG_DIRS.iter().map(|d| (*d, false)))
        {
            let Ok(entries) = self.browser.list(&server, directory).await else {
                // Absent or unreadable. Either way there is nothing here to report, and
                // failing the whole call because one distribution's path does not exist
                // would make this useless on every other distribution.
                continue;
            };

            for entry in entries {
                if !entry.kind.is_readable() {
                    continue;
                }
                let path = join(directory, &entry.name);
                let Ok(contents) = self.browser.read(&server, &path, MAX_CONFIG_BYTES).await else {
                    continue;
                };

                let parsed = if nginx {
                    parse_nginx_roots(&contents.text, &path)
                } else {
                    parse_apache_roots(&contents.text, &path)
                };
                for site in parsed {
                    if !found.iter().any(|existing| existing.path == site.path) {
                        found.push(site);
                    }
                }
            }
        }

        Ok(found)
    }

    /// Resolves the server, so a stale id in the interface fails here rather than as a
    /// confusing connection error.
    async fn server(&self, server_id: ServerId) -> Result<Server, FileError> {
        self.servers
            .get(server_id)
            .await
            .map_err(|e| FileError::Malformed(e.to_string()))
    }

    fn record(&self, server_id: ServerId, path: String, action: FileAction) {
        self.events.publish(DomainEvent::FileChanged {
            server_id,
            path,
            action,
        });
    }
}

impl std::fmt::Debug for FileService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileService").finish_non_exhaustive()
    }
}

/// One directory's contents, with the path they were read from.
///
/// The path is returned rather than assumed: it has been normalised, so it may differ
/// from what was asked for, and the interface has to show where it actually is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<DirectoryEntry>,
}

impl Listing {
    /// The containing directory, or `None` at the root.
    pub fn parent(&self) -> Option<String> {
        parent_of(&self.path)
    }
}

/// The directory containing `path`, or `None` if there is none.
pub fn parent_of(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('/');
    let cut = trimmed.rfind('/')?;
    if cut == 0 {
        Some("/".to_owned())
    } else {
        Some(trimmed[..cut].to_owned())
    }
}

/// Joins a directory and an entry name.
pub fn join(directory: &str, name: &str) -> String {
    if directory.ends_with('/') {
        format!("{directory}{name}")
    } else {
        format!("{directory}/{name}")
    }
}

/// Resolves a path to an absolute, `..`-free form.
///
/// Done here rather than left to the shell so that what is sent is what was meant. `..`
/// is resolved, not rejected: navigating up out of a web root is ordinary administration.
/// Resolution is textual, which is the correct choice — a symlink's target is shown to
/// the user unresolved, and silently following it here would contradict that.
pub fn normalise(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", parts.join("/"))
    }
}

/// Whether a path is one this application will let a user browse to at all.
///
/// Deliberately permissive: an administrator's own server is theirs to look at, and a
/// tool that hid `/etc` while offering an editor would be pretending. What it does catch
/// is a path that could not have come from navigation — one that would not survive being
/// quoted intact.
pub fn is_browsable(path: &str) -> bool {
    let normalised = normalise(path);
    !normalised.contains('\0') && shell_quote(&normalised).len() >= normalised.len() + 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_resolved_rather_than_passed_through() {
        assert_eq!(normalise("/var/www/html"), "/var/www/html");
        assert_eq!(normalise("/var/www/../log"), "/var/log");
        assert_eq!(normalise("/var//www///html/"), "/var/www/html");
        assert_eq!(normalise("var/www"), "/var/www");
        assert_eq!(normalise("./x/./y"), "/x/y");
    }

    #[test]
    fn climbing_above_the_root_stops_at_the_root() {
        // Not an error: a shell does the same thing, and pretending otherwise would only
        // produce a path nobody could reach.
        assert_eq!(normalise("/../../../etc"), "/etc");
        assert_eq!(normalise("/.."), "/");
        assert_eq!(normalise(""), "/");
        assert_eq!(normalise("/"), "/");
    }

    #[test]
    fn walking_up_ends_at_the_root() {
        assert_eq!(parent_of("/var/www/html"), Some("/var/www".to_owned()));
        assert_eq!(parent_of("/var/www/"), Some("/var".to_owned()));
        assert_eq!(parent_of("/var"), Some("/".to_owned()));
        assert_eq!(parent_of("/"), None);
    }

    #[test]
    fn joining_never_doubles_the_separator() {
        assert_eq!(join("/var/www", "html"), "/var/www/html");
        assert_eq!(join("/", "etc"), "/etc");
    }

    #[test]
    fn a_path_with_an_embedded_nul_is_not_browsable() {
        // It cannot survive as a C string on the far side, so sending it would produce a
        // silently different path.
        assert!(is_browsable("/var/www"));
        assert!(!is_browsable("/var/\0www"));
    }
}

#[cfg(test)]
mod service_tests;
