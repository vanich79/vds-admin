//! The server aggregate: configuration, the snapshot a collection cycle produces, and
//! the derived runtime state.

use crate::ids::{CredentialRef, ServerId};
use crate::metrics::{CollectorOutcome, MetricResult, MetricValue};
use crate::status::{Status, Threshold};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// How metrics are obtained from a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionMode {
    /// Mode A — agentless, commands executed over SSH.
    Ssh,
    /// Mode B — the `vds-agent` daemon pushes/serves metrics over HTTPS.
    Agent,
}

impl ConnectionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionMode::Ssh => "ssh",
            ConnectionMode::Agent => "agent",
        }
    }

    pub fn parse(raw: &str) -> Option<ConnectionMode> {
        match raw {
            "ssh" => Some(ConnectionMode::Ssh),
            "agent" => Some(ConnectionMode::Agent),
            _ => None,
        }
    }
}

/// Which SSH authentication method a server's stored credential holds.
///
/// The *material* — password, key bytes, passphrase — is never in the domain. Only the
/// shape is, so the UI can render the right form and the SSH layer can ask the
/// [`crate::ports::SecretStore`] for the right thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthKind {
    Password,
    PrivateKey,
    /// Encrypted private key; the passphrase is stored alongside it in the secret store.
    EncryptedPrivateKey,
}

impl SshAuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SshAuthKind::Password => "password",
            SshAuthKind::PrivateKey => "private_key",
            SshAuthKind::EncryptedPrivateKey => "encrypted_private_key",
        }
    }

    pub fn parse(raw: &str) -> Option<SshAuthKind> {
        match raw {
            "password" => Some(SshAuthKind::Password),
            "private_key" => Some(SshAuthKind::PrivateKey),
            "encrypted_private_key" => Some(SshAuthKind::EncryptedPrivateKey),
            _ => None,
        }
    }
}

/// Per-server connection settings for SSH mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshSettings {
    pub username: String,
    pub auth_kind: SshAuthKind,
    /// Handle into the secret store. Never the secret itself.
    pub credential_ref: CredentialRef,
}

/// Per-server connection settings for agent mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettings {
    /// Port the agent listens on.
    pub port: u16,
    /// Handle to the bearer token used to authenticate against the agent.
    pub credential_ref: CredentialRef,
    /// SHA-256 fingerprint of the agent's TLS certificate, pinned on first connection.
    pub certificate_fingerprint: Option<String>,
}

/// Mode-specific connection configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ConnectionSettings {
    Ssh(SshSettings),
    Agent(AgentSettings),
}

impl ConnectionSettings {
    pub fn mode(&self) -> ConnectionMode {
        match self {
            ConnectionSettings::Ssh(_) => ConnectionMode::Ssh,
            ConnectionSettings::Agent(_) => ConnectionMode::Agent,
        }
    }

    /// The secret this server needs in order to connect.
    pub fn credential_ref(&self) -> CredentialRef {
        match self {
            ConnectionSettings::Ssh(s) => s.credential_ref,
            ConnectionSettings::Agent(a) => a.credential_ref,
        }
    }
}

/// Thresholds that turn raw measurements into a [`Status`].
///
/// These live in configuration, not in collectors, so that operators can tune them
/// without a rebuild and so that no collector encodes a policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MonitoringThresholds {
    pub cpu: Threshold,
    pub memory: Threshold,
    pub disk: Threshold,
    pub swap: Threshold,
    /// Load average per core.
    pub load_per_core: Threshold,
    pub temperature: Threshold,
}

impl Default for MonitoringThresholds {
    fn default() -> Self {
        Self {
            cpu: Threshold::above(80.0, 95.0),
            memory: Threshold::above(85.0, 95.0),
            disk: Threshold::above(85.0, 90.0),
            swap: Threshold::above(50.0, 80.0),
            load_per_core: Threshold::above(1.0, 2.0),
            temperature: Threshold::above(70.0, 85.0),
        }
    }
}

/// A monitored server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Server {
    pub id: ServerId,
    pub name: String,
    /// Hostname or IP address.
    pub host: String,
    /// SSH port. Agent mode carries its own port in [`AgentSettings`].
    pub port: u16,
    pub connection: ConnectionSettings,
    pub enabled: bool,
    /// How often to collect, in seconds.
    pub poll_interval_secs: u32,
    /// Consecutive failures before the server is declared [`Status::Offline`].
    pub offline_after_failures: u32,
    /// Per-collection timeout, in seconds.
    pub timeout_secs: u32,
    pub thresholds: MonitoringThresholds,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

/// Fixed defaults referenced by both the domain and the configuration layer.
pub const DEFAULT_POLL_INTERVAL_SECS: u32 = 30;
pub const DEFAULT_OFFLINE_AFTER_FAILURES: u32 = 3;
pub const DEFAULT_TIMEOUT_SECS: u32 = 20;
pub const DEFAULT_SSH_PORT: u16 = 22;
/// Default port an agent listens on.
///
/// Duplicated from `vds-agent-protocol` rather than imported: the domain depends on
/// nothing, and that rule is worth more than one shared integer. `vds-agent` has a test
/// asserting the two agree, so they cannot drift apart silently.
pub const DEFAULT_AGENT_PORT: u16 = 9443;

impl Server {
    /// Builds a server with the documented defaults applied.
    pub fn new(
        name: impl Into<String>,
        host: impl Into<String>,
        connection: ConnectionSettings,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: ServerId::new(),
            name: name.into(),
            host: host.into(),
            port: DEFAULT_SSH_PORT,
            connection,
            enabled: true,
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            offline_after_failures: DEFAULT_OFFLINE_AFTER_FAILURES,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            thresholds: MonitoringThresholds::default(),
            tags: Vec::new(),
            created_at: now,
        }
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::seconds(i64::from(self.poll_interval_secs.max(1)))
    }

    pub fn timeout(&self) -> Duration {
        Duration::seconds(i64::from(self.timeout_secs.max(1)))
    }

    /// Validates the invariants the UI and the importers must respect.
    pub fn validate(&self) -> Result<(), ServerValidationError> {
        if self.name.trim().is_empty() {
            return Err(ServerValidationError::EmptyName);
        }
        if self.host.trim().is_empty() {
            return Err(ServerValidationError::EmptyHost);
        }
        if self.port == 0 {
            return Err(ServerValidationError::InvalidPort(self.port));
        }
        if self.poll_interval_secs == 0 {
            return Err(ServerValidationError::InvalidPollInterval);
        }
        if self.offline_after_failures == 0 {
            return Err(ServerValidationError::InvalidFailureThreshold);
        }
        if self.timeout_secs == 0 {
            return Err(ServerValidationError::InvalidTimeout);
        }
        if u64::from(self.timeout_secs) > u64::from(self.poll_interval_secs) * 4 {
            return Err(ServerValidationError::TimeoutExceedsInterval);
        }
        for threshold in [
            self.thresholds.cpu,
            self.thresholds.memory,
            self.thresholds.disk,
            self.thresholds.swap,
            self.thresholds.load_per_core,
            self.thresholds.temperature,
        ] {
            if !threshold.is_coherent() {
                return Err(ServerValidationError::IncoherentThreshold);
            }
        }
        Ok(())
    }
}

/// Why a server configuration was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerValidationError {
    #[error("server name must not be empty")]
    EmptyName,
    #[error("server host must not be empty")]
    EmptyHost,
    #[error("port {0} is not valid")]
    InvalidPort(u16),
    #[error("poll interval must be at least 1 second")]
    InvalidPollInterval,
    #[error("offline threshold must be at least 1 failed check")]
    InvalidFailureThreshold,
    #[error("timeout must be at least 1 second")]
    InvalidTimeout,
    #[error("timeout must not exceed four polling intervals, or checks will pile up")]
    TimeoutExceedsInterval,
    #[error("threshold values are inverted relative to their direction")]
    IncoherentThreshold,
}

/// Static facts about a machine, refreshed rarely.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub hostname: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub kernel: Option<String>,
    pub architecture: Option<String>,
    pub cpu_model: Option<String>,
    pub cpu_cores: Option<u32>,
}

/// CPU utilisation for one collection cycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuUsage {
    /// Total busy percentage, 0–100.
    pub total_percent: MetricValue,
    pub user_percent: MetricValue,
    pub system_percent: MetricValue,
    pub iowait_percent: MetricValue,
    pub cores: Option<u32>,
}

/// Memory and swap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryUsage {
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
    pub available_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
    pub swap_used_bytes: Option<u64>,
}

impl MemoryUsage {
    /// Used memory as a percentage of total, excluding cache/buffers where the source
    /// reports `MemAvailable`.
    pub fn used_percent(&self) -> MetricValue {
        percentage(self.used_bytes, self.total_bytes)
    }

    pub fn swap_used_percent(&self) -> MetricValue {
        percentage(self.swap_used_bytes, self.swap_total_bytes)
    }
}

/// One mounted filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemUsage {
    pub mount_point: String,
    pub device: Option<String>,
    pub filesystem: Option<String>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

impl FilesystemUsage {
    pub fn used_percent(&self) -> MetricValue {
        percentage(Some(self.used_bytes), Some(self.total_bytes))
    }
}

/// Cumulative counters for one network interface.
///
/// Rates are derived by the application layer from two consecutive snapshots; the
/// collector only reports what the kernel reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInterface {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
}

/// A process, as reported by `ps`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub user: Option<String>,
    pub command: String,
    pub cpu_percent: f64,
    pub memory_percent: f64,
    pub rss_bytes: Option<u64>,
}

/// Lifecycle state of a Docker container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
    Running,
    Paused,
    Restarting,
    Exited,
    Created,
    Dead,
    Removing,
    Unknown,
}

impl ContainerState {
    pub fn parse(raw: &str) -> ContainerState {
        match raw.trim().to_ascii_lowercase().as_str() {
            "running" | "up" => ContainerState::Running,
            "paused" => ContainerState::Paused,
            "restarting" => ContainerState::Restarting,
            "exited" => ContainerState::Exited,
            "created" => ContainerState::Created,
            "dead" => ContainerState::Dead,
            "removing" => ContainerState::Removing,
            _ => ContainerState::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ContainerState::Running => "running",
            ContainerState::Paused => "paused",
            ContainerState::Restarting => "restarting",
            ContainerState::Exited => "exited",
            ContainerState::Created => "created",
            ContainerState::Dead => "dead",
            ContainerState::Removing => "removing",
            ContainerState::Unknown => "unknown",
        }
    }
}

/// Docker's own health-check verdict, which is independent of the lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerHealth {
    Healthy,
    Unhealthy,
    Starting,
    /// The image declares no `HEALTHCHECK`.
    None,
}

impl ContainerHealth {
    /// The stable string form, used on the agent wire protocol.
    pub fn as_str(self) -> &'static str {
        match self {
            ContainerHealth::Healthy => "healthy",
            ContainerHealth::Unhealthy => "unhealthy",
            ContainerHealth::Starting => "starting",
            ContainerHealth::None => "none",
        }
    }

    /// Parses [`ContainerHealth::as_str`].
    ///
    /// Anything unrecognised becomes [`ContainerHealth::None`] — "no health check" — so
    /// a newer agent's vocabulary can never make a container look unhealthy by accident.
    pub fn parse(raw: &str) -> ContainerHealth {
        match raw.trim().to_ascii_lowercase().as_str() {
            "healthy" => ContainerHealth::Healthy,
            "unhealthy" => ContainerHealth::Unhealthy,
            "starting" => ContainerHealth::Starting,
            _ => ContainerHealth::None,
        }
    }
}

/// A Docker container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: ContainerState,
    pub health: ContainerHealth,
    /// Docker's human-readable status string, e.g. `"Up 3 days (healthy)"`.
    pub status_text: String,
    pub cpu_percent: MetricValue,
    pub memory_used_bytes: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
    pub restart_count: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
}

impl ContainerInfo {
    /// Status of this container alone.
    ///
    /// A container that exited cleanly is a warning rather than critical: many
    /// containers are one-shot jobs, and the operator decides which matter.
    pub fn status(&self) -> Status {
        match (self.state, self.health) {
            (ContainerState::Running, ContainerHealth::Unhealthy) => Status::Critical,
            (ContainerState::Running, _) => Status::Healthy,
            (ContainerState::Restarting, _) | (ContainerState::Dead, _) => Status::Critical,
            (ContainerState::Paused, _)
            | (ContainerState::Exited, _)
            | (ContainerState::Created, _)
            | (ContainerState::Removing, _) => Status::Warning,
            (ContainerState::Unknown, _) => Status::Unknown,
        }
    }
}

/// State of a systemd unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Active,
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Unknown,
}

impl ServiceState {
    pub fn parse(raw: &str) -> ServiceState {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => ServiceState::Active,
            "inactive" => ServiceState::Inactive,
            "failed" => ServiceState::Failed,
            "activating" => ServiceState::Activating,
            "deactivating" => ServiceState::Deactivating,
            _ => ServiceState::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ServiceState::Active => "active",
            ServiceState::Inactive => "inactive",
            ServiceState::Failed => "failed",
            ServiceState::Activating => "activating",
            ServiceState::Deactivating => "deactivating",
            ServiceState::Unknown => "unknown",
        }
    }

    pub fn status(self) -> Status {
        match self {
            ServiceState::Active => Status::Healthy,
            ServiceState::Failed => Status::Critical,
            ServiceState::Inactive => Status::Warning,
            ServiceState::Activating | ServiceState::Deactivating => Status::Warning,
            ServiceState::Unknown => Status::Unknown,
        }
    }
}

/// A systemd unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub state: ServiceState,
    /// systemd's `SUB` column, e.g. `"running"`, `"exited"`.
    pub sub_state: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

/// Load average triple.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

impl LoadAverage {
    /// One-minute load normalised by core count, which is the only comparable form
    /// across machines of different sizes.
    pub fn per_core(&self, cores: Option<u32>) -> MetricValue {
        match cores {
            Some(c) if c > 0 => MetricValue::available(self.one / f64::from(c)),
            _ => MetricValue::NotAvailable,
        }
    }
}

/// Everything one collection cycle produced for one server.
///
/// Fields are independently optional: a server without Docker still yields CPU and
/// memory, and one collector failing never voids the cycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerSnapshot {
    pub server_id: ServerId,
    pub collected_at: DateTime<Utc>,
    pub system: SystemInfo,
    pub cpu: CpuUsage,
    pub memory: MemoryUsage,
    pub filesystems: Vec<FilesystemUsage>,
    pub interfaces: Vec<NetworkInterface>,
    pub load: Option<LoadAverage>,
    pub uptime_secs: Option<u64>,
    pub temperature_celsius: MetricValue,
    pub processes: Vec<ProcessInfo>,
    pub containers: Option<Vec<ContainerInfo>>,
    pub services: Option<Vec<ServiceInfo>>,
    /// Per-collector success/failure, so partial results are explainable.
    pub outcomes: Vec<CollectorOutcome>,
}

impl ServerSnapshot {
    pub fn new(server_id: ServerId, collected_at: DateTime<Utc>) -> Self {
        Self {
            server_id,
            collected_at,
            system: SystemInfo::default(),
            cpu: CpuUsage::default(),
            memory: MemoryUsage::default(),
            filesystems: Vec::new(),
            interfaces: Vec::new(),
            load: None,
            uptime_secs: None,
            temperature_celsius: MetricValue::NotAvailable,
            processes: Vec::new(),
            containers: None,
            services: None,
            outcomes: Vec::new(),
        }
    }

    /// Usage of the fullest filesystem, which is the number worth alerting on.
    pub fn worst_filesystem_percent(&self) -> MetricValue {
        self.filesystems
            .iter()
            .filter_map(|fs| fs.used_percent().value())
            .fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| if v > a { v } else { a }))
            })
            .map_or(MetricValue::NotAvailable, MetricValue::available)
    }

    /// Total bytes across all filesystems.
    pub fn total_disk_bytes(&self) -> u64 {
        self.filesystems.iter().map(|fs| fs.total_bytes).sum()
    }

    pub fn used_disk_bytes(&self) -> u64 {
        self.filesystems.iter().map(|fs| fs.used_bytes).sum()
    }

    /// Containers that need attention.
    pub fn unhealthy_containers(&self) -> Vec<&ContainerInfo> {
        self.containers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|c| c.status().is_problem())
            .collect()
    }

    /// Units in the `failed` state.
    pub fn failed_services(&self) -> Vec<&ServiceInfo> {
        self.services
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|s| s.state == ServiceState::Failed)
            .collect()
    }
}

/// The derived, persisted state of a server between collection cycles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerRuntimeState {
    pub server_id: ServerId,
    pub status: Status,
    pub last_check: Option<DateTime<Utc>>,
    pub last_success: Option<DateTime<Utc>>,
    /// Consecutive failed checks; reset to zero on any success.
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    /// The kind of the last failure, so the interface can say what happened in the
    /// user's language. `last_error` keeps the original technical detail.
    pub last_error_kind: Option<crate::ports::TransportErrorKind>,
    pub uptime_secs: Option<u64>,
    pub cpu_percent: MetricValue,
    pub memory_percent: MetricValue,
    pub disk_percent: MetricValue,
}

impl ServerRuntimeState {
    pub fn unknown(server_id: ServerId) -> Self {
        Self {
            server_id,
            status: Status::Unknown,
            last_check: None,
            last_success: None,
            consecutive_failures: 0,
            last_error: None,
            last_error_kind: None,
            uptime_secs: None,
            cpu_percent: MetricValue::NotAvailable,
            memory_percent: MetricValue::NotAvailable,
            disk_percent: MetricValue::NotAvailable,
        }
    }
}

/// Evaluates a snapshot against a server's thresholds.
///
/// Returns one [`MetricResult`] per measurable quantity. The overall server status is
/// the worst of them, computed by the caller via [`Status::worst_of`], which keeps this
/// function free of aggregation policy.
pub fn evaluate_snapshot(
    snapshot: &ServerSnapshot,
    thresholds: &MonitoringThresholds,
) -> Vec<MetricResult> {
    use crate::metrics::MetricKind;

    let at = snapshot.collected_at;
    let mut results = Vec::with_capacity(8);

    // A plain fn rather than a closure: the load-average branch below also needs to
    // push directly, and a closure capturing `results` would lock it for the whole scope.
    fn push(
        results: &mut Vec<MetricResult>,
        kind: MetricKind,
        value: MetricValue,
        threshold: Option<Threshold>,
        at: DateTime<Utc>,
    ) {
        let status = match (value.value(), threshold) {
            (Some(v), Some(t)) => t.classify(v),
            (Some(_), None) => Status::Healthy,
            (None, _) => Status::Unknown,
        };
        results.push(MetricResult::new(kind, value, status, at));
    }

    let r = &mut results;
    push(
        r,
        MetricKind::CpuUsage,
        snapshot.cpu.total_percent,
        Some(thresholds.cpu),
        at,
    );
    push(
        r,
        MetricKind::MemoryUsage,
        snapshot.memory.used_percent(),
        Some(thresholds.memory),
        at,
    );
    push(
        r,
        MetricKind::SwapUsage,
        snapshot.memory.swap_used_percent(),
        Some(thresholds.swap),
        at,
    );
    push(
        r,
        MetricKind::DiskUsage,
        snapshot.worst_filesystem_percent(),
        Some(thresholds.disk),
        at,
    );
    push(
        r,
        MetricKind::TemperatureCelsius,
        snapshot.temperature_celsius,
        Some(thresholds.temperature),
        at,
    );

    if let Some(load) = snapshot.load {
        // The load average itself is stored raw; the *status* comes from the per-core
        // normalisation, because absolute load is meaningless without a core count.
        let per_core_status = match load.per_core(snapshot.system.cpu_cores).value() {
            Some(v) => thresholds.load_per_core.classify(v),
            None => Status::Unknown,
        };
        results.push(MetricResult::new(
            MetricKind::LoadAverage1,
            MetricValue::available(load.one),
            per_core_status,
            at,
        ));
        push(
            &mut results,
            MetricKind::LoadAverage5,
            MetricValue::available(load.five),
            None,
            at,
        );
        push(
            &mut results,
            MetricKind::LoadAverage15,
            MetricValue::available(load.fifteen),
            None,
            at,
        );
    }

    if let Some(uptime) = snapshot.uptime_secs {
        push(
            &mut results,
            MetricKind::UptimeSeconds,
            MetricValue::available(uptime as f64),
            None,
            at,
        );
    }
    if let Some(used) = snapshot.memory.used_bytes {
        push(
            &mut results,
            MetricKind::MemoryUsedBytes,
            MetricValue::available(used as f64),
            None,
            at,
        );
    }
    if !snapshot.filesystems.is_empty() {
        push(
            &mut results,
            MetricKind::DiskUsedBytes,
            MetricValue::available(snapshot.used_disk_bytes() as f64),
            None,
            at,
        );
    }
    if !snapshot.processes.is_empty() {
        push(
            &mut results,
            MetricKind::ProcessCount,
            MetricValue::available(snapshot.processes.len() as f64),
            None,
            at,
        );
    }

    results
}

/// Percentage helper shared by the usage structs.
fn percentage(part: Option<u64>, whole: Option<u64>) -> MetricValue {
    match (part, whole) {
        (Some(p), Some(w)) if w > 0 => MetricValue::available(p as f64 / w as f64 * 100.0),
        _ => MetricValue::NotAvailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn sample_server() -> Server {
        Server::new(
            "web-01",
            "10.0.0.5",
            ConnectionSettings::Ssh(SshSettings {
                username: "root".into(),
                auth_kind: SshAuthKind::PrivateKey,
                credential_ref: CredentialRef::new(),
            }),
            at(0),
        )
    }

    #[test]
    fn a_default_server_is_valid() {
        assert_eq!(sample_server().validate(), Ok(()));
    }

    #[test]
    fn validation_rejects_blank_identity() {
        let mut server = sample_server();
        server.name = "   ".into();
        assert_eq!(server.validate(), Err(ServerValidationError::EmptyName));

        let mut server = sample_server();
        server.host = String::new();
        assert_eq!(server.validate(), Err(ServerValidationError::EmptyHost));
    }

    #[test]
    fn validation_rejects_zero_offline_threshold() {
        let mut server = sample_server();
        server.offline_after_failures = 0;
        assert_eq!(
            server.validate(),
            Err(ServerValidationError::InvalidFailureThreshold)
        );
    }

    #[test]
    fn validation_rejects_a_timeout_that_would_pile_up_checks() {
        let mut server = sample_server();
        server.poll_interval_secs = 15;
        server.timeout_secs = 120;
        assert_eq!(
            server.validate(),
            Err(ServerValidationError::TimeoutExceedsInterval)
        );
    }

    #[test]
    fn validation_rejects_inverted_thresholds() {
        let mut server = sample_server();
        server.thresholds.cpu = Threshold::above(95.0, 80.0);
        assert_eq!(
            server.validate(),
            Err(ServerValidationError::IncoherentThreshold)
        );
    }

    #[test]
    fn memory_percentage_needs_both_numbers() {
        let mem = MemoryUsage {
            total_bytes: Some(1_000),
            used_bytes: Some(250),
            ..Default::default()
        };
        assert_eq!(mem.used_percent(), MetricValue::Available(25.0));

        let partial = MemoryUsage {
            total_bytes: Some(1_000),
            ..Default::default()
        };
        assert_eq!(partial.used_percent(), MetricValue::NotAvailable);
    }

    #[test]
    fn zero_sized_swap_is_unavailable_not_a_division_by_zero() {
        let mem = MemoryUsage {
            swap_total_bytes: Some(0),
            swap_used_bytes: Some(0),
            ..Default::default()
        };
        assert_eq!(mem.swap_used_percent(), MetricValue::NotAvailable);
    }

    #[test]
    fn worst_filesystem_wins() {
        let mut snapshot = ServerSnapshot::new(ServerId::new(), at(0));
        snapshot.filesystems = vec![
            FilesystemUsage {
                mount_point: "/".into(),
                device: None,
                filesystem: None,
                total_bytes: 100,
                used_bytes: 20,
                available_bytes: 80,
            },
            FilesystemUsage {
                mount_point: "/var".into(),
                device: None,
                filesystem: None,
                total_bytes: 100,
                used_bytes: 91,
                available_bytes: 9,
            },
        ];
        assert_eq!(
            snapshot.worst_filesystem_percent(),
            MetricValue::Available(91.0)
        );
        assert_eq!(snapshot.total_disk_bytes(), 200);
        assert_eq!(snapshot.used_disk_bytes(), 111);
    }

    #[test]
    fn load_per_core_needs_a_core_count() {
        let load = LoadAverage {
            one: 4.0,
            five: 3.0,
            fifteen: 2.0,
        };
        assert_eq!(load.per_core(Some(4)), MetricValue::Available(1.0));
        assert_eq!(load.per_core(None), MetricValue::NotAvailable);
        assert_eq!(load.per_core(Some(0)), MetricValue::NotAvailable);
    }

    #[test]
    fn container_status_distinguishes_unhealthy_from_stopped() {
        let base = ContainerInfo {
            id: "abc".into(),
            name: "web".into(),
            image: "nginx".into(),
            state: ContainerState::Running,
            health: ContainerHealth::Unhealthy,
            status_text: "Up 2 days (unhealthy)".into(),
            cpu_percent: MetricValue::NotAvailable,
            memory_used_bytes: None,
            memory_limit_bytes: None,
            restart_count: None,
            started_at: None,
        };
        assert_eq!(base.status(), Status::Critical);

        let healthy = ContainerInfo {
            health: ContainerHealth::None,
            ..base.clone()
        };
        assert_eq!(healthy.status(), Status::Healthy);

        let stopped = ContainerInfo {
            state: ContainerState::Exited,
            ..base.clone()
        };
        assert_eq!(stopped.status(), Status::Warning);

        let restarting = ContainerInfo {
            state: ContainerState::Restarting,
            ..base
        };
        assert_eq!(restarting.status(), Status::Critical);
    }

    #[test]
    fn service_states_map_to_statuses() {
        assert_eq!(ServiceState::parse("active").status(), Status::Healthy);
        assert_eq!(ServiceState::parse("failed").status(), Status::Critical);
        assert_eq!(ServiceState::parse("inactive").status(), Status::Warning);
        assert_eq!(ServiceState::parse("wat").status(), Status::Unknown);
    }

    #[test]
    fn evaluation_flags_the_breaching_metric_and_leaves_others_healthy() {
        let mut snapshot = ServerSnapshot::new(ServerId::new(), at(0));
        snapshot.cpu.total_percent = MetricValue::Available(97.0);
        snapshot.memory = MemoryUsage {
            total_bytes: Some(100),
            used_bytes: Some(10),
            ..Default::default()
        };

        let results = evaluate_snapshot(&snapshot, &MonitoringThresholds::default());
        let cpu = results
            .iter()
            .find(|r| r.kind == crate::metrics::MetricKind::CpuUsage)
            .expect("cpu result present");
        let memory = results
            .iter()
            .find(|r| r.kind == crate::metrics::MetricKind::MemoryUsage)
            .expect("memory result present");

        assert_eq!(cpu.status, Status::Critical);
        assert_eq!(memory.status, Status::Healthy);
    }

    #[test]
    fn missing_measurements_evaluate_to_unknown_never_healthy() {
        let snapshot = ServerSnapshot::new(ServerId::new(), at(0));
        let results = evaluate_snapshot(&snapshot, &MonitoringThresholds::default());
        assert!(!results.is_empty());
        assert!(
            results.iter().all(|r| r.status == Status::Unknown),
            "an empty snapshot must not report healthy metrics"
        );
    }

    #[test]
    fn load_status_is_normalised_by_core_count() {
        let mut snapshot = ServerSnapshot::new(ServerId::new(), at(0));
        snapshot.load = Some(LoadAverage {
            one: 8.0,
            five: 8.0,
            fifteen: 8.0,
        });
        snapshot.system.cpu_cores = Some(16);

        let results = evaluate_snapshot(&snapshot, &MonitoringThresholds::default());
        let load = results
            .iter()
            .find(|r| r.kind == crate::metrics::MetricKind::LoadAverage1)
            .expect("load result present");
        // 8.0 across 16 cores is 0.5 per core — healthy, despite the large absolute value.
        assert_eq!(load.status, Status::Healthy);
        assert_eq!(load.value, MetricValue::Available(8.0));
    }

    #[test]
    fn connection_settings_expose_the_credential_handle_for_both_modes() {
        let secret = CredentialRef::new();
        let ssh = ConnectionSettings::Ssh(SshSettings {
            username: "root".into(),
            auth_kind: SshAuthKind::Password,
            credential_ref: secret,
        });
        assert_eq!(ssh.credential_ref(), secret);
        assert_eq!(ssh.mode(), ConnectionMode::Ssh);

        let agent = ConnectionSettings::Agent(AgentSettings {
            port: 9443,
            credential_ref: secret,
            certificate_fingerprint: None,
        });
        assert_eq!(agent.credential_ref(), secret);
        assert_eq!(agent.mode(), ConnectionMode::Agent);
    }

    #[test]
    fn container_health_round_trips_through_its_wire_form() {
        for health in [
            ContainerHealth::Healthy,
            ContainerHealth::Unhealthy,
            ContainerHealth::Starting,
            ContainerHealth::None,
        ] {
            assert_eq!(ContainerHealth::parse(health.as_str()), health);
        }
    }

    #[test]
    fn an_unfamiliar_health_word_never_reads_as_unhealthy() {
        // A newer agent must not be able to make a container look broken just by using
        // a word this version has not heard of.
        assert_eq!(ContainerHealth::parse("thriving"), ContainerHealth::None);
        assert_eq!(ContainerHealth::parse(""), ContainerHealth::None);
        assert_eq!(ContainerHealth::parse("HEALTHY"), ContainerHealth::Healthy);
    }
}
