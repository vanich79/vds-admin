//! The acquisition seam.
//!
//! [`CommandRunner`] is the *only* thing collectors know about the outside world. SSH,
//! a local shell and a test script all implement it, which is what lets one set of
//! parsers serve agentless monitoring, the on-server agent and the test suite alike.
//! See `docs/adr/002-monitoring-architecture.md`.
//!
//! ## Why collectors are not async
//!
//! A [`Collector`] declares the [`Command`]s it needs and then *parses* their output.
//! It never performs I/O itself. Two things fall out of that split:
//!
//! * every command in a collection cycle can be gathered and sent in one round trip,
//!   because the whole set is known before anything is executed;
//! * every parser is a plain synchronous function of `&[CommandOutput]`, testable
//!   against captured fixtures with no runtime, no network and no mocking framework.

use crate::ids::{CollectorId, ServerId};
use crate::server::ServerSnapshot;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Marker printed between the two halves of a [`Command::SampleTwice`] result.
pub const SAMPLE_SEPARATOR: &str = "---vds-sample---";

/// Something a collector needs from the target host.
///
/// Modelling this as data rather than as a shell string lets each transport choose the
/// cheapest way to satisfy it: the agent reads `/proc` files directly instead of
/// spawning `cat`, while SSH turns everything into shell.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Command {
    /// Run a shell command.
    Shell(String),
    /// Read a file's contents.
    ReadFile(String),
    /// Read a file, wait, and read it again — for counters whose *rate* is the metric.
    ///
    /// The two halves are separated by [`SAMPLE_SEPARATOR`] in the output.
    SampleTwice { path: String, delay_ms: u64 },
}

impl Command {
    pub fn shell(command: impl Into<String>) -> Self {
        Command::Shell(command.into())
    }

    pub fn read(path: impl Into<String>) -> Self {
        Command::ReadFile(path.into())
    }

    pub fn sample_twice(path: impl Into<String>, delay_ms: u64) -> Self {
        Command::SampleTwice {
            path: path.into(),
            delay_ms,
        }
    }

    /// The POSIX shell form, used by transports that only have a shell.
    ///
    /// Paths are single-quoted so a path containing shell metacharacters cannot escape
    /// into the command. Collector paths are compile-time constants today, but the
    /// quoting keeps that from being a latent hazard if that ever changes.
    pub fn to_shell(&self) -> String {
        match self {
            Command::Shell(command) => command.clone(),
            Command::ReadFile(path) => format!("cat {}", quote(path)),
            Command::SampleTwice { path, delay_ms } => {
                let quoted = quote(path);
                // `sleep` takes fractional seconds on GNU coreutils and busybox alike.
                let seconds = *delay_ms as f64 / 1000.0;
                format!("cat {quoted}; echo '{SAMPLE_SEPARATOR}'; sleep {seconds:.3}; cat {quoted}")
            }
        }
    }

    /// Longest this command should be allowed to take, in milliseconds, excluding
    /// transport latency.
    pub fn min_budget_ms(&self) -> u64 {
        match self {
            Command::SampleTwice { delay_ms, .. } => delay_ms + 1_000,
            _ => 1_000,
        }
    }
}

/// Single-quotes a string for POSIX shell.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// The result of running one command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl CommandOutput {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            exit_code: 0,
        }
    }

    pub fn failure(exit_code: i32, stderr: impl Into<String>) -> Self {
        Self {
            stdout: String::new(),
            stderr: stderr.into(),
            exit_code,
        }
    }

    pub fn is_success(&self) -> bool {
        self.exit_code == 0
    }

    /// Trimmed stdout, which is what nearly every parser actually wants.
    pub fn trimmed(&self) -> &str {
        self.stdout.trim()
    }

    /// Splits the output of a [`Command::SampleTwice`] into its two halves.
    ///
    /// Returns `None` when the separator is missing, which means the second sample never
    /// happened — treating that as "no change" would report a bogus zero rate.
    pub fn split_samples(&self) -> Option<(&str, &str)> {
        let (first, second) = self.stdout.split_once(SAMPLE_SEPARATOR)?;
        Some((first.trim(), second.trim()))
    }
}

/// Optional abilities a transport may or may not have.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportCapabilities {
    /// Whether several commands are sent in one round trip.
    pub supports_batching: bool,
    /// Whether files can be read without spawning a process.
    pub supports_direct_file_read: bool,
    /// Whether commands can be run with elevated privileges.
    pub supports_privileged: bool,
}

/// Why a command could not be run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    #[error("connection failed: {0}")]
    Connection(String),
    #[error("authentication failed: {0}")]
    Authentication(String),
    #[error("host key rejected: {0}")]
    HostKeyRejected(String),
    #[error("operation timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("command execution failed: {0}")]
    Execution(String),
    #[error("transport is not connected")]
    NotConnected,
    #[error("credential unavailable: {0}")]
    MissingCredential(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Why a collection failed, as a value rather than a sentence.
///
/// `TransportError`'s `Display` is written for a log: "authentication failed: could not
/// read the private key" is accurate, English, and reaches the user's screen unchanged.
/// Translating the *sentence* is impossible once it has been formatted, so the kind is
/// carried alongside it and the presentation layer turns that into the user's language.
/// The original text is kept as the detail, because "which key, exactly" is what makes a
/// failure diagnosable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportErrorKind {
    Connection,
    Authentication,
    HostKeyRejected,
    Timeout,
    Execution,
    NotConnected,
    MissingCredential,
    Protocol,
}

impl TransportErrorKind {
    /// The stable form written to the database.
    pub fn as_str(self) -> &'static str {
        match self {
            TransportErrorKind::Connection => "connection",
            TransportErrorKind::Authentication => "authentication",
            TransportErrorKind::HostKeyRejected => "host_key_rejected",
            TransportErrorKind::Timeout => "timeout",
            TransportErrorKind::Execution => "execution",
            TransportErrorKind::NotConnected => "not_connected",
            TransportErrorKind::MissingCredential => "missing_credential",
            TransportErrorKind::Protocol => "protocol",
        }
    }

    /// Parses [`TransportErrorKind::as_str`].
    ///
    /// An unknown code — written by a newer version — yields `None` rather than a wrong
    /// kind, and the interface then falls back to showing the detail alone.
    pub fn parse(raw: &str) -> Option<TransportErrorKind> {
        match raw {
            "connection" => Some(TransportErrorKind::Connection),
            "authentication" => Some(TransportErrorKind::Authentication),
            "host_key_rejected" => Some(TransportErrorKind::HostKeyRejected),
            "timeout" => Some(TransportErrorKind::Timeout),
            "execution" => Some(TransportErrorKind::Execution),
            "not_connected" => Some(TransportErrorKind::NotConnected),
            "missing_credential" => Some(TransportErrorKind::MissingCredential),
            "protocol" => Some(TransportErrorKind::Protocol),
            _ => None,
        }
    }
}

impl TransportError {
    /// Which kind of failure this is, for translation and for reporting.
    pub fn kind(&self) -> TransportErrorKind {
        match self {
            TransportError::Connection(_) => TransportErrorKind::Connection,
            TransportError::Authentication(_) => TransportErrorKind::Authentication,
            TransportError::HostKeyRejected(_) => TransportErrorKind::HostKeyRejected,
            TransportError::Timeout { .. } => TransportErrorKind::Timeout,
            TransportError::Execution(_) => TransportErrorKind::Execution,
            TransportError::NotConnected => TransportErrorKind::NotConnected,
            TransportError::MissingCredential(_) => TransportErrorKind::MissingCredential,
            TransportError::Protocol(_) => TransportErrorKind::Protocol,
        }
    }

    /// Whether retrying could plausibly help.
    ///
    /// Authentication and host-key failures are configuration problems: retrying them on
    /// a schedule produces noise and, for password auth, can lock an account out.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            TransportError::Connection(_)
                | TransportError::Timeout { .. }
                | TransportError::Execution(_)
                | TransportError::NotConnected
                | TransportError::Protocol(_)
        )
    }
}

/// Runs commands somewhere.
///
/// Implementations: `SshCommandRunner` (agentless), `LocalCommandRunner` (the agent),
/// `ScriptedCommandRunner` (tests).
#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Runs a batch of commands.
    ///
    /// The returned vector has exactly one entry per input command, in the same order.
    /// An `Err` at the outer level means the transport itself failed and nothing ran;
    /// an `Err` at the inner level means that one command failed.
    ///
    /// Implementations must enforce their own timeout; a collector never blocks forever.
    async fn execute(
        &self,
        commands: &[Command],
    ) -> Result<Vec<Result<CommandOutput, TransportError>>, TransportError>;

    fn capabilities(&self) -> TransportCapabilities {
        TransportCapabilities::default()
    }
}

/// A feature a collector needs the target to have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// A `/proc` filesystem, i.e. Linux.
    ProcFs,
    Docker,
    Systemd,
    /// `df`, `ps` and friends.
    CoreUtils,
    /// Thermal sensors under `/sys/class/thermal`.
    ThermalSensors,
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Capability::ProcFs => "procfs",
            Capability::Docker => "docker",
            Capability::Systemd => "systemd",
            Capability::CoreUtils => "coreutils",
            Capability::ThermalSensors => "thermal",
        };
        f.write_str(name)
    }
}

/// Why a collector could not produce a result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CollectError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("could not parse output of {collector}: {reason}")]
    Parse {
        collector: CollectorId,
        reason: String,
    },
    /// The target lacks something this collector needs. Not an error condition for the
    /// server as a whole — a machine without Docker is perfectly healthy.
    #[error("{capability} is not available on this host")]
    Unsupported { capability: Capability },
}

impl CollectError {
    pub fn parse(collector: &CollectorId, reason: impl Into<String>) -> Self {
        CollectError::Parse {
            collector: collector.clone(),
            reason: reason.into(),
        }
    }

    /// Whether this failure should count against the server's health.
    ///
    /// A missing capability must not; otherwise every Docker-less server would look
    /// permanently degraded.
    pub fn affects_server_health(&self) -> bool {
        !matches!(self, CollectError::Unsupported { .. })
    }
}

/// What a collector contributes to a snapshot.
///
/// Each collector fills its own slice of [`ServerSnapshot`]; the application layer
/// merges them. Modelling the contribution rather than returning a whole snapshot keeps
/// collectors from overwriting each other's work.
#[derive(Debug, Clone, PartialEq)]
pub enum CollectorOutput {
    System(crate::server::SystemInfo),
    Cpu(crate::server::CpuUsage),
    Memory(crate::server::MemoryUsage),
    Filesystems(Vec<crate::server::FilesystemUsage>),
    Network(Vec<crate::server::NetworkInterface>),
    Load {
        load: crate::server::LoadAverage,
        uptime_secs: Option<u64>,
    },
    Processes(Vec<crate::server::ProcessInfo>),
    Containers(Vec<crate::server::ContainerInfo>),
    Services(Vec<crate::server::ServiceInfo>),
    Temperature(crate::metrics::MetricValue),
}

impl CollectorOutput {
    /// Merges this contribution into a snapshot.
    pub fn apply(self, snapshot: &mut ServerSnapshot) {
        match self {
            CollectorOutput::System(info) => snapshot.system = info,
            CollectorOutput::Cpu(cpu) => snapshot.cpu = cpu,
            CollectorOutput::Memory(memory) => snapshot.memory = memory,
            CollectorOutput::Filesystems(filesystems) => snapshot.filesystems = filesystems,
            CollectorOutput::Network(interfaces) => snapshot.interfaces = interfaces,
            CollectorOutput::Load { load, uptime_secs } => {
                snapshot.load = Some(load);
                if uptime_secs.is_some() {
                    snapshot.uptime_secs = uptime_secs;
                }
            }
            CollectorOutput::Processes(processes) => snapshot.processes = processes,
            CollectorOutput::Containers(containers) => snapshot.containers = Some(containers),
            CollectorOutput::Services(services) => snapshot.services = Some(services),
            CollectorOutput::Temperature(value) => snapshot.temperature_celsius = value,
        }
    }
}

/// Turns command output into a piece of a snapshot.
///
/// Implementations are pure: given the same outputs they produce the same result. That
/// is what makes them testable against captured fixtures.
pub trait Collector: Send + Sync {
    fn id(&self) -> CollectorId;

    /// What the target must provide for this collector to work.
    fn requires(&self) -> &'static [Capability];

    /// The commands this collector needs, in the order `parse` expects them.
    fn commands(&self) -> Vec<Command>;

    /// Interprets the results of [`Collector::commands`].
    ///
    /// `outputs` has the same length and order as `commands()`. Implementations must
    /// tolerate individual failures rather than assuming success.
    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError>;
}

/// Obtains a complete snapshot of a server.
///
/// This is the seam the application layer schedules against, so that agentless SSH, the
/// agent and a future cloud API look identical from above.
#[async_trait]
pub trait ServerProbe: Send + Sync {
    /// Collects everything currently obtainable from the server.
    async fn probe(
        &self,
        server: &crate::server::Server,
        at: DateTime<Utc>,
    ) -> Result<ServerSnapshot, TransportError>;

    /// Cheap reachability test, used for backoff decisions without a full collection.
    async fn ping(&self, server: &crate::server::Server) -> Result<(), TransportError>;

    /// Releases any pooled connection held for this server.
    async fn disconnect(&self, server_id: ServerId);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::MetricValue;
    use crate::server::{CpuUsage, LoadAverage, ServerSnapshot};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    #[test]
    fn command_output_reports_success_by_exit_code() {
        assert!(CommandOutput::success("ok").is_success());
        assert!(!CommandOutput::failure(1, "boom").is_success());
        assert_eq!(CommandOutput::success("  padded \n").trimmed(), "padded");
    }

    #[test]
    fn file_reads_become_cat_in_shell_form() {
        assert_eq!(
            Command::read("/proc/meminfo").to_shell(),
            "cat '/proc/meminfo'"
        );
    }

    #[test]
    fn shell_form_quotes_paths_so_metacharacters_cannot_escape() {
        let nasty = Command::read("/tmp/a; rm -rf /");
        assert_eq!(nasty.to_shell(), "cat '/tmp/a; rm -rf /'");

        // A single quote in the path must not terminate the quoting.
        let quoted = Command::read("/tmp/it's");
        assert_eq!(quoted.to_shell(), r"cat '/tmp/it'\''s'");
    }

    #[test]
    fn double_sampling_emits_both_halves_around_the_separator() {
        let shell = Command::sample_twice("/proc/stat", 500).to_shell();
        assert!(shell.contains(SAMPLE_SEPARATOR));
        assert!(shell.contains("sleep 0.500"));
        assert_eq!(shell.matches("cat '/proc/stat'").count(), 2);
    }

    #[test]
    fn sampled_output_splits_on_the_separator() {
        let output = CommandOutput::success(format!("first\n{SAMPLE_SEPARATOR}\nsecond"));
        assert_eq!(output.split_samples(), Some(("first", "second")));
    }

    #[test]
    fn a_truncated_sample_does_not_silently_become_a_zero_rate() {
        // If the second read never happened we must not treat the delta as zero.
        let output = CommandOutput::success("first only");
        assert_eq!(output.split_samples(), None);
    }

    #[test]
    fn double_sampling_asks_for_a_bigger_time_budget() {
        assert_eq!(
            Command::sample_twice("/proc/stat", 1_000).min_budget_ms(),
            2_000
        );
        assert_eq!(Command::read("/proc/meminfo").min_budget_ms(), 1_000);
    }

    #[test]
    fn configuration_failures_are_not_retried() {
        assert!(TransportError::Timeout { seconds: 5 }.is_retryable());
        assert!(TransportError::Connection("refused".into()).is_retryable());
        assert!(!TransportError::Authentication("bad password".into()).is_retryable());
        assert!(!TransportError::HostKeyRejected("changed".into()).is_retryable());
    }

    #[test]
    fn a_missing_capability_does_not_make_a_server_unhealthy() {
        let missing = CollectError::Unsupported {
            capability: Capability::Docker,
        };
        assert!(!missing.affects_server_health());

        let real = CollectError::Transport(TransportError::NotConnected);
        assert!(real.affects_server_health());
    }

    #[test]
    fn collector_outputs_merge_into_distinct_snapshot_slices() {
        let mut snapshot = ServerSnapshot::new(ServerId::new(), at(0));

        CollectorOutput::Cpu(CpuUsage {
            total_percent: MetricValue::Available(42.0),
            ..Default::default()
        })
        .apply(&mut snapshot);
        CollectorOutput::Load {
            load: LoadAverage {
                one: 1.0,
                five: 2.0,
                fifteen: 3.0,
            },
            uptime_secs: Some(1_000),
        }
        .apply(&mut snapshot);

        assert_eq!(snapshot.cpu.total_percent, MetricValue::Available(42.0));
        assert_eq!(snapshot.load.map(|l| l.five), Some(2.0));
        assert_eq!(snapshot.uptime_secs, Some(1_000));
    }

    #[test]
    fn docker_absence_is_distinguishable_from_an_empty_docker() {
        let mut snapshot = ServerSnapshot::new(ServerId::new(), at(0));
        assert_eq!(snapshot.containers, None);

        CollectorOutput::Containers(Vec::new()).apply(&mut snapshot);
        assert_eq!(snapshot.containers, Some(Vec::new()));
    }
}
