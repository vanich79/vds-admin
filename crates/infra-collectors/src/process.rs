//! Top processes from `ps`.

use crate::parse::split_n;
use vds_domain::ids::CollectorId;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};
use vds_domain::server::ProcessInfo;

/// How many processes to keep after sorting.
///
/// The interesting question is always "what is eating the machine", so a short list of
/// the heaviest consumers is worth far more than the full table — and it keeps the
/// payload small for a fleet of hundreds.
pub const TOP_N: usize = 15;

/// Reads the process table.
///
/// Sorting happens in Rust rather than via `ps --sort`, because `--sort` is a procps
/// extension that busybox and toybox `ps` do not implement, and those are what minimal
/// container images and embedded ARM systems ship.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessCollector;

impl Collector for ProcessCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("process")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::CoreUtils]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command::shell(
            "ps -eo pid,user,pcpu,pmem,rss,args 2>/dev/null",
        )]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let id = self.id();
        let output = outputs
            .first()
            .ok_or_else(|| CollectError::parse(&id, "no output for ps"))?
            .as_ref()
            .map_err(|e| CollectError::Transport(e.clone()))?;

        if !output.is_success() {
            return Err(CollectError::parse(
                &id,
                format!("ps failed: {}", output.stderr.trim()),
            ));
        }

        let processes = parse_ps(&output.stdout);
        if processes.is_empty() && !output.stdout.trim().is_empty() {
            return Err(CollectError::parse(&id, "ps produced no recognisable rows"));
        }
        Ok(CollectorOutput::Processes(top_by_cpu(processes, TOP_N)))
    }
}

/// Parses `ps -eo pid,user,pcpu,pmem,rss,args` output.
pub fn parse_ps(text: &str) -> Vec<ProcessInfo> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        // Drop the header without assuming it is the first line: some `ps`
        // implementations print a warning line first.
        .filter(|line| {
            let upper = line.trim_start().to_ascii_uppercase();
            !(upper.starts_with("PID ") || upper.starts_with("PID\t"))
        })
        .filter_map(|line| {
            // The command is the remainder, because arguments contain spaces.
            let (fields, command) = split_n(line, 5)?;
            let pid: u32 = fields.first()?.parse().ok()?;
            let user = fields.get(1).map(|u| (*u).to_owned());
            let cpu_percent: f64 = fields.get(2)?.parse().ok()?;
            let memory_percent: f64 = fields.get(3)?.parse().ok()?;
            // RSS is in kibibytes.
            let rss_bytes = fields
                .get(4)
                .and_then(|v| v.parse::<u64>().ok())
                .map(|kb| kb * 1_024);

            let command = command.trim();
            if command.is_empty() {
                return None;
            }
            if !cpu_percent.is_finite() || !memory_percent.is_finite() {
                return None;
            }

            Some(ProcessInfo {
                pid,
                user,
                command: command.to_owned(),
                cpu_percent,
                memory_percent,
                rss_bytes,
            })
        })
        .collect()
}

/// Keeps the `limit` heaviest processes by CPU, breaking ties by memory.
pub fn top_by_cpu(mut processes: Vec<ProcessInfo>, limit: usize) -> Vec<ProcessInfo> {
    processes.sort_by(|a, b| {
        b.cpu_percent
            .partial_cmp(&a.cpu_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.memory_percent
                    .partial_cmp(&a.memory_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    processes.truncate(limit);
    processes
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS_OUTPUT: &str = "\
  PID USER     %CPU %MEM   RSS COMMAND
    1 root      0.0  0.1  1234 /sbin/init splash
  842 www-data 12.5  3.4 45678 nginx: worker process
  843 postgres 45.2 15.1 987654 postgres: 14/main: checkpointer
 1024 root      0.3  0.5  8192 /usr/bin/dockerd -H fd:// --containerd=/run/containerd/containerd.sock
 2048 alice     1.1  0.2  4096 [kworker/0:1]";

    #[test]
    fn processes_are_parsed_with_full_command_lines() {
        let processes = parse_ps(PS_OUTPUT);
        assert_eq!(processes.len(), 5);
        let dockerd = processes
            .iter()
            .find(|p| p.pid == 1_024)
            .expect("dockerd present");
        assert_eq!(
            dockerd.command,
            "/usr/bin/dockerd -H fd:// --containerd=/run/containerd/containerd.sock"
        );
        assert_eq!(dockerd.user.as_deref(), Some("root"));
    }

    #[test]
    fn the_header_row_is_not_treated_as_a_process() {
        let processes = parse_ps(PS_OUTPUT);
        assert!(processes.iter().all(|p| p.command != "COMMAND"));
    }

    #[test]
    fn rss_is_converted_from_kibibytes() {
        let processes = parse_ps(PS_OUTPUT);
        let init = processes.iter().find(|p| p.pid == 1).expect("init present");
        assert_eq!(init.rss_bytes, Some(1_234 * 1_024));
    }

    #[test]
    fn sorting_puts_the_heaviest_consumer_first() {
        let processes = top_by_cpu(parse_ps(PS_OUTPUT), TOP_N);
        assert_eq!(processes.first().map(|p| p.pid), Some(843));
        assert_eq!(processes.last().map(|p| p.pid), Some(1));
    }

    #[test]
    fn only_the_top_n_survive() {
        let processes = top_by_cpu(parse_ps(PS_OUTPUT), 2);
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].pid, 843);
        assert_eq!(processes[1].pid, 842);
    }

    #[test]
    fn ties_on_cpu_are_broken_by_memory() {
        let text = "\
  PID USER  %CPU %MEM   RSS COMMAND
    1 root   5.0  1.0  1000 low-memory
    2 root   5.0  9.0  9000 high-memory";
        let processes = top_by_cpu(parse_ps(text), 2);
        assert_eq!(processes[0].pid, 2);
    }

    #[test]
    fn busybox_style_output_without_a_leading_header_still_parses() {
        let text = "    1 root      0.0  0.1  1234 /sbin/init";
        let processes = parse_ps(text);
        assert_eq!(processes.len(), 1);
        assert_eq!(processes[0].command, "/sbin/init");
    }

    #[test]
    fn malformed_rows_are_skipped_without_losing_the_rest() {
        let text = "\
  PID USER  %CPU %MEM   RSS COMMAND
    1 root   0.0  0.1  1234 /sbin/init
garbage
  842 www    1.0  1.0  4096 nginx";
        let processes = parse_ps(text);
        assert_eq!(processes.len(), 2);
    }

    #[test]
    fn a_row_with_no_command_is_dropped() {
        let text = "    1 root      0.0  0.1  1234    ";
        assert!(parse_ps(text).is_empty());
    }

    #[test]
    fn a_failed_ps_is_an_error_not_an_empty_process_list() {
        let err = ProcessCollector
            .parse(&[Ok(CommandOutput::failure(127, "ps: not found"))])
            .expect_err("must fail");
        assert!(matches!(err, CollectError::Parse { .. }));
    }

    #[test]
    fn empty_output_is_accepted_as_an_empty_list() {
        // A container with no visible processes is unusual but not a parse failure.
        let output = ProcessCollector
            .parse(&[Ok(CommandOutput::success(""))])
            .expect("parses");
        let CollectorOutput::Processes(processes) = output else {
            panic!("expected processes")
        };
        assert!(processes.is_empty());
    }
}
