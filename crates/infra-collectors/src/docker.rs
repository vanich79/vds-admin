//! Docker containers, via the `docker` CLI's JSON output.

use crate::parse::{human_bytes, percent, slash_pair};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use vds_domain::ids::CollectorId;
use vds_domain::metrics::MetricValue;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};
use vds_domain::server::{ContainerHealth, ContainerInfo, ContainerState};

/// Collects Docker container state and resource usage.
///
/// Three commands rather than one, because Docker splits the information:
/// `ps` has identity and state, `stats` has live CPU/memory, and only `inspect` knows
/// the restart count. Each degrades independently — losing `stats` costs the CPU column
/// but still shows which containers are down, which is the more important signal.
#[derive(Debug, Clone, Copy, Default)]
pub struct DockerCollector;

const PS: usize = 0;
const STATS: usize = 1;
const INSPECT: usize = 2;

impl Collector for DockerCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("docker")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::Docker]
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            Command::shell("docker ps -a --no-trunc --format '{{json .}}' 2>&1"),
            // `--no-stream` takes a single sample instead of streaming forever.
            Command::shell("docker stats --no-stream --format '{{json .}}' 2>/dev/null"),
            // `xargs -r` avoids running `inspect` with no arguments when there are no
            // containers, which would be an error rather than an empty result.
            Command::shell(
                "docker ps -aq --no-trunc 2>/dev/null | xargs -r docker inspect \
                 --format '{{.Id}}|{{.RestartCount}}|{{.State.StartedAt}}' 2>/dev/null",
            ),
        ]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let id = self.id();
        let ps = outputs
            .get(PS)
            .ok_or_else(|| CollectError::parse(&id, "no output for docker ps"))?
            .as_ref()
            .map_err(|e| CollectError::Transport(e.clone()))?;

        if !ps.is_success() {
            // Distinguish "Docker is not installed" — a normal state for most servers —
            // from "Docker is installed but broken", which is worth surfacing.
            if looks_like_docker_missing(&ps.stdout) || looks_like_docker_missing(&ps.stderr) {
                return Err(CollectError::Unsupported {
                    capability: Capability::Docker,
                });
            }
            return Err(CollectError::parse(
                &id,
                format!("docker ps failed: {}", first_line(&ps.stdout, &ps.stderr)),
            ));
        }

        let mut containers = parse_ps_json(&ps.stdout);

        if let Some(Ok(stats)) = outputs.get(STATS)
            && stats.is_success()
        {
            apply_stats(&mut containers, &parse_stats_json(&stats.stdout));
        }

        if let Some(Ok(inspect)) = outputs.get(INSPECT)
            && inspect.is_success()
        {
            apply_inspect(&mut containers, &parse_inspect(&inspect.stdout));
        }

        Ok(CollectorOutput::Containers(containers))
    }
}

/// Whether the output means Docker simply is not there.
fn looks_like_docker_missing(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("not found")
        || lower.contains("command not found")
        || lower.contains("no such file or directory")
        || lower.contains("docker: not found")
}

fn first_line(stdout: &str, stderr: &str) -> String {
    let source = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    source
        .lines()
        .next()
        .unwrap_or("unknown error")
        .trim()
        .to_owned()
}

/// One line of `docker ps --format '{{json .}}'`.
#[derive(Debug, Deserialize)]
struct PsRow {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "Names", default)]
    names: String,
    #[serde(rename = "Image", default)]
    image: String,
    #[serde(rename = "State", default)]
    state: String,
    #[serde(rename = "Status", default)]
    status: String,
}

/// Parses `docker ps -a --format '{{json .}}'`, one JSON object per line.
pub fn parse_ps_json(text: &str) -> Vec<ContainerInfo> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<PsRow>(line).ok())
        .map(|row| {
            // `docker ps` joins multiple names with commas; the first is canonical.
            let name = row
                .names
                .split(',')
                .next()
                .unwrap_or(&row.names)
                .trim()
                .trim_start_matches('/')
                .to_owned();

            // Older Docker releases have no State field and only the Status text.
            let state = if row.state.is_empty() {
                state_from_status(&row.status)
            } else {
                ContainerState::parse(&row.state)
            };

            ContainerInfo {
                id: row.id.clone(),
                name: if name.is_empty() {
                    row.id.clone()
                } else {
                    name
                },
                image: row.image,
                state,
                health: health_from_status(&row.status),
                status_text: row.status,
                cpu_percent: MetricValue::NotAvailable,
                memory_used_bytes: None,
                memory_limit_bytes: None,
                restart_count: None,
                started_at: None,
            }
        })
        .collect()
}

/// Infers lifecycle state from Docker's human-readable status text.
pub fn state_from_status(status: &str) -> ContainerState {
    let lower = status.to_ascii_lowercase();
    if lower.starts_with("up") {
        if lower.contains("paused") {
            ContainerState::Paused
        } else {
            ContainerState::Running
        }
    } else if lower.starts_with("restarting") {
        ContainerState::Restarting
    } else if lower.starts_with("exited") {
        ContainerState::Exited
    } else if lower.starts_with("created") {
        ContainerState::Created
    } else if lower.starts_with("dead") {
        ContainerState::Dead
    } else if lower.starts_with("removal") {
        ContainerState::Removing
    } else {
        ContainerState::Unknown
    }
}

/// Extracts the health-check verdict, which Docker only puts in the status text.
pub fn health_from_status(status: &str) -> ContainerHealth {
    let lower = status.to_ascii_lowercase();
    if lower.contains("(unhealthy)") {
        ContainerHealth::Unhealthy
    } else if lower.contains("(health: starting)") {
        ContainerHealth::Starting
    } else if lower.contains("(healthy)") {
        ContainerHealth::Healthy
    } else {
        ContainerHealth::None
    }
}

/// Live resource usage for one container.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ContainerStats {
    pub cpu_percent: Option<f64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_limit_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StatsRow {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "CPUPerc", default)]
    cpu_perc: String,
    #[serde(rename = "MemUsage", default)]
    mem_usage: String,
}

/// Parses `docker stats --no-stream --format '{{json .}}'`.
///
/// Keys the result by both ID and name, because `docker stats` may truncate IDs while
/// `docker ps --no-trunc` does not.
pub fn parse_stats_json(text: &str) -> HashMap<String, ContainerStats> {
    let mut stats = HashMap::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(row) = serde_json::from_str::<StatsRow>(line) else {
            continue;
        };
        let (used, limit) = match slash_pair(&row.mem_usage) {
            Some((used, limit)) => (human_bytes(used), human_bytes(limit)),
            None => (None, None),
        };
        let entry = ContainerStats {
            cpu_percent: percent(&row.cpu_perc),
            memory_used_bytes: used,
            memory_limit_bytes: limit,
        };
        if !row.id.is_empty() {
            stats.insert(row.id.clone(), entry);
        }
        if !row.name.is_empty() {
            stats.insert(row.name.trim_start_matches('/').to_owned(), entry);
        }
    }
    stats
}

/// Merges live stats into the container list.
pub fn apply_stats(containers: &mut [ContainerInfo], stats: &HashMap<String, ContainerStats>) {
    for container in containers.iter_mut() {
        // Try the full ID, then a truncated form, then the name.
        let entry = stats
            .get(&container.id)
            .or_else(|| container.id.get(..12).and_then(|short| stats.get(short)))
            .or_else(|| stats.get(&container.name));
        let Some(entry) = entry else { continue };

        container.cpu_percent = entry
            .cpu_percent
            .map_or(MetricValue::NotAvailable, MetricValue::available);
        container.memory_used_bytes = entry.memory_used_bytes;
        container.memory_limit_bytes = entry.memory_limit_bytes;
    }
}

/// Restart count and start time for one container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InspectRow {
    pub restart_count: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
}

/// Parses the pipe-separated `docker inspect` output this collector requests.
pub fn parse_inspect(text: &str) -> HashMap<String, InspectRow> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let mut parts = line.split('|');
            let id = parts.next()?.trim().to_owned();
            if id.is_empty() {
                return None;
            }
            let restart_count = parts.next().and_then(|v| v.trim().parse::<u32>().ok());
            let started_at = parts.next().and_then(|v| {
                DateTime::parse_from_rfc3339(v.trim())
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            });
            Some((
                id,
                InspectRow {
                    restart_count,
                    started_at,
                },
            ))
        })
        .collect()
}

/// Merges inspect data into the container list.
pub fn apply_inspect(containers: &mut [ContainerInfo], rows: &HashMap<String, InspectRow>) {
    for container in containers.iter_mut() {
        let Some(row) = rows.get(&container.id) else {
            continue;
        };
        container.restart_count = row.restart_count;
        container.started_at = row.started_at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::Status;

    const PS_JSON: &str = r#"
{"Command":"\"/docker-entrypoint.…\"","CreatedAt":"2026-08-01 10:00:00 +0000 UTC","ID":"a1b2c3d4e5f60000000000000000000000000000000000000000000000000000","Image":"nginx:1.25","Names":"web","State":"running","Status":"Up 3 days (healthy)"}
{"Command":"\"postgres\"","CreatedAt":"2026-08-01 10:00:00 +0000 UTC","ID":"b2c3d4e5f6070000000000000000000000000000000000000000000000000000","Image":"postgres:16","Names":"db","State":"running","Status":"Up 3 days (unhealthy)"}
{"Command":"\"worker\"","CreatedAt":"2026-08-02 10:00:00 +0000 UTC","ID":"c3d4e5f6a1b20000000000000000000000000000000000000000000000000000","Image":"myapp:latest","Names":"worker","State":"exited","Status":"Exited (1) 2 hours ago"}
{"Command":"\"flap\"","CreatedAt":"2026-08-02 10:00:00 +0000 UTC","ID":"d4e5f6a1b2c30000000000000000000000000000000000000000000000000000","Image":"flaky:latest","Names":"flapper","State":"restarting","Status":"Restarting (1) 5 seconds ago"}
"#;

    const STATS_JSON: &str = r#"
{"BlockIO":"0B / 0B","CPUPerc":"1.53%","Container":"a1b2c3d4e5f6","ID":"a1b2c3d4e5f60000000000000000000000000000000000000000000000000000","MemPerc":"2.50%","MemUsage":"104.9MiB / 4GiB","Name":"web","NetIO":"1.2MB / 3.4MB","PIDs":"5"}
{"BlockIO":"0B / 0B","CPUPerc":"25.00%","Container":"b2c3d4e5f607","ID":"b2c3d4e5f6070000000000000000000000000000000000000000000000000000","MemPerc":"10.00%","MemUsage":"1.5GiB / 4GiB","Name":"db","NetIO":"1.2MB / 3.4MB","PIDs":"20"}
"#;

    const INSPECT: &str = "\
a1b2c3d4e5f60000000000000000000000000000000000000000000000000000|0|2026-08-23T10:00:00.123456789Z
d4e5f6a1b2c30000000000000000000000000000000000000000000000000000|47|2026-08-26T12:00:00Z";

    fn collect() -> Vec<ContainerInfo> {
        let outputs = vec![
            Ok(CommandOutput::success(PS_JSON)),
            Ok(CommandOutput::success(STATS_JSON)),
            Ok(CommandOutput::success(INSPECT)),
        ];
        let output = DockerCollector.parse(&outputs).expect("parses");
        let CollectorOutput::Containers(containers) = output else {
            panic!("expected containers")
        };
        containers
    }

    #[test]
    fn containers_are_parsed_with_identity_and_state() {
        let containers = collect();
        assert_eq!(containers.len(), 4);
        let web = containers
            .iter()
            .find(|c| c.name == "web")
            .expect("web present");
        assert_eq!(web.image, "nginx:1.25");
        assert_eq!(web.state, ContainerState::Running);
        assert_eq!(web.status_text, "Up 3 days (healthy)");
    }

    #[test]
    fn an_unhealthy_running_container_is_critical_not_healthy() {
        // This is the whole point of parsing the health suffix: the container is "Up",
        // so state alone would call it fine.
        let containers = collect();
        let db = containers
            .iter()
            .find(|c| c.name == "db")
            .expect("db present");
        assert_eq!(db.state, ContainerState::Running);
        assert_eq!(db.health, ContainerHealth::Unhealthy);
        assert_eq!(db.status(), Status::Critical);
    }

    #[test]
    fn stopped_and_restarting_containers_are_distinguished() {
        let containers = collect();
        let worker = containers
            .iter()
            .find(|c| c.name == "worker")
            .expect("worker present");
        assert_eq!(worker.state, ContainerState::Exited);
        assert_eq!(worker.status(), Status::Warning);

        let flapper = containers
            .iter()
            .find(|c| c.name == "flapper")
            .expect("flapper present");
        assert_eq!(flapper.state, ContainerState::Restarting);
        assert_eq!(flapper.status(), Status::Critical);
    }

    #[test]
    fn stats_are_merged_by_container_id() {
        let containers = collect();
        let db = containers
            .iter()
            .find(|c| c.name == "db")
            .expect("db present");
        assert_eq!(db.cpu_percent, MetricValue::Available(25.0));
        assert_eq!(db.memory_used_bytes, Some(1_610_612_736));
        assert_eq!(db.memory_limit_bytes, Some(4 * 1_024 * 1_024 * 1_024));
    }

    #[test]
    fn containers_without_stats_report_usage_as_unavailable_not_zero() {
        // Stopped containers do not appear in `docker stats` at all.
        let containers = collect();
        let worker = containers
            .iter()
            .find(|c| c.name == "worker")
            .expect("worker present");
        assert_eq!(worker.cpu_percent, MetricValue::NotAvailable);
        assert_eq!(worker.memory_used_bytes, None);
    }

    #[test]
    fn restart_counts_come_from_inspect() {
        let containers = collect();
        let flapper = containers
            .iter()
            .find(|c| c.name == "flapper")
            .expect("flapper present");
        assert_eq!(flapper.restart_count, Some(47));
        assert!(flapper.started_at.is_some());

        let web = containers
            .iter()
            .find(|c| c.name == "web")
            .expect("web present");
        assert_eq!(web.restart_count, Some(0));
    }

    #[test]
    fn a_host_without_docker_reports_unsupported_not_a_failure() {
        // A server with no Docker is perfectly healthy; this must not degrade it.
        let err = DockerCollector
            .parse(&[Ok(CommandOutput::failure(127, "sh: 1: docker: not found"))])
            .expect_err("must fail");
        assert_eq!(
            err,
            CollectError::Unsupported {
                capability: Capability::Docker
            }
        );
        assert!(!err.affects_server_health());
    }

    #[test]
    fn a_broken_docker_daemon_is_a_real_error() {
        let err = DockerCollector
            .parse(&[Ok(CommandOutput::failure(
                1,
                "Cannot connect to the Docker daemon at unix:///var/run/docker.sock.",
            ))])
            .expect_err("must fail");
        assert!(matches!(err, CollectError::Parse { .. }));
        assert!(err.affects_server_health());
    }

    #[test]
    fn docker_present_with_no_containers_yields_an_empty_list_not_unsupported() {
        let output = DockerCollector
            .parse(&[
                Ok(CommandOutput::success("")),
                Ok(CommandOutput::success("")),
                Ok(CommandOutput::success("")),
            ])
            .expect("parses");
        let CollectorOutput::Containers(containers) = output else {
            panic!("expected containers")
        };
        assert!(containers.is_empty());
    }

    #[test]
    fn losing_stats_does_not_lose_the_container_list() {
        let outputs = vec![
            Ok(CommandOutput::success(PS_JSON)),
            Err(TransportError::Timeout { seconds: 5 }),
            Err(TransportError::Timeout { seconds: 5 }),
        ];
        let output = DockerCollector.parse(&outputs).expect("parses");
        let CollectorOutput::Containers(containers) = output else {
            panic!("expected containers")
        };
        assert_eq!(containers.len(), 4);
        assert_eq!(containers[0].cpu_percent, MetricValue::NotAvailable);
    }

    #[test]
    fn malformed_json_lines_are_skipped() {
        let text = format!("{PS_JSON}\nnot json at all\n{{\"broken\": ");
        let containers = parse_ps_json(&text);
        assert_eq!(containers.len(), 4);
    }

    #[test]
    fn health_is_read_out_of_the_status_text() {
        assert_eq!(
            health_from_status("Up 3 days (healthy)"),
            ContainerHealth::Healthy
        );
        assert_eq!(
            health_from_status("Up 3 days (unhealthy)"),
            ContainerHealth::Unhealthy
        );
        assert_eq!(
            health_from_status("Up 5 seconds (health: starting)"),
            ContainerHealth::Starting
        );
        assert_eq!(health_from_status("Up 3 days"), ContainerHealth::None);
    }

    #[test]
    fn older_docker_without_a_state_field_falls_back_to_the_status_text() {
        let legacy = r#"{"ID":"abc","Names":"legacy","Image":"old","Status":"Up 2 hours"}"#;
        let containers = parse_ps_json(legacy);
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].state, ContainerState::Running);
    }

    #[test]
    fn container_names_are_stripped_of_docker_decoration() {
        let row = r#"{"ID":"abc","Names":"/web,/web_alias","Image":"nginx","State":"running","Status":"Up"}"#;
        let containers = parse_ps_json(row);
        assert_eq!(containers[0].name, "web");
    }

    #[test]
    fn stats_match_even_when_docker_truncates_the_id() {
        let mut containers = parse_ps_json(PS_JSON);
        let truncated =
            r#"{"ID":"a1b2c3d4e5f6","Name":"other","CPUPerc":"7.00%","MemUsage":"1MiB / 2MiB"}"#;
        apply_stats(&mut containers, &parse_stats_json(truncated));
        let web = containers
            .iter()
            .find(|c| c.name == "web")
            .expect("web present");
        assert_eq!(web.cpu_percent, MetricValue::Available(7.0));
    }
}
