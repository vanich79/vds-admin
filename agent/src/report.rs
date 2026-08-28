//! The single place the domain model and the wire protocol meet.
//!
//! `vds-infra-collectors` produces a [`ServerSnapshot`]; the app expects a
//! [`MetricsReport`]. Everything about that translation lives here, as pure functions, so
//! it can be tested without a socket, a process or a Linux host.
//!
//! Two rules the translation must never break:
//!
//! * **absent is not zero.** A [`MetricValue::NotAvailable`] becomes a JSON `null`, never
//!   a `0.0`. The whole point of the type is preserved across the wire.
//! * **absent is not empty.** `containers: null` means "Docker was not found"; an empty
//!   array means "Docker is here and running nothing". The app renders those differently,
//!   so collapsing them would be a lie.

use vds_agent_protocol::{
    CollectorErrorReport, ContainerReport, CpuReport, FilesystemReport, InterfaceReport,
    LoadReport, MemoryReport, MetricsReport, PROTOCOL_VERSION, ProcessReport, ServiceReport,
    SystemReport,
};
use vds_domain::Status;
use vds_domain::server::{
    ContainerInfo, FilesystemUsage, NetworkInterface, ProcessInfo, ServerSnapshot, ServiceInfo,
};

/// Converts a collected snapshot into the wire format.
pub fn to_report(snapshot: &ServerSnapshot) -> MetricsReport {
    MetricsReport {
        protocol_version: PROTOCOL_VERSION,
        collected_at: snapshot.collected_at.timestamp(),
        system: SystemReport {
            hostname: snapshot.system.hostname.clone(),
            os_name: snapshot.system.os_name.clone(),
            os_version: snapshot.system.os_version.clone(),
            kernel: snapshot.system.kernel.clone(),
            architecture: snapshot.system.architecture.clone(),
            cpu_model: snapshot.system.cpu_model.clone(),
            cpu_cores: snapshot.system.cpu_cores,
        },
        cpu: CpuReport {
            total_percent: snapshot.cpu.total_percent.value(),
            user_percent: snapshot.cpu.user_percent.value(),
            system_percent: snapshot.cpu.system_percent.value(),
            iowait_percent: snapshot.cpu.iowait_percent.value(),
            cores: snapshot.cpu.cores,
        },
        memory: MemoryReport {
            total_bytes: snapshot.memory.total_bytes,
            used_bytes: snapshot.memory.used_bytes,
            available_bytes: snapshot.memory.available_bytes,
            swap_total_bytes: snapshot.memory.swap_total_bytes,
            swap_used_bytes: snapshot.memory.swap_used_bytes,
        },
        filesystems: snapshot.filesystems.iter().map(filesystem).collect(),
        interfaces: snapshot.interfaces.iter().map(interface).collect(),
        load: snapshot.load.map(|load| LoadReport {
            one: load.one,
            five: load.five,
            fifteen: load.fifteen,
        }),
        uptime_secs: snapshot.uptime_secs,
        temperature_celsius: snapshot.temperature_celsius.value(),
        processes: snapshot.processes.iter().map(process).collect(),
        // `Option` is preserved deliberately: see the module documentation.
        containers: snapshot
            .containers
            .as_ref()
            .map(|list| list.iter().map(container).collect()),
        services: snapshot
            .services
            .as_ref()
            .map(|list| list.iter().map(service).collect()),
        errors: snapshot.outcomes.iter().filter_map(outcome).collect(),
    }
}

fn filesystem(usage: &FilesystemUsage) -> FilesystemReport {
    FilesystemReport {
        mount_point: usage.mount_point.clone(),
        device: usage.device.clone(),
        filesystem: usage.filesystem.clone(),
        total_bytes: usage.total_bytes,
        used_bytes: usage.used_bytes,
        available_bytes: usage.available_bytes,
    }
}

fn interface(nic: &NetworkInterface) -> InterfaceReport {
    InterfaceReport {
        name: nic.name.clone(),
        rx_bytes: nic.rx_bytes,
        tx_bytes: nic.tx_bytes,
        rx_errors: nic.rx_errors,
        tx_errors: nic.tx_errors,
    }
}

fn process(info: &ProcessInfo) -> ProcessReport {
    ProcessReport {
        pid: info.pid,
        user: info.user.clone(),
        command: info.command.clone(),
        cpu_percent: info.cpu_percent,
        memory_percent: info.memory_percent,
        rss_bytes: info.rss_bytes,
    }
}

fn container(info: &ContainerInfo) -> ContainerReport {
    ContainerReport {
        id: info.id.clone(),
        name: info.name.clone(),
        image: info.image.clone(),
        state: info.state.as_str().to_owned(),
        health: Some(info.health.as_str().to_owned()),
        status_text: info.status_text.clone(),
        cpu_percent: info.cpu_percent.value(),
        memory_used_bytes: info.memory_used_bytes,
        memory_limit_bytes: info.memory_limit_bytes,
        restart_count: info.restart_count,
        started_at: info.started_at.map(|at| at.timestamp()),
    }
}

fn service(info: &ServiceInfo) -> ServiceReport {
    ServiceReport {
        name: info.name.clone(),
        state: info.state.as_str().to_owned(),
        sub_state: info.sub_state.clone(),
        description: info.description.clone(),
        enabled: info.enabled,
    }
}

/// Reports a collector that did not produce a result.
///
/// Successes are omitted — the report would otherwise carry a line per collector on every
/// scrape, which is noise. `Unknown` is how the collector layer signals "this host does
/// not have that feature", and it travels as `unsupported: true` so the app can show it
/// as an absence rather than as a fault.
fn outcome(outcome: &vds_domain::metrics::CollectorOutcome) -> Option<CollectorErrorReport> {
    match outcome.status {
        Status::Healthy => None,
        status => Some(CollectorErrorReport {
            collector: outcome.collector.as_str().to_owned(),
            message: outcome
                .message
                .clone()
                .unwrap_or_else(|| "collection failed".to_owned()),
            unsupported: status == Status::Unknown,
        }),
    }
}

/// The optional collectors this build can run, for the info endpoint.
pub fn capabilities() -> Vec<String> {
    vec![
        "system".to_owned(),
        "cpu".to_owned(),
        "memory".to_owned(),
        "disk".to_owned(),
        "network".to_owned(),
        "load".to_owned(),
        "process".to_owned(),
        "docker".to_owned(),
        "systemd".to_owned(),
        "temperature".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use vds_domain::ids::{CollectorId, ServerId};
    use vds_domain::metrics::{CollectorOutcome, MetricValue};
    use vds_domain::server::{
        ContainerHealth, ContainerState, CpuUsage, LoadAverage, MemoryUsage, ServiceState,
        SystemInfo,
    };

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap_or_default()
    }

    fn snapshot() -> ServerSnapshot {
        ServerSnapshot::new(ServerId::new(), at(1_700_000_000))
    }

    #[test]
    fn an_unavailable_metric_travels_as_null_not_as_zero() {
        // The distinction is the reason `MetricValue` exists; losing it here would undo
        // the whole chain.
        let mut snap = snapshot();
        snap.cpu = CpuUsage::default();
        snap.temperature_celsius = MetricValue::NotAvailable;

        let report = to_report(&snap);
        assert_eq!(report.cpu.total_percent, None);
        assert_eq!(report.temperature_celsius, None);

        let json = serde_json::to_string(&report).unwrap_or_default();
        assert!(json.contains("\"total_percent\":null"), "json was: {json}");
    }

    #[test]
    fn a_measured_metric_keeps_its_value() {
        let mut snap = snapshot();
        snap.cpu.total_percent = MetricValue::Available(12.5);
        snap.temperature_celsius = MetricValue::Available(41.0);

        let report = to_report(&snap);
        assert_eq!(report.cpu.total_percent, Some(12.5));
        assert_eq!(report.temperature_celsius, Some(41.0));
    }

    #[test]
    fn absent_docker_and_empty_docker_stay_distinguishable() {
        // `null` means "no Docker on this host"; `[]` means "Docker with nothing running".
        // The UI shows a hidden panel for one and an empty panel for the other.
        let mut absent = snapshot();
        absent.containers = None;
        assert_eq!(to_report(&absent).containers, None);

        let mut empty = snapshot();
        empty.containers = Some(Vec::new());
        assert_eq!(to_report(&empty).containers, Some(Vec::new()));
    }

    #[test]
    fn the_same_holds_for_systemd() {
        let mut absent = snapshot();
        absent.services = None;
        assert_eq!(to_report(&absent).services, None);

        let mut empty = snapshot();
        empty.services = Some(Vec::new());
        assert_eq!(to_report(&empty).services, Some(Vec::new()));
    }

    #[test]
    fn a_missing_feature_is_reported_as_unsupported_rather_than_as_a_failure() {
        // A host without Docker is not a broken host, and the app must not show it as one.
        let mut snap = snapshot();
        snap.outcomes = vec![CollectorOutcome {
            collector: CollectorId::new("docker"),
            status: Status::Unknown,
            message: Some("docker is not installed".to_owned()),
        }];

        let report = to_report(&snap);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].unsupported);
        assert_eq!(report.errors[0].collector, "docker");
    }

    #[test]
    fn a_real_collector_failure_is_not_marked_unsupported() {
        let mut snap = snapshot();
        snap.outcomes = vec![CollectorOutcome {
            collector: CollectorId::new("disk"),
            status: Status::Critical,
            message: Some("permission denied".to_owned()),
        }];

        let report = to_report(&snap);
        assert!(!report.errors[0].unsupported);
        assert_eq!(report.errors[0].message, "permission denied");
    }

    #[test]
    fn successful_collectors_are_omitted_from_the_error_list() {
        // Otherwise every scrape would carry ten lines saying nothing went wrong.
        let mut snap = snapshot();
        snap.outcomes = vec![
            CollectorOutcome {
                collector: CollectorId::new("cpu"),
                status: Status::Healthy,
                message: None,
            },
            CollectorOutcome {
                collector: CollectorId::new("docker"),
                status: Status::Unknown,
                message: None,
            },
        ];

        let report = to_report(&snap);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].collector, "docker");
    }

    #[test]
    fn a_failure_without_a_message_still_says_something() {
        let mut snap = snapshot();
        snap.outcomes = vec![CollectorOutcome {
            collector: CollectorId::new("memory"),
            status: Status::Critical,
            message: None,
        }];
        assert!(!to_report(&snap).errors[0].message.is_empty());
    }

    #[test]
    fn a_full_snapshot_survives_the_round_trip_through_json() {
        let mut snap = snapshot();
        snap.system = SystemInfo {
            hostname: Some("web-01".to_owned()),
            os_name: Some("Debian GNU/Linux".to_owned()),
            os_version: Some("12".to_owned()),
            kernel: Some("6.1.0".to_owned()),
            architecture: Some("x86_64".to_owned()),
            cpu_model: Some("AMD EPYC".to_owned()),
            cpu_cores: Some(4),
        };
        snap.cpu = CpuUsage {
            total_percent: MetricValue::Available(12.5),
            user_percent: MetricValue::Available(9.0),
            system_percent: MetricValue::Available(3.0),
            iowait_percent: MetricValue::Available(0.5),
            cores: Some(4),
        };
        snap.memory = MemoryUsage {
            total_bytes: Some(8_000_000),
            used_bytes: Some(2_000_000),
            available_bytes: Some(6_000_000),
            swap_total_bytes: Some(1_000_000),
            swap_used_bytes: Some(0),
        };
        snap.filesystems = vec![FilesystemUsage {
            mount_point: "/".to_owned(),
            device: Some("/dev/sda1".to_owned()),
            filesystem: Some("ext4".to_owned()),
            total_bytes: 100,
            used_bytes: 40,
            available_bytes: 60,
        }];
        snap.interfaces = vec![NetworkInterface {
            name: "eth0".to_owned(),
            rx_bytes: 10,
            tx_bytes: 20,
            rx_errors: 0,
            tx_errors: 1,
        }];
        snap.load = Some(LoadAverage {
            one: 0.5,
            five: 0.4,
            fifteen: 0.3,
        });
        snap.uptime_secs = Some(86_400);
        snap.processes = vec![ProcessInfo {
            pid: 1,
            user: Some("root".to_owned()),
            command: "/sbin/init".to_owned(),
            cpu_percent: 0.1,
            memory_percent: 0.2,
            rss_bytes: Some(4_096),
        }];
        snap.containers = Some(vec![ContainerInfo {
            id: "abc".to_owned(),
            name: "web".to_owned(),
            image: "nginx:latest".to_owned(),
            state: ContainerState::Running,
            health: ContainerHealth::Healthy,
            status_text: "Up 3 days (healthy)".to_owned(),
            cpu_percent: MetricValue::Available(1.5),
            memory_used_bytes: Some(1_024),
            memory_limit_bytes: Some(2_048),
            restart_count: Some(0),
            started_at: Some(at(1_699_000_000)),
        }]);
        snap.services = Some(vec![ServiceInfo {
            name: "nginx.service".to_owned(),
            state: ServiceState::Active,
            sub_state: Some("running".to_owned()),
            description: Some("nginx".to_owned()),
            enabled: Some(true),
        }]);

        let report = to_report(&snap);
        let json = serde_json::to_string(&report).unwrap_or_default();
        let back: MetricsReport =
            serde_json::from_str(&json).unwrap_or_else(|_| to_report(&snapshot()));

        assert_eq!(back, report);
        assert_eq!(back.system.hostname.as_deref(), Some("web-01"));
        assert_eq!(back.collected_at, 1_700_000_000);
        assert_eq!(
            back.containers
                .as_ref()
                .and_then(|c| c.first())
                .map(|c| c.state.as_str()),
            Some("running")
        );
        assert_eq!(
            back.services
                .as_ref()
                .and_then(|s| s.first())
                .map(|s| s.state.as_str()),
            Some("active")
        );
    }

    #[test]
    fn container_state_and_health_travel_as_the_words_the_domain_parses_back() {
        // The app parses these with `ContainerState::parse`/`ContainerHealth::parse`; if
        // the spellings drift, every container silently becomes `Unknown`.
        let mut snap = snapshot();
        snap.containers = Some(vec![ContainerInfo {
            id: "x".to_owned(),
            name: "x".to_owned(),
            image: "x".to_owned(),
            state: ContainerState::Restarting,
            health: ContainerHealth::Unhealthy,
            status_text: String::new(),
            cpu_percent: MetricValue::NotAvailable,
            memory_used_bytes: None,
            memory_limit_bytes: None,
            restart_count: Some(7),
            started_at: None,
        }]);

        let report = to_report(&snap);
        let container = report
            .containers
            .as_ref()
            .and_then(|c| c.first())
            .cloned()
            .unwrap_or_else(|| ContainerReport {
                id: String::new(),
                name: String::new(),
                image: String::new(),
                state: String::new(),
                health: None,
                status_text: String::new(),
                cpu_percent: None,
                memory_used_bytes: None,
                memory_limit_bytes: None,
                restart_count: None,
                started_at: None,
            });

        assert_eq!(
            ContainerState::parse(&container.state),
            ContainerState::Restarting
        );
        assert_eq!(
            ContainerHealth::parse(container.health.as_deref().unwrap_or_default()),
            ContainerHealth::Unhealthy
        );
    }

    #[test]
    fn the_report_carries_the_protocol_version_this_build_speaks() {
        assert_eq!(to_report(&snapshot()).protocol_version, PROTOCOL_VERSION);
    }
}
