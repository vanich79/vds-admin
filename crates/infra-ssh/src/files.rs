//! The file browser, over SSH.
//!
//! Thin by design. Every decision that could be got wrong — how a path is quoted, how a
//! listing is read, what counts as text — lives in
//! [`vds_infra_collectors::files`](vds_infra_collectors::files), where it is a pure
//! function with tests. What remains here is running one command on a pooled session,
//! which is the part that cannot be tested without a server.
//!
//! It is implemented on [`SshServerProbe`] rather than beside it so that browsing reuses
//! the same pooled connection as monitoring. Opening a second SSH session per server to
//! list a directory would double the handshakes, and on a host running fail2ban it is a
//! good way to get the application's own address banned.

use crate::probe::SshServerProbe;
use crate::session::SshCommandRunner;
use async_trait::async_trait;
use std::sync::Arc;
use vds_domain::ports::{
    Command, CommandOutput, CommandRunner, DirectoryEntry, FileBrowser, FileContents, FileError,
    TransportError,
};
use vds_domain::server::Server;
use vds_infra_collectors::files;

impl SshServerProbe {
    /// Runs one command and returns its output.
    ///
    /// A failure to reach the machine is a [`TransportError`], which [`FileError`] carries
    /// through unchanged: "the connection dropped" and "you may not read that file" are
    /// different problems and the interface says so differently.
    async fn run_one(&self, server: &Server, command: Command) -> Result<CommandOutput, FileError> {
        let session = self.session_for(server).await?;
        let runner = SshCommandRunner::new(Arc::clone(&session));

        let mut results = runner.execute(&[command]).await?;
        // `execute` returns one result per command; a shorter vector would mean the
        // batching layer lost one, which is a bug rather than a server condition.
        results
            .pop()
            .unwrap_or_else(|| {
                Err(TransportError::Protocol(
                    "the server returned no output for the command".to_owned(),
                ))
            })
            .map_err(FileError::from)
    }
}

#[async_trait]
impl FileBrowser for SshServerProbe {
    async fn list(&self, server: &Server, path: &str) -> Result<Vec<DirectoryEntry>, FileError> {
        let output = self.run_one(server, files::list_command(path)).await?;
        files::parse_listing(&output, path)
    }

    async fn read(
        &self,
        server: &Server,
        path: &str,
        max_bytes: u64,
    ) -> Result<FileContents, FileError> {
        let output = self
            .run_one(server, files::read_command(path, max_bytes))
            .await?;
        files::parse_read(&output, path, max_bytes)
    }

    async fn write(&self, server: &Server, path: &str, contents: &str) -> Result<(), FileError> {
        let output = self
            .run_one(server, files::write_command(path, contents))
            .await?;
        files::parse_action(&output, path)
    }

    async fn delete(&self, server: &Server, path: &str) -> Result<(), FileError> {
        let output = self.run_one(server, files::delete_command(path)).await?;
        files::parse_action(&output, path)
    }

    async fn create_directory(&self, server: &Server, path: &str) -> Result<(), FileError> {
        let output = self
            .run_one(server, files::create_directory_command(path))
            .await?;
        files::parse_action(&output, path)
    }
}
