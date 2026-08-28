//! The collector registry: the one place that knows which collectors exist.
//!
//! Adding a collector is a single line here plus a new module. Nothing else in the
//! codebase changes — the registry batches its commands, routes the results back, and
//! merges its output like every other collector's.

use std::sync::Arc;
use vds_domain::Status;
use vds_domain::ids::ServerId;
use vds_domain::metrics::CollectorOutcome;
use vds_domain::ports::{
    CollectError, Collector, Command, CommandOutput, CommandRunner, TransportError,
};
use vds_domain::server::ServerSnapshot;

use crate::cpu::CpuCollector;
use crate::disk::DiskCollector;
use crate::docker::DockerCollector;
use crate::load::LoadCollector;
use crate::memory::MemoryCollector;
use crate::network::NetworkCollector;
use crate::process::ProcessCollector;
use crate::service::ServiceCollector;
use crate::system::SystemCollector;
use crate::temperature::TemperatureCollector;

/// An ordered set of collectors.
#[derive(Clone)]
pub struct CollectorRegistry {
    collectors: Vec<Arc<dyn Collector>>,
}

impl CollectorRegistry {
    /// Everything a Linux host can offer.
    pub fn linux() -> Self {
        Self {
            collectors: vec![
                Arc::new(SystemCollector),
                Arc::new(CpuCollector),
                Arc::new(MemoryCollector),
                Arc::new(DiskCollector),
                Arc::new(NetworkCollector),
                Arc::new(LoadCollector),
                Arc::new(ProcessCollector),
                Arc::new(TemperatureCollector),
                Arc::new(DockerCollector),
                Arc::new(ServiceCollector),
            ],
        }
    }

    /// Only the collectors needed for the core CPU/RAM/disk/uptime view.
    ///
    /// Used for very large fleets, where skipping the process table, Docker and systemd
    /// cuts the per-cycle payload by most of its weight.
    pub fn essential() -> Self {
        Self {
            collectors: vec![
                Arc::new(SystemCollector),
                Arc::new(CpuCollector),
                Arc::new(MemoryCollector),
                Arc::new(DiskCollector),
                Arc::new(LoadCollector),
            ],
        }
    }

    pub fn new(collectors: Vec<Arc<dyn Collector>>) -> Self {
        Self { collectors }
    }

    pub fn is_empty(&self) -> bool {
        self.collectors.is_empty()
    }

    pub fn len(&self) -> usize {
        self.collectors.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Collector>> {
        self.collectors.iter()
    }

    /// Every command every collector needs, flattened, with the slice each collector's
    /// results occupy.
    ///
    /// Flattening up front is what makes a single round trip possible.
    pub fn plan(&self) -> CollectionPlan {
        let mut commands = Vec::new();
        let mut spans = Vec::with_capacity(self.collectors.len());
        for collector in &self.collectors {
            let start = commands.len();
            commands.extend(collector.commands());
            spans.push(start..commands.len());
        }
        CollectionPlan { commands, spans }
    }

    /// Runs every collector against a transport and merges the results.
    ///
    /// One transport failure fails the whole collection — the host is unreachable. An
    /// individual collector failing does not: the snapshot carries whatever succeeded,
    /// plus a per-collector outcome explaining the rest.
    pub async fn collect(
        &self,
        runner: &dyn CommandRunner,
        server_id: ServerId,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<ServerSnapshot, TransportError> {
        let plan = self.plan();
        let results = runner.execute(&plan.commands).await?;
        Ok(self.assemble(&plan, &results, server_id, at))
    }

    /// Turns raw command results into a snapshot. Pure, and therefore testable.
    pub fn assemble(
        &self,
        plan: &CollectionPlan,
        results: &[Result<CommandOutput, TransportError>],
        server_id: ServerId,
        at: chrono::DateTime<chrono::Utc>,
    ) -> ServerSnapshot {
        let mut snapshot = ServerSnapshot::new(server_id, at);

        for (collector, span) in self.collectors.iter().zip(&plan.spans) {
            // A transport that returned fewer results than commands is a broken
            // transport; treat the missing ones as failures rather than panicking.
            let slice: Vec<Result<CommandOutput, TransportError>> = span
                .clone()
                .map(|index| {
                    results
                        .get(index)
                        .cloned()
                        .unwrap_or(Err(TransportError::Protocol(
                            "transport returned fewer results than commands".to_owned(),
                        )))
                })
                .collect();

            match collector.parse(&slice) {
                Ok(output) => {
                    output.apply(&mut snapshot);
                    snapshot.outcomes.push(CollectorOutcome {
                        collector: collector.id(),
                        status: Status::Healthy,
                        message: None,
                    });
                }
                Err(err) => {
                    let status = if err.affects_server_health() {
                        Status::Warning
                    } else {
                        // A host without Docker is not degraded.
                        Status::Unknown
                    };
                    snapshot.outcomes.push(CollectorOutcome {
                        collector: collector.id(),
                        status,
                        message: Some(err.to_string()),
                    });
                    tracing::debug!(
                        collector = %collector.id(),
                        error = %err,
                        "collector produced no result"
                    );
                }
            }
        }

        snapshot
    }
}

impl Default for CollectorRegistry {
    fn default() -> Self {
        Self::linux()
    }
}

impl std::fmt::Debug for CollectorRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ids: Vec<String> = self.collectors.iter().map(|c| c.id().to_string()).collect();
        f.debug_struct("CollectorRegistry")
            .field("collectors", &ids)
            .finish()
    }
}

/// The flattened command list plus where each collector's results live in it.
#[derive(Debug, Clone)]
pub struct CollectionPlan {
    pub commands: Vec<Command>,
    /// One span per collector, in registry order.
    pub spans: Vec<std::ops::Range<usize>>,
}

impl CollectionPlan {
    /// Time budget for the whole plan, in milliseconds.
    pub fn budget_ms(&self) -> u64 {
        self.commands.iter().map(Command::min_budget_ms).sum()
    }
}

/// Whether a collector's failure means the host lacks a feature rather than being ill.
pub fn is_capability_gap(err: &CollectError) -> bool {
    matches!(err, CollectError::Unsupported { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::ScriptedCommandRunner;
    use chrono::{DateTime, Utc};
    use vds_domain::metrics::MetricValue;
    use vds_domain::ports::Capability;
    use vds_domain::server::{ContainerState, ServiceState};

    fn at() -> DateTime<Utc> {
        DateTime::UNIX_EPOCH
    }

    const PROC_STAT_1: &str = "cpu  100 10 50 1000 20 0 5 0 0 0\ncpu0 50 5 25 500 10 0 2 0 0 0";
    const PROC_STAT_2: &str = "cpu  150 10 100 1080 40 0 5 0 0 0\ncpu0 75 5 50 540 20 0 2 0 0 0";

    /// A transport that answers like a healthy Ubuntu host running Docker.
    fn healthy_host() -> ScriptedCommandRunner {
        ScriptedCommandRunner::new()
            .on(Command::read("/proc/sys/kernel/hostname"), "prod-01\n")
            .on(
                Command::shell("cat /etc/os-release 2>/dev/null || true"),
                "PRETTY_NAME=\"Ubuntu 22.04.3 LTS\"\nVERSION_ID=\"22.04\"",
            )
            .on(Command::shell("uname -s -r -m"), "Linux 5.15.0-91-generic x86_64")
            .on(Command::read("/proc/cpuinfo"), "processor\t: 0\nmodel name\t: Xeon\n")
            .on(
                Command::sample_twice("/proc/stat", 500),
                format!("{PROC_STAT_1}\n---vds-sample---\n{PROC_STAT_2}"),
            )
            .on(
                Command::read("/proc/meminfo"),
                "MemTotal: 1000000 kB\nMemAvailable: 400000 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB",
            )
            .on(
                Command::shell("df -PkT 2>/dev/null || df -Pk"),
                "Filesystem Type 1024-blocks Used Available Capacity Mounted on\n\
                 /dev/sda1 ext4 1000000 720000 280000 72% /",
            )
            .on(
                Command::read("/proc/net/dev"),
                "  eth0: 100 1 0 0 0 0 0 0 200 2 0 0 0 0 0 0",
            )
            .on(Command::read("/proc/loadavg"), "0.50 0.40 0.30 1/200 300")
            .on(Command::read("/proc/uptime"), "12352000.0 1.0")
            .on(
                Command::shell("ps -eo pid,user,pcpu,pmem,rss,args 2>/dev/null"),
                "  PID USER %CPU %MEM RSS COMMAND\n    1 root 0.0 0.1 1234 /sbin/init",
            )
            .on(
                Command::shell("cat /sys/class/thermal/thermal_zone*/temp 2>/dev/null | head -c 4096"),
                "45000",
            )
            .on(
                Command::shell("docker ps -a --no-trunc --format '{{json .}}' 2>&1"),
                r#"{"ID":"abc","Names":"web","Image":"nginx","State":"running","Status":"Up 3 days"}"#,
            )
            .on(
                Command::shell("docker stats --no-stream --format '{{json .}}' 2>/dev/null"),
                "",
            )
            .on(
                Command::shell(
                    "docker ps -aq --no-trunc 2>/dev/null | xargs -r docker inspect \
                     --format '{{.Id}}|{{.RestartCount}}|{{.State.StartedAt}}' 2>/dev/null",
                ),
                "",
            )
            .on(
                Command::shell(
                    "systemctl list-units --type=service --all --no-legend --no-pager --plain 2>&1",
                ),
                "nginx.service loaded active running Web server\n\
                 redis.service loaded failed failed Redis",
            )
    }

    #[test]
    fn the_plan_flattens_every_collectors_commands_without_overlap() {
        let registry = CollectorRegistry::linux();
        let plan = registry.plan();

        assert_eq!(plan.spans.len(), registry.len());
        // Spans must tile the command list exactly.
        let mut expected_start = 0;
        for span in &plan.spans {
            assert_eq!(span.start, expected_start);
            expected_start = span.end;
        }
        assert_eq!(expected_start, plan.commands.len());
    }

    #[test]
    fn the_budget_accounts_for_the_cpu_sampling_pause() {
        let plan = CollectorRegistry::linux().plan();
        // The CPU collector's 500 ms double-sample dominates the floor.
        assert!(plan.budget_ms() >= 1_500, "budget was {}", plan.budget_ms());
    }

    #[tokio::test]
    async fn a_healthy_host_produces_a_complete_snapshot() {
        let registry = CollectorRegistry::linux();
        let snapshot = registry
            .collect(&healthy_host(), ServerId::new(), at())
            .await
            .expect("host reachable");

        assert_eq!(snapshot.system.hostname.as_deref(), Some("prod-01"));
        assert_eq!(
            snapshot.system.os_name.as_deref(),
            Some("Ubuntu 22.04.3 LTS")
        );
        assert_eq!(snapshot.cpu.total_percent, MetricValue::Available(50.0));
        assert_eq!(snapshot.memory.used_percent(), MetricValue::Available(60.0));
        assert_eq!(
            snapshot.worst_filesystem_percent(),
            MetricValue::Available(72.0)
        );
        assert_eq!(snapshot.load.map(|l| l.one), Some(0.5));
        assert_eq!(snapshot.uptime_secs, Some(12_352_000));
        assert_eq!(snapshot.temperature_celsius, MetricValue::Available(45.0));
        assert_eq!(snapshot.processes.len(), 1);
        assert_eq!(snapshot.interfaces.len(), 1);
    }

    #[tokio::test]
    async fn docker_and_systemd_results_reach_the_snapshot() {
        let snapshot = CollectorRegistry::linux()
            .collect(&healthy_host(), ServerId::new(), at())
            .await
            .expect("host reachable");

        let containers = snapshot.containers.as_ref().expect("docker present");
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].state, ContainerState::Running);

        let services = snapshot.services.as_ref().expect("systemd present");
        assert_eq!(services.len(), 2);
        assert_eq!(snapshot.failed_services().len(), 1);
        assert_eq!(services[1].state, ServiceState::Failed);
    }

    #[tokio::test]
    async fn every_collector_reports_an_outcome() {
        let registry = CollectorRegistry::linux();
        let snapshot = registry
            .collect(&healthy_host(), ServerId::new(), at())
            .await
            .expect("host reachable");
        assert_eq!(snapshot.outcomes.len(), registry.len());
        assert!(
            snapshot
                .outcomes
                .iter()
                .all(|o| o.status == Status::Healthy)
        );
    }

    #[tokio::test]
    async fn a_host_without_docker_or_systemd_is_still_fully_monitored() {
        // The case that matters most: an Alpine container host or a minimal VPS.
        let runner = healthy_host()
            .on_failure(
                Command::shell("docker ps -a --no-trunc --format '{{json .}}' 2>&1"),
                127,
                "sh: docker: not found",
            )
            .on_failure(
                Command::shell(
                    "systemctl list-units --type=service --all --no-legend --no-pager --plain 2>&1",
                ),
                127,
                "sh: systemctl: not found",
            );

        let snapshot = CollectorRegistry::linux()
            .collect(&runner, ServerId::new(), at())
            .await
            .expect("host reachable");

        // Core metrics are untouched.
        assert_eq!(snapshot.cpu.total_percent, MetricValue::Available(50.0));
        // Absent, not empty — the UI must be able to hide the panels entirely.
        assert_eq!(snapshot.containers, None);
        assert_eq!(snapshot.services, None);

        // And critically, the host is not marked degraded for lacking them.
        let docker_outcome = snapshot
            .outcomes
            .iter()
            .find(|o| o.collector.as_str() == "docker")
            .expect("docker outcome recorded");
        assert_eq!(docker_outcome.status, Status::Unknown);
        assert!(
            snapshot
                .outcomes
                .iter()
                .all(|o| o.status != Status::Warning)
        );
    }

    #[tokio::test]
    async fn one_broken_collector_does_not_void_the_others() {
        let runner = healthy_host().on_error(
            Command::read("/proc/meminfo"),
            TransportError::Execution("permission denied".into()),
        );

        let snapshot = CollectorRegistry::linux()
            .collect(&runner, ServerId::new(), at())
            .await
            .expect("host reachable");

        assert_eq!(snapshot.memory.used_percent(), MetricValue::NotAvailable);
        assert_eq!(snapshot.cpu.total_percent, MetricValue::Available(50.0));

        let memory_outcome = snapshot
            .outcomes
            .iter()
            .find(|o| o.collector.as_str() == "memory")
            .expect("memory outcome recorded");
        assert_eq!(memory_outcome.status, Status::Warning);
        assert!(memory_outcome.message.is_some());
    }

    #[tokio::test]
    async fn an_unreachable_host_fails_the_whole_collection() {
        // Distinct from a partial failure: there is nothing to report at all.
        let runner = ScriptedCommandRunner::new().offline(TransportError::Timeout { seconds: 20 });
        let err = CollectorRegistry::linux()
            .collect(&runner, ServerId::new(), at())
            .await
            .expect_err("must fail");
        assert_eq!(err, TransportError::Timeout { seconds: 20 });
    }

    #[tokio::test]
    async fn a_transport_returning_too_few_results_does_not_panic() {
        struct ShortRunner;

        #[async_trait::async_trait]
        impl CommandRunner for ShortRunner {
            async fn execute(
                &self,
                _commands: &[Command],
            ) -> Result<Vec<Result<CommandOutput, TransportError>>, TransportError> {
                Ok(vec![Ok(CommandOutput::success("only one"))])
            }
        }

        let snapshot = CollectorRegistry::linux()
            .collect(&ShortRunner, ServerId::new(), at())
            .await
            .expect("transport itself did not fail");
        // No panic, and the shortfall is recorded rather than silently ignored.
        assert_eq!(snapshot.outcomes.len(), CollectorRegistry::linux().len());
    }

    #[test]
    fn the_essential_registry_is_a_strict_subset() {
        let essential = CollectorRegistry::essential();
        let full = CollectorRegistry::linux();
        assert!(essential.len() < full.len());

        let full_ids: Vec<String> = full.iter().map(|c| c.id().to_string()).collect();
        for collector in essential.iter() {
            assert!(full_ids.contains(&collector.id().to_string()));
        }
    }

    #[test]
    fn the_essential_registry_skips_the_expensive_collectors() {
        let ids: Vec<String> = CollectorRegistry::essential()
            .iter()
            .map(|c| c.id().to_string())
            .collect();
        assert!(!ids.contains(&"docker".to_owned()));
        assert!(!ids.contains(&"process".to_owned()));
        assert!(!ids.contains(&"service".to_owned()));
    }

    #[test]
    fn collector_ids_are_unique() {
        let mut ids: Vec<String> = CollectorRegistry::linux()
            .iter()
            .map(|c| c.id().to_string())
            .collect();
        let count = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate collector id");
    }

    #[test]
    fn optional_collectors_declare_the_capability_they_need() {
        for collector in CollectorRegistry::linux().iter() {
            match collector.id().as_str() {
                "docker" => assert_eq!(collector.requires(), &[Capability::Docker]),
                "service" => assert_eq!(collector.requires(), &[Capability::Systemd]),
                "temperature" => assert_eq!(collector.requires(), &[Capability::ThermalSensors]),
                _ => assert!(!collector.requires().is_empty()),
            }
        }
    }
}
