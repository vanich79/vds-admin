//! # `vds-agent-protocol` — the wire contract between the app and `vds-agent`
//!
//! Shared by both sides so they cannot disagree about the format. Kept in its own crate,
//! with no dependency on the domain, so that the protocol can evolve on its own schedule
//! and so a third-party agent could implement it without pulling in the application's
//! internals.
//!
//! ## Versioning
//!
//! Every message carries [`PROTOCOL_VERSION`]. The app refuses to talk to an agent whose
//! major version differs, and tolerates a *newer* minor version by ignoring fields it
//! does not know — which is what `#[serde(default)]` on additive fields buys.
//!
//! ## Transport
//!
//! HTTPS with a bearer token, plus certificate pinning on the client side. The protocol
//! itself is transport-agnostic: these are just types.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use serde::{Deserialize, Serialize};

/// Semantic version of this protocol.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion { major: 1, minor: 0 };

/// Default TCP port the agent listens on.
pub const DEFAULT_AGENT_PORT: u16 = 9443;

/// HTTP header carrying the bearer token.
pub const AUTH_HEADER: &str = "authorization";

/// Path of the metrics endpoint.
pub const PATH_METRICS: &str = "/v1/metrics";
/// Path of the liveness endpoint. Unauthenticated by design: it reveals nothing.
pub const PATH_HEALTH: &str = "/v1/health";
/// Path of the agent-information endpoint.
pub const PATH_INFO: &str = "/v1/info";

/// Protocol version, compared by the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    /// Whether a peer speaking `other` can be understood.
    ///
    /// Same major version is required. A newer minor version is accepted: the peer may
    /// send fields we ignore, which is safe. An older minor version is also accepted:
    /// we may receive fewer fields, and every additive field has a default.
    pub fn is_compatible_with(self, other: ProtocolVersion) -> bool {
        self.major == other.major
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Response from [`PATH_INFO`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub protocol_version: ProtocolVersion,
    /// Version of the agent binary itself.
    pub agent_version: String,
    pub hostname: String,
    /// Target triple the agent was built for.
    pub architecture: String,
    /// Seconds the agent process has been running.
    pub agent_uptime_secs: u64,
    /// Which optional collectors this host supports.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Response from [`PATH_HEALTH`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub protocol_version: ProtocolVersion,
}

/// Response from [`PATH_METRICS`]: a complete reading of the host.
///
/// Mirrors the shape of a domain snapshot without depending on the domain crate. The
/// application converts it; that conversion is the single place the two models meet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricsReport {
    pub protocol_version: ProtocolVersion,
    /// Unix timestamp, seconds.
    pub collected_at: i64,
    pub system: SystemReport,
    pub cpu: CpuReport,
    pub memory: MemoryReport,
    #[serde(default)]
    pub filesystems: Vec<FilesystemReport>,
    #[serde(default)]
    pub interfaces: Vec<InterfaceReport>,
    #[serde(default)]
    pub load: Option<LoadReport>,
    #[serde(default)]
    pub uptime_secs: Option<u64>,
    #[serde(default)]
    pub temperature_celsius: Option<f64>,
    #[serde(default)]
    pub processes: Vec<ProcessReport>,
    /// `None` means Docker was not detected; an empty vector means Docker is present
    /// with no containers. The distinction is load-bearing in the UI.
    #[serde(default)]
    pub containers: Option<Vec<ContainerReport>>,
    #[serde(default)]
    pub services: Option<Vec<ServiceReport>>,
    /// Collectors that failed, so a partial report is explainable rather than silent.
    #[serde(default)]
    pub errors: Vec<CollectorErrorReport>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemReport {
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub os_name: Option<String>,
    #[serde(default)]
    pub os_version: Option<String>,
    #[serde(default)]
    pub kernel: Option<String>,
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub cpu_model: Option<String>,
    #[serde(default)]
    pub cpu_cores: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuReport {
    #[serde(default)]
    pub total_percent: Option<f64>,
    #[serde(default)]
    pub user_percent: Option<f64>,
    #[serde(default)]
    pub system_percent: Option<f64>,
    #[serde(default)]
    pub iowait_percent: Option<f64>,
    #[serde(default)]
    pub cores: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReport {
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub used_bytes: Option<u64>,
    #[serde(default)]
    pub available_bytes: Option<u64>,
    #[serde(default)]
    pub swap_total_bytes: Option<u64>,
    #[serde(default)]
    pub swap_used_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemReport {
    pub mount_point: String,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub filesystem: Option<String>,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceReport {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    #[serde(default)]
    pub rx_errors: u64,
    #[serde(default)]
    pub tx_errors: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct LoadReport {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessReport {
    pub pid: u32,
    #[serde(default)]
    pub user: Option<String>,
    pub command: String,
    pub cpu_percent: f64,
    pub memory_percent: f64,
    #[serde(default)]
    pub rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContainerReport {
    pub id: String,
    pub name: String,
    pub image: String,
    pub state: String,
    #[serde(default)]
    pub health: Option<String>,
    #[serde(default)]
    pub status_text: String,
    #[serde(default)]
    pub cpu_percent: Option<f64>,
    #[serde(default)]
    pub memory_used_bytes: Option<u64>,
    #[serde(default)]
    pub memory_limit_bytes: Option<u64>,
    #[serde(default)]
    pub restart_count: Option<u32>,
    /// Unix timestamp, seconds.
    #[serde(default)]
    pub started_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceReport {
    pub name: String,
    pub state: String,
    #[serde(default)]
    pub sub_state: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// A collector that did not produce a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectorErrorReport {
    pub collector: String,
    pub message: String,
    /// True when the host simply lacks the feature (no Docker, no systemd), which is not
    /// a fault.
    #[serde(default)]
    pub unsupported: bool,
}

/// Error body returned by the agent for any non-2xx response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default)]
    pub detail: Option<String>,
}

/// Formats a bearer token for the [`AUTH_HEADER`].
pub fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Extracts a bearer token from an `Authorization` header value.
///
/// Returns `None` for anything that is not exactly a bearer scheme, rather than trying
/// to be lenient: a permissive parser on an auth path is a liability.
pub fn parse_bearer(header: &str) -> Option<&str> {
    let rest = header
        .strip_prefix("Bearer ")
        .or_else(|| header.strip_prefix("bearer "))?;
    let token = rest.trim();
    if token.is_empty() { None } else { Some(token) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_major_versions_are_compatible() {
        let v1 = ProtocolVersion { major: 1, minor: 0 };
        assert!(v1.is_compatible_with(ProtocolVersion { major: 1, minor: 4 }));
        assert!(v1.is_compatible_with(ProtocolVersion { major: 1, minor: 0 }));
        assert!(!v1.is_compatible_with(ProtocolVersion { major: 2, minor: 0 }));
    }

    #[test]
    fn version_renders_as_major_dot_minor() {
        assert_eq!(PROTOCOL_VERSION.to_string(), "1.0");
    }

    #[test]
    fn bearer_tokens_round_trip() {
        let header = bearer("s3cret");
        assert_eq!(header, "Bearer s3cret");
        assert_eq!(parse_bearer(&header), Some("s3cret"));
    }

    #[test]
    fn malformed_authorization_headers_are_rejected() {
        assert_eq!(parse_bearer("Basic abc"), None);
        assert_eq!(parse_bearer("Bearer "), None);
        assert_eq!(parse_bearer("s3cret"), None);
        assert_eq!(parse_bearer(""), None);
    }

    #[test]
    fn a_minimal_report_deserialises_from_the_required_fields_only() {
        // Everything optional really is optional, so an older agent's smaller payload
        // still parses.
        let json = r#"{
            "protocol_version": {"major": 1, "minor": 0},
            "collected_at": 1700000000,
            "system": {},
            "cpu": {},
            "memory": {}
        }"#;
        let report: MetricsReport = serde_json::from_str(json).expect("minimal report parses");
        assert_eq!(report.collected_at, 1_700_000_000);
        assert!(report.filesystems.is_empty());
        assert_eq!(report.containers, None);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn unknown_fields_from_a_newer_agent_are_ignored() {
        let json = r#"{
            "protocol_version": {"major": 1, "minor": 9},
            "collected_at": 1,
            "system": {},
            "cpu": {},
            "memory": {},
            "something_from_the_future": {"nested": [1, 2, 3]}
        }"#;
        let report: MetricsReport = serde_json::from_str(json).expect("forward-compatible parse");
        assert_eq!(report.protocol_version.minor, 9);
    }

    #[test]
    fn docker_absence_and_empty_docker_are_distinguishable_on_the_wire() {
        let absent = r#"{"protocol_version":{"major":1,"minor":0},"collected_at":1,
            "system":{},"cpu":{},"memory":{},"containers":null}"#;
        let empty = r#"{"protocol_version":{"major":1,"minor":0},"collected_at":1,
            "system":{},"cpu":{},"memory":{},"containers":[]}"#;

        let absent: MetricsReport = serde_json::from_str(absent).expect("parses");
        let empty: MetricsReport = serde_json::from_str(empty).expect("parses");

        assert_eq!(absent.containers, None);
        assert_eq!(empty.containers, Some(Vec::new()));
    }

    #[test]
    fn a_full_report_round_trips() {
        let report = MetricsReport {
            protocol_version: PROTOCOL_VERSION,
            collected_at: 1_700_000_000,
            system: SystemReport {
                hostname: Some("web-01".into()),
                cpu_cores: Some(4),
                ..Default::default()
            },
            cpu: CpuReport {
                total_percent: Some(12.5),
                ..Default::default()
            },
            memory: MemoryReport {
                total_bytes: Some(8 * 1024 * 1024 * 1024),
                used_bytes: Some(2 * 1024 * 1024 * 1024),
                ..Default::default()
            },
            filesystems: vec![FilesystemReport {
                mount_point: "/".into(),
                device: Some("/dev/sda1".into()),
                filesystem: Some("ext4".into()),
                total_bytes: 100,
                used_bytes: 40,
                available_bytes: 60,
            }],
            interfaces: vec![InterfaceReport {
                name: "eth0".into(),
                rx_bytes: 1,
                tx_bytes: 2,
                rx_errors: 0,
                tx_errors: 0,
            }],
            load: Some(LoadReport {
                one: 0.5,
                five: 0.4,
                fifteen: 0.3,
            }),
            uptime_secs: Some(12_345),
            temperature_celsius: Some(41.0),
            processes: vec![ProcessReport {
                pid: 1,
                user: Some("root".into()),
                command: "/sbin/init".into(),
                cpu_percent: 0.1,
                memory_percent: 0.2,
                rss_bytes: Some(4096),
            }],
            containers: Some(vec![ContainerReport {
                id: "abc123".into(),
                name: "web".into(),
                image: "nginx:latest".into(),
                state: "running".into(),
                health: Some("healthy".into()),
                status_text: "Up 3 days".into(),
                cpu_percent: Some(1.5),
                memory_used_bytes: Some(1024),
                memory_limit_bytes: Some(2048),
                restart_count: Some(0),
                started_at: Some(1_699_000_000),
            }]),
            services: Some(vec![ServiceReport {
                name: "nginx.service".into(),
                state: "active".into(),
                sub_state: Some("running".into()),
                description: Some("nginx".into()),
                enabled: Some(true),
            }]),
            errors: vec![CollectorErrorReport {
                collector: "docker".into(),
                message: "not installed".into(),
                unsupported: true,
            }],
        };

        let json = serde_json::to_string(&report).expect("serialises");
        let back: MetricsReport = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, report);
    }
}
