//! Transports that are not SSH: the local shell (used by the agent) and a scripted one
//! (used by tests).

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vds_domain::ports::{
    Command, CommandOutput, CommandRunner, TransportCapabilities, TransportError,
};

/// Runs commands on the machine the process is running on.
///
/// Used by `vds-agent`. [`Command::ReadFile`] and [`Command::SampleTwice`] are served by
/// reading the file directly rather than spawning `cat`, which is what keeps the agent's
/// footprint to a couple of syscalls per metric instead of a process per metric — the
/// difference between a negligible daemon and one that shows up in `top`.
#[derive(Debug, Clone)]
pub struct LocalCommandRunner {
    /// Per-command timeout.
    timeout: Duration,
    /// Shell used for [`Command::Shell`].
    shell: String,
}

impl LocalCommandRunner {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            shell: "/bin/sh".to_owned(),
        }
    }

    /// Overrides the shell, mainly for hosts where `/bin/sh` is somewhere unusual.
    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }

    async fn run_one(&self, command: &Command) -> Result<CommandOutput, TransportError> {
        match command {
            Command::ReadFile(path) => self.read_file(path).await,
            Command::SampleTwice { path, delay_ms } => {
                let first = self.read_file(path).await?;
                tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
                let second = self.read_file(path).await?;
                Ok(CommandOutput::success(format!(
                    "{}\n{}\n{}",
                    first.stdout.trim_end(),
                    vds_domain::ports::SAMPLE_SEPARATOR,
                    second.stdout.trim_end()
                )))
            }
            Command::Shell(script) => self.run_shell(script).await,
        }
    }

    async fn read_file(&self, path: &str) -> Result<CommandOutput, TransportError> {
        match tokio::time::timeout(self.timeout, tokio::fs::read_to_string(path)).await {
            Ok(Ok(contents)) => Ok(CommandOutput::success(contents)),
            // A missing /proc file is a normal "not supported here" signal, so it is
            // reported as a non-zero exit rather than as a transport failure.
            Ok(Err(err)) => Ok(CommandOutput::failure(1, format!("{path}: {err}"))),
            Err(_) => Err(TransportError::Timeout {
                seconds: self.timeout.as_secs(),
            }),
        }
    }

    async fn run_shell(&self, script: &str) -> Result<CommandOutput, TransportError> {
        let mut command = tokio::process::Command::new(&self.shell);
        command.arg("-c").arg(script);
        command.kill_on_drop(true);

        let output = tokio::time::timeout(self.timeout, command.output())
            .await
            .map_err(|_| TransportError::Timeout {
                seconds: self.timeout.as_secs(),
            })?
            .map_err(|err| TransportError::Execution(err.to_string()))?;

        Ok(CommandOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }
}

impl Default for LocalCommandRunner {
    fn default() -> Self {
        Self::new(Duration::from_secs(10))
    }
}

#[async_trait]
impl CommandRunner for LocalCommandRunner {
    async fn execute(
        &self,
        commands: &[Command],
    ) -> Result<Vec<Result<CommandOutput, TransportError>>, TransportError> {
        let mut results = Vec::with_capacity(commands.len());
        for command in commands {
            results.push(self.run_one(command).await);
        }
        Ok(results)
    }

    fn capabilities(&self) -> TransportCapabilities {
        TransportCapabilities {
            supports_batching: false,
            supports_direct_file_read: true,
            supports_privileged: false,
        }
    }
}

/// A transport that replays canned responses.
///
/// This is the test infrastructure the brief asks for: it emulates an online server, an
/// offline one, a slow one, a host without Docker, a broken `systemctl` — without a
/// network, a container, or a real machine of any kind.
#[derive(Debug, Clone, Default)]
pub struct ScriptedCommandRunner {
    responses: Arc<Mutex<HashMap<Command, Result<CommandOutput, TransportError>>>>,
    /// Returned by `execute` when the whole transport is meant to be down.
    transport_failure: Option<TransportError>,
    /// Response for commands with no scripted entry.
    fallback: Arc<Mutex<Option<Result<CommandOutput, TransportError>>>>,
    /// Every command the runner was asked for, in order.
    calls: Arc<Mutex<Vec<Command>>>,
    /// Artificial delay, for exercising timeouts.
    delay: Option<Duration>,
}

impl ScriptedCommandRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scripts a successful response for a command.
    pub fn on(mut self, command: Command, stdout: impl Into<String>) -> Self {
        self.insert(command, Ok(CommandOutput::success(stdout)));
        self
    }

    /// Scripts a non-zero exit for a command.
    pub fn on_failure(
        mut self,
        command: Command,
        exit_code: i32,
        stderr: impl Into<String>,
    ) -> Self {
        self.insert(command, Ok(CommandOutput::failure(exit_code, stderr)));
        self
    }

    /// Scripts a transport-level error for a single command.
    pub fn on_error(mut self, command: Command, error: TransportError) -> Self {
        self.insert(command, Err(error));
        self
    }

    /// Makes every unscripted command return this.
    pub fn fallback(self, response: Result<CommandOutput, TransportError>) -> Self {
        if let Ok(mut guard) = self.fallback.lock() {
            *guard = Some(response);
        }
        self
    }

    /// Makes the whole transport fail, as if the host were unreachable.
    pub fn offline(mut self, error: TransportError) -> Self {
        self.transport_failure = Some(error);
        self
    }

    /// Delays every batch, for exercising timeout handling.
    pub fn slow(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    /// Commands the runner has been asked for, in order.
    pub fn calls(&self) -> Vec<Command> {
        self.calls
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn insert(&mut self, command: Command, response: Result<CommandOutput, TransportError>) {
        if let Ok(mut guard) = self.responses.lock() {
            guard.insert(command, response);
        }
    }
}

#[async_trait]
impl CommandRunner for ScriptedCommandRunner {
    async fn execute(
        &self,
        commands: &[Command],
    ) -> Result<Vec<Result<CommandOutput, TransportError>>, TransportError> {
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        if let Some(error) = &self.transport_failure {
            return Err(error.clone());
        }

        if let Ok(mut guard) = self.calls.lock() {
            guard.extend_from_slice(commands);
        }

        let scripted = self.responses.lock().ok();
        let fallback = self.fallback.lock().ok().and_then(|g| g.clone());

        Ok(commands
            .iter()
            .map(
                |command| match scripted.as_ref().and_then(|s| s.get(command)) {
                    Some(response) => response.clone(),
                    None => fallback.clone().unwrap_or_else(|| {
                        Ok(CommandOutput::failure(
                            127,
                            format!("unscripted command: {command:?}"),
                        ))
                    }),
                },
            )
            .collect())
    }

    fn capabilities(&self) -> TransportCapabilities {
        TransportCapabilities {
            supports_batching: true,
            supports_direct_file_read: true,
            supports_privileged: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_scripted_runner_replays_what_it_was_given() {
        let runner = ScriptedCommandRunner::new()
            .on(Command::read("/proc/meminfo"), "MemTotal: 1 kB")
            .on_failure(Command::shell("docker ps"), 127, "docker: not found");

        let results = runner
            .execute(&[Command::read("/proc/meminfo"), Command::shell("docker ps")])
            .await
            .expect("transport is up");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].as_ref().expect("ok").stdout, "MemTotal: 1 kB");
        assert_eq!(results[1].as_ref().expect("ok").exit_code, 127);
    }

    #[tokio::test]
    async fn an_offline_host_fails_the_whole_batch() {
        let runner =
            ScriptedCommandRunner::new().offline(TransportError::Connection("refused".into()));
        let err = runner
            .execute(&[Command::read("/proc/stat")])
            .await
            .expect_err("must fail");
        assert_eq!(err, TransportError::Connection("refused".into()));
    }

    #[tokio::test]
    async fn unscripted_commands_fail_loudly_rather_than_silently_succeeding() {
        // A test that forgets to script a command should notice.
        let runner = ScriptedCommandRunner::new();
        let results = runner
            .execute(&[Command::shell("whoami")])
            .await
            .expect("transport up");
        assert!(!results[0].as_ref().expect("ok").is_success());
    }

    #[tokio::test]
    async fn the_fallback_covers_commands_a_test_does_not_care_about() {
        let runner = ScriptedCommandRunner::new()
            .fallback(Ok(CommandOutput::success("whatever")))
            .on(Command::read("/proc/stat"), "cpu 1 2 3 4");

        let results = runner
            .execute(&[Command::read("/proc/stat"), Command::shell("anything")])
            .await
            .expect("transport up");
        assert_eq!(results[0].as_ref().expect("ok").stdout, "cpu 1 2 3 4");
        assert_eq!(results[1].as_ref().expect("ok").stdout, "whatever");
    }

    #[tokio::test]
    async fn calls_are_recorded_in_order() {
        let runner = ScriptedCommandRunner::new().fallback(Ok(CommandOutput::success("")));
        runner
            .execute(&[Command::read("/a"), Command::read("/b")])
            .await
            .expect("transport up");
        assert_eq!(
            runner.calls(),
            vec![Command::read("/a"), Command::read("/b")]
        );
    }

    #[tokio::test]
    async fn the_local_runner_reads_files_without_spawning_a_shell() {
        let dir = std::env::temp_dir().join(format!("vds-runner-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.expect("temp dir");
        let path = dir.join("sample");
        tokio::fs::write(&path, "hello from the file")
            .await
            .expect("write");

        let runner = LocalCommandRunner::new(Duration::from_secs(5));
        let results = runner
            .execute(&[Command::read(path.to_string_lossy().into_owned())])
            .await
            .expect("transport up");

        let output = results[0].as_ref().expect("read succeeded");
        assert!(output.is_success());
        assert_eq!(output.stdout, "hello from the file");
        assert!(runner.capabilities().supports_direct_file_read);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn a_missing_file_is_a_non_zero_exit_not_a_transport_failure() {
        // "/proc/this-does-not-exist" means the host lacks a feature, not that the
        // connection broke. Conflating them would mark healthy servers offline.
        let runner = LocalCommandRunner::new(Duration::from_secs(5));
        let results = runner
            .execute(&[Command::read("/definitely/not/a/real/path")])
            .await
            .expect("transport up");
        let output = results[0].as_ref().expect("no transport error");
        assert!(!output.is_success());
    }

    #[tokio::test]
    async fn double_sampling_produces_both_halves_around_the_separator() {
        let dir = std::env::temp_dir().join(format!("vds-sample-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.expect("temp dir");
        let path = dir.join("counter");
        tokio::fs::write(&path, "42").await.expect("write");

        let runner = LocalCommandRunner::new(Duration::from_secs(5));
        let results = runner
            .execute(&[Command::sample_twice(
                path.to_string_lossy().into_owned(),
                1,
            )])
            .await
            .expect("transport up");

        let output = results[0].as_ref().expect("read succeeded");
        let (first, second) = output.split_samples().expect("separator present");
        assert_eq!(first, "42");
        assert_eq!(second, "42");

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
