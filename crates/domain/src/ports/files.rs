//! Browsing and changing files on a monitored server.
//!
//! # What this changes about the application
//!
//! Everything else here reads. This writes, and that is a different thing to hand a
//! stolen credential: until now the worst an attacker could do with the application's
//! SSH key was learn how busy the machine was. Three consequences follow, and they are
//! design constraints rather than advice:
//!
//! * **every path is quoted.** Commands are shell strings, so a path is an injection
//!   vector. [`crate::ports::shell_quote`] is the only thing between what a user types
//!   and arbitrary execution, and nothing here builds a command without it.
//! * **reads are bounded.** A log file is not a text file with a size you can assume;
//!   pulling `/var/log/syslog` into a window would consume the machine's memory before
//!   anyone could stop it.
//! * **writes are not in-place.** Content is written beside the target and moved over
//!   it, so an interrupted transfer leaves the original intact rather than truncated.
//!
//! # Why it lives behind a port
//!
//! The same operations have to work over SSH today and through the agent later, and the
//! agent deliberately has no write endpoint. Keeping this a trait means the decision
//! about *which* transports may do it stays in the composition root, where it can be
//! read in one place.

use crate::ports::TransportError;
use crate::server::Server;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
    /// A symbolic link. The target is carried separately, unresolved: resolving it here
    /// would hide the fact that it is a link, and a link into somewhere unexpected is
    /// exactly what an operator wants to see.
    Symlink,
    /// A socket, device, or named pipe. Listed, never opened.
    Other,
}

impl EntryKind {
    /// Whether this entry can be entered.
    pub fn is_directory(self) -> bool {
        matches!(self, EntryKind::Directory)
    }

    /// Whether reading it as text could make sense.
    ///
    /// A device file that answers `read` forever is the reason this is asked before
    /// anything is opened.
    pub fn is_readable(self) -> bool {
        matches!(self, EntryKind::File)
    }
}

/// One entry in a directory listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size_bytes: u64,
    pub modified: Option<DateTime<Utc>>,
    /// The permission string as the system reports it, e.g. `rw-r--r--`.
    pub mode: String,
    pub owner: String,
    pub group: String,
    /// Where a symlink points, exactly as recorded — not resolved.
    pub target: Option<String>,
}

impl DirectoryEntry {
    /// Whether the name begins with a dot.
    ///
    /// Hidden files are listed, not filtered: `.env` and `.htaccess` are among the most
    /// interesting files on a web server, and hiding them by default would be a strange
    /// thing for an administration tool to do.
    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// How much of a file is worth pulling across a link that may be a phone's.
///
/// Large enough for any configuration file, a certificate, or a page of source; small
/// enough that hitting it by accident on `/var/log/syslog` costs a moment rather than the
/// machine's memory.
pub const DEFAULT_MAX_READ_BYTES: u64 = 1024 * 1024;

/// The contents of a file, and whether they are all of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileContents {
    pub text: String,
    /// True when the file was longer than the limit and this is only the beginning.
    ///
    /// Shown to the user, because an editor that silently opens the first megabyte of a
    /// file and then saves it would destroy the rest.
    pub truncated: bool,
    /// Total size on disk, which may exceed what was read.
    pub size_bytes: u64,
}

/// Why a file operation failed.
///
/// Distinct from [`TransportError`] because these are conditions on the far side that a
/// user can act on — a wrong path, a file they may not read — rather than a broken
/// connection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    #[error("no such file or directory: {0}")]
    NotFound(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("{0} is not a directory")]
    NotADirectory(String),
    #[error("{0} is not a regular file")]
    NotAFile(String),
    /// The file is not text, so showing it would be meaningless and editing it
    /// destructive.
    #[error("{0} is not a text file")]
    NotText(String),
    #[error("the file is too large to open: {size_bytes} bytes")]
    TooLarge { size_bytes: u64 },
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("could not interpret the server's response: {0}")]
    Malformed(String),
}

impl FileError {
    /// A stable code, so the interface can translate it.
    ///
    /// The same reasoning as [`crate::ports::TransportErrorKind`]: a formatted English
    /// sentence cannot be translated once it exists.
    pub fn kind(&self) -> &'static str {
        match self {
            FileError::NotFound(_) => "not_found",
            FileError::PermissionDenied(_) => "permission_denied",
            FileError::NotADirectory(_) => "not_a_directory",
            FileError::NotAFile(_) => "not_a_file",
            FileError::NotText(_) => "not_text",
            FileError::TooLarge { .. } => "too_large",
            FileError::Transport(_) => "transport",
            FileError::Malformed(_) => "malformed",
        }
    }
}

/// Reading and writing files on a server.
///
/// Every method takes the whole [`Server`] rather than its id, matching
/// [`ServerProbe`](crate::ports::ServerProbe): an implementation needs the host, port and
/// credential reference to connect, and one implementation serves the whole fleet rather
/// than holding per-server state of its own.
#[async_trait]
pub trait FileBrowser: Send + Sync {
    /// Lists one directory. Not recursive: a recursive listing of `/` is a denial of
    /// service against the machine being administered.
    async fn list(&self, server: &Server, path: &str) -> Result<Vec<DirectoryEntry>, FileError>;

    /// Reads a file as text, up to `max_bytes`.
    async fn read(
        &self,
        server: &Server,
        path: &str,
        max_bytes: u64,
    ) -> Result<FileContents, FileError>;

    /// Replaces a file's contents.
    ///
    /// Implementations write beside the target and move over it, so an interrupted
    /// write cannot leave a half-file where a working configuration used to be.
    async fn write(&self, server: &Server, path: &str, contents: &str) -> Result<(), FileError>;

    /// Deletes a file, or an empty directory.
    ///
    /// Recursive deletion is deliberately absent. `rm -rf` driven by a path from a text
    /// field is how an administration tool destroys a machine, and nothing here needs it.
    async fn delete(&self, server: &Server, path: &str) -> Result<(), FileError>;

    /// Creates a directory, including missing parents.
    async fn create_directory(&self, server: &Server, path: &str) -> Result<(), FileError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_file_is_recognised_but_not_special() {
        let entry = DirectoryEntry {
            name: ".env".into(),
            kind: EntryKind::File,
            size_bytes: 120,
            modified: None,
            mode: "rw-------".into(),
            owner: "www-data".into(),
            group: "www-data".into(),
            target: None,
        };
        assert!(entry.is_hidden());
        assert!(entry.kind.is_readable());
    }

    #[test]
    fn only_regular_files_are_offered_for_reading() {
        // A device file answers `read` forever; a directory answers with nonsense.
        assert!(EntryKind::File.is_readable());
        assert!(!EntryKind::Directory.is_readable());
        assert!(!EntryKind::Symlink.is_readable());
        assert!(!EntryKind::Other.is_readable());
    }

    #[test]
    fn every_failure_has_a_stable_code_to_translate() {
        // A formatted English sentence cannot be translated after the fact, which is the
        // lesson `TransportErrorKind` already taught.
        let cases = [
            FileError::NotFound("/tmp/x".into()),
            FileError::PermissionDenied("/root".into()),
            FileError::NotADirectory("/etc/passwd".into()),
            FileError::NotAFile("/etc".into()),
            FileError::NotText("/bin/ls".into()),
            FileError::TooLarge { size_bytes: 1 },
            FileError::Malformed("bad".into()),
        ];
        let mut codes: Vec<&str> = cases.iter().map(FileError::kind).collect();
        let count = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), count, "two failures share a code");
    }
}
