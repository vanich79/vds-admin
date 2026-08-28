//! systemd units.

use crate::parse::split_n;
use vds_domain::ids::CollectorId;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};
use vds_domain::server::{ServiceInfo, ServiceState};

/// Collects systemd service state.
///
/// `--plain` strips the leading `●` bullet that systemd prints for failed units, which
/// would otherwise shift every column by one. `--no-legend` drops the trailing summary
/// text, and `--no-pager` stops systemd from trying to open `less` on a non-tty.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServiceCollector;

impl Collector for ServiceCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("service")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::Systemd]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command::shell(
            "systemctl list-units --type=service --all --no-legend --no-pager --plain 2>&1",
        )]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let id = self.id();
        let output = outputs
            .first()
            .ok_or_else(|| CollectError::parse(&id, "no output for systemctl"))?
            .as_ref()
            .map_err(|e| CollectError::Transport(e.clone()))?;

        if !output.is_success() {
            if looks_like_systemd_missing(&output.stdout)
                || looks_like_systemd_missing(&output.stderr)
            {
                return Err(CollectError::Unsupported {
                    capability: Capability::Systemd,
                });
            }
            return Err(CollectError::parse(
                &id,
                format!(
                    "systemctl failed: {}",
                    first_line(&output.stdout, &output.stderr)
                ),
            ));
        }

        Ok(CollectorOutput::Services(parse_list_units(&output.stdout)))
    }
}

/// Whether the output means this host does not use systemd.
///
/// Alpine (OpenRC), busybox images and many containers have no systemd at all — that is
/// a normal configuration, not a fault.
fn looks_like_systemd_missing(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("not found")
        || lower.contains("no such file or directory")
        || lower.contains("failed to connect to bus")
        || lower.contains("system has not been booted with systemd")
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

/// Parses `systemctl list-units --type=service --all --no-legend --plain`.
///
/// Columns: `UNIT LOAD ACTIVE SUB DESCRIPTION`, where the description runs to the end of
/// the line.
pub fn parse_list_units(text: &str) -> Vec<ServiceInfo> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        // Defensive: strip the bullet in case `--plain` was not honoured by an older
        // systemd.
        .map(|line| line.trim_start_matches(['●', '*', '\u{25CF}']).trim_start())
        .filter_map(|line| {
            let (fields, description) = split_n(line, 4)?;
            let name = fields.first()?;
            if !name.ends_with(".service") {
                return None;
            }
            let load = fields.get(1)?;
            let active = fields.get(2)?;
            let sub = fields.get(3)?;

            // A unit whose file could not be found reports LOAD=not-found; systemd then
            // shows ACTIVE=inactive, which would look like a merely stopped service.
            let state = if load.eq_ignore_ascii_case("not-found") {
                ServiceState::Unknown
            } else {
                ServiceState::parse(active)
            };

            let description = description.trim();
            Some(ServiceInfo {
                name: (*name).to_owned(),
                state,
                sub_state: Some((*sub).to_owned()),
                description: if description.is_empty() {
                    None
                } else {
                    Some(description.to_owned())
                },
                // `list-units` does not report enablement; `list-unit-files` would, at
                // the cost of another round trip. Left unknown rather than guessed.
                enabled: None,
            })
        })
        .collect()
}

/// Units in the `failed` state.
pub fn failed(services: &[ServiceInfo]) -> Vec<&ServiceInfo> {
    services
        .iter()
        .filter(|s| s.state == ServiceState::Failed)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::Status;

    const UNITS: &str = "\
docker.service                loaded active   running Docker Application Container Engine
nginx.service                 loaded active   running A high performance web server and a reverse proxy server
postgresql.service            loaded active   exited  PostgreSQL RDBMS
redis-server.service          loaded failed   failed  Advanced key-value store
ssh.service                   loaded active   running OpenBSD Secure Shell server
systemd-networkd.service      loaded inactive dead    Network Configuration
ghost.service                 not-found inactive dead ghost.service
";

    fn collect(text: &str) -> Vec<ServiceInfo> {
        let output = ServiceCollector
            .parse(&[Ok(CommandOutput::success(text))])
            .expect("parses");
        let CollectorOutput::Services(services) = output else {
            panic!("expected services")
        };
        services
    }

    #[test]
    fn services_are_parsed_with_their_descriptions() {
        let services = collect(UNITS);
        let nginx = services
            .iter()
            .find(|s| s.name == "nginx.service")
            .expect("nginx present");
        assert_eq!(nginx.state, ServiceState::Active);
        assert_eq!(nginx.sub_state.as_deref(), Some("running"));
        assert_eq!(
            nginx.description.as_deref(),
            Some("A high performance web server and a reverse proxy server")
        );
    }

    #[test]
    fn a_failed_unit_is_critical() {
        let services = collect(UNITS);
        let redis = services
            .iter()
            .find(|s| s.name == "redis-server.service")
            .expect("redis present");
        assert_eq!(redis.state, ServiceState::Failed);
        assert_eq!(redis.state.status(), Status::Critical);
        assert_eq!(failed(&services).len(), 1);
    }

    #[test]
    fn a_oneshot_service_that_exited_is_still_active() {
        // ACTIVE=active with SUB=exited is normal for oneshot units; calling it stopped
        // would produce a permanent false warning.
        let services = collect(UNITS);
        let postgres = services
            .iter()
            .find(|s| s.name == "postgresql.service")
            .expect("present");
        assert_eq!(postgres.state, ServiceState::Active);
        assert_eq!(postgres.sub_state.as_deref(), Some("exited"));
    }

    #[test]
    fn a_unit_whose_file_is_missing_is_unknown_not_merely_stopped() {
        let services = collect(UNITS);
        let ghost = services
            .iter()
            .find(|s| s.name == "ghost.service")
            .expect("present");
        assert_eq!(ghost.state, ServiceState::Unknown);
    }

    #[test]
    fn the_failed_bullet_does_not_shift_the_columns() {
        // Some systemd versions print ● even with --plain.
        let bulleted = "● redis.service loaded failed failed Advanced key-value store";
        let services = collect(bulleted);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "redis.service");
        assert_eq!(services[0].state, ServiceState::Failed);
    }

    #[test]
    fn non_service_units_are_ignored() {
        let mixed = "\
nginx.service       loaded active running Web server
sys-devices.device  loaded active plugged Some device
tmp.mount           loaded active mounted Temporary Directory";
        let services = collect(mixed);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "nginx.service");
    }

    #[test]
    fn a_host_without_systemd_reports_unsupported_not_a_failure() {
        let err = ServiceCollector
            .parse(&[Ok(CommandOutput::failure(127, "sh: systemctl: not found"))])
            .expect_err("must fail");
        assert_eq!(
            err,
            CollectError::Unsupported {
                capability: Capability::Systemd
            }
        );
        assert!(!err.affects_server_health());
    }

    #[test]
    fn a_container_without_a_running_systemd_bus_is_also_unsupported() {
        let err = ServiceCollector
            .parse(&[Ok(CommandOutput::failure(
                1,
                "System has not been booted with systemd as init system (PID 1). Can't operate.",
            ))])
            .expect_err("must fail");
        assert_eq!(
            err,
            CollectError::Unsupported {
                capability: Capability::Systemd
            }
        );
    }

    #[test]
    fn a_genuinely_broken_systemctl_is_a_real_error() {
        let err = ServiceCollector
            .parse(&[Ok(CommandOutput::failure(
                1,
                "Too many levels of symbolic links",
            ))])
            .expect_err("must fail");
        assert!(matches!(err, CollectError::Parse { .. }));
        assert!(err.affects_server_health());
    }

    #[test]
    fn truncated_rows_are_skipped() {
        let text = "nginx.service loaded\nredis.service loaded active running Redis";
        let services = collect(text);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "redis.service");
    }

    #[test]
    fn a_unit_with_no_description_is_reported_without_one() {
        let text = "bare.service loaded active running ";
        let services = collect(text);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].description, None);
    }
}
