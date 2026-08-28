//! CPU utilisation from two samples of `/proc/stat`.

use crate::parse::clamp_percent;
use vds_domain::ids::CollectorId;
use vds_domain::metrics::MetricValue;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};
use vds_domain::server::CpuUsage;

/// Gap between the two `/proc/stat` reads.
///
/// Long enough that jiffy granularity does not dominate, short enough that it barely
/// affects a collection cycle. It is spent asleep on the monitored host, and cycles for
/// different servers overlap, so it does not serialise the fleet.
const SAMPLE_DELAY_MS: u64 = 500;

/// Reads `/proc/stat` twice and reports the busy fraction over the interval.
///
/// The delta approach is the only correct one: `/proc/stat` holds cumulative counters
/// since boot, so a single read tells you the average CPU usage since the machine
/// started, which is never what anyone means by "CPU usage".
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuCollector;

impl Collector for CpuCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("cpu")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::ProcFs]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command::sample_twice("/proc/stat", SAMPLE_DELAY_MS)]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let id = self.id();
        let output = outputs
            .first()
            .ok_or_else(|| CollectError::parse(&id, "no output for /proc/stat"))?
            .as_ref()
            .map_err(|e| CollectError::Transport(e.clone()))?;

        if !output.is_success() {
            return Err(CollectError::parse(
                &id,
                format!("reading /proc/stat failed: {}", output.stderr.trim()),
            ));
        }

        let (first, second) = output
            .split_samples()
            .ok_or_else(|| CollectError::parse(&id, "second /proc/stat sample is missing"))?;

        let before = CpuTimes::parse_aggregate(first)
            .ok_or_else(|| CollectError::parse(&id, "no aggregate cpu line in first sample"))?;
        let after = CpuTimes::parse_aggregate(second)
            .ok_or_else(|| CollectError::parse(&id, "no aggregate cpu line in second sample"))?;

        Ok(CollectorOutput::Cpu(usage_between(
            before,
            after,
            count_cores(first),
        )))
    }
}

/// The jiffy counters from one `cpu` line of `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    /// Parses the aggregate `cpu ` line out of a `/proc/stat` fragment.
    pub fn parse_aggregate(text: &str) -> Option<CpuTimes> {
        text.lines()
            .map(str::trim_start)
            // "cpu " with the space excludes the per-core "cpu0", "cpu1" lines.
            .find(|line| line.starts_with("cpu "))
            .and_then(CpuTimes::parse_line)
    }

    /// Parses one `cpu*` line.
    ///
    /// Fields after the label are, in order: user, nice, system, idle, iowait, irq,
    /// softirq, steal, guest, guest_nice. Older kernels stop earlier, so every field
    /// past `idle` defaults to zero rather than failing the parse.
    pub fn parse_line(line: &str) -> Option<CpuTimes> {
        let mut fields = line.split_whitespace();
        let label = fields.next()?;
        if !label.starts_with("cpu") {
            return None;
        }
        let mut values = [0_u64; 8];
        for (index, slot) in values.iter_mut().enumerate() {
            match fields.next() {
                Some(raw) => *slot = raw.parse().ok()?,
                // The first four fields have been mandatory since Linux 2.0; anything
                // shorter is not a `/proc/stat` line we understand.
                None if index < 4 => return None,
                None => break,
            }
        }
        Some(CpuTimes {
            user: values[0],
            nice: values[1],
            system: values[2],
            idle: values[3],
            iowait: values[4],
            irq: values[5],
            softirq: values[6],
            steal: values[7],
        })
    }

    /// All counted time.
    pub fn total(&self) -> u64 {
        self.user
            .saturating_add(self.nice)
            .saturating_add(self.system)
            .saturating_add(self.idle)
            .saturating_add(self.iowait)
            .saturating_add(self.irq)
            .saturating_add(self.softirq)
            .saturating_add(self.steal)
    }

    /// Time the CPU was not doing useful work.
    ///
    /// `iowait` counts as idle here — the CPU really was available — but it is also
    /// reported separately, because a machine pinned at 90% iowait has a very different
    /// problem from one pinned at 90% user.
    pub fn idle_total(&self) -> u64 {
        self.idle.saturating_add(self.iowait)
    }
}

/// Computes utilisation between two cumulative readings.
pub fn usage_between(before: CpuTimes, after: CpuTimes, cores: Option<u32>) -> CpuUsage {
    let total_delta = after.total().saturating_sub(before.total());
    if total_delta == 0 {
        // The counters did not move: either the interval was too short to register a
        // jiffy, or the samples are identical. Either way we measured nothing, and
        // reporting 0% would be a lie about an idle machine.
        return CpuUsage {
            cores,
            ..Default::default()
        };
    }

    let total = total_delta as f64;
    let fraction = |now: u64, then: u64| {
        clamp_percent(now.saturating_sub(then) as f64 / total * 100.0)
            .map_or(MetricValue::NotAvailable, MetricValue::available)
    };

    let idle_delta = after.idle_total().saturating_sub(before.idle_total());
    let busy = clamp_percent((total_delta.saturating_sub(idle_delta)) as f64 / total * 100.0);

    CpuUsage {
        total_percent: busy.map_or(MetricValue::NotAvailable, MetricValue::available),
        user_percent: fraction(
            after.user.saturating_add(after.nice),
            before.user.saturating_add(before.nice),
        ),
        system_percent: fraction(after.system, before.system),
        iowait_percent: fraction(after.iowait, before.iowait),
        cores,
    }
}

/// Counts `cpuN` lines, which is the core count as the kernel sees it.
pub fn count_cores(text: &str) -> Option<u32> {
    let count = text
        .lines()
        .map(str::trim_start)
        .filter(|line| {
            line.starts_with("cpu") && line.as_bytes().get(3).is_some_and(u8::is_ascii_digit)
        })
        .count();
    if count == 0 {
        None
    } else {
        u32::try_from(count).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::ports::SAMPLE_SEPARATOR;

    const SAMPLE_1: &str = "\
cpu  100 10 50 1000 20 0 5 0 0 0
cpu0 50 5 25 500 10 0 2 0 0 0
cpu1 50 5 25 500 10 0 3 0 0 0
intr 12345
ctxt 67890";

    // Exactly 100 jiffies of extra busy time and 100 of extra idle: 50% busy.
    const SAMPLE_2: &str = "\
cpu  150 10 100 1080 40 0 5 0 0 0
cpu0 75 5 50 540 20 0 2 0 0 0
cpu1 75 5 50 540 20 0 3 0 0 0
intr 12999
ctxt 68999";

    fn run(stdout: String) -> Result<CollectorOutput, CollectError> {
        CpuCollector.parse(&[Ok(CommandOutput::success(stdout))])
    }

    fn joined(first: &str, second: &str) -> String {
        format!("{first}\n{SAMPLE_SEPARATOR}\n{second}")
    }

    #[test]
    fn the_collector_asks_for_two_samples_of_proc_stat() {
        let commands = CpuCollector.commands();
        assert_eq!(
            commands,
            vec![Command::sample_twice("/proc/stat", SAMPLE_DELAY_MS)]
        );
    }

    #[test]
    fn usage_is_the_delta_not_the_since_boot_average() {
        let output = run(joined(SAMPLE_1, SAMPLE_2)).expect("parses");
        let CollectorOutput::Cpu(cpu) = output else {
            panic!("expected cpu output")
        };

        // busy delta = (150-100)+(100-50) = 100; total delta = 200 ⇒ 50%.
        assert_eq!(cpu.total_percent, MetricValue::Available(50.0));
        // Since-boot busy would be ~15%, which is the wrong answer.
        assert_eq!(cpu.cores, Some(2));
    }

    #[test]
    fn user_system_and_iowait_are_broken_out() {
        let output = run(joined(SAMPLE_1, SAMPLE_2)).expect("parses");
        let CollectorOutput::Cpu(cpu) = output else {
            panic!("expected cpu output")
        };

        assert_eq!(cpu.user_percent, MetricValue::Available(25.0));
        assert_eq!(cpu.system_percent, MetricValue::Available(25.0));
        assert_eq!(cpu.iowait_percent, MetricValue::Available(10.0));
    }

    #[test]
    fn identical_samples_report_nothing_rather_than_zero_percent() {
        // Zero would claim the machine is idle; we simply did not measure anything.
        let output = run(joined(SAMPLE_1, SAMPLE_1)).expect("parses");
        let CollectorOutput::Cpu(cpu) = output else {
            panic!("expected cpu output")
        };
        assert_eq!(cpu.total_percent, MetricValue::NotAvailable);
        assert_eq!(cpu.cores, Some(2));
    }

    #[test]
    fn a_fully_busy_cpu_reads_as_one_hundred_percent() {
        let busy = "cpu  200 10 50 1000 20 0 5 0 0 0";
        let output = run(joined(SAMPLE_1, busy)).expect("parses");
        let CollectorOutput::Cpu(cpu) = output else {
            panic!("expected cpu output")
        };
        assert_eq!(cpu.total_percent, MetricValue::Available(100.0));
    }

    #[test]
    fn a_missing_second_sample_is_an_error_not_a_zero() {
        let err = run("cpu  1 2 3 4 5 6 7 8".to_owned()).expect_err("must fail");
        assert!(matches!(err, CollectError::Parse { .. }), "got {err:?}");
    }

    #[test]
    fn older_kernels_without_steal_still_parse() {
        // Linux 2.4-era /proc/stat: user, nice, system, idle only.
        let old_1 = "cpu  100 10 50 1000";
        let old_2 = "cpu  150 10 100 1100";
        let output = run(joined(old_1, old_2)).expect("parses");
        let CollectorOutput::Cpu(cpu) = output else {
            panic!("expected cpu output")
        };
        assert_eq!(cpu.total_percent, MetricValue::Available(50.0));
    }

    #[test]
    fn a_truncated_cpu_line_is_rejected() {
        assert_eq!(CpuTimes::parse_line("cpu  1 2"), None);
        assert_eq!(CpuTimes::parse_line("intr 1 2 3 4 5"), None);
    }

    #[test]
    fn per_core_lines_do_not_masquerade_as_the_aggregate() {
        // "cpu0" must not be picked up when the aggregate line is absent.
        assert_eq!(CpuTimes::parse_aggregate("cpu0 1 2 3 4 5 6 7 8"), None);
    }

    #[test]
    fn core_counting_ignores_the_aggregate_line() {
        assert_eq!(count_cores(SAMPLE_1), Some(2));
        assert_eq!(count_cores("cpu  1 2 3 4"), None);
        assert_eq!(count_cores("intr 1"), None);
    }

    #[test]
    fn transport_failures_propagate_as_transport_errors() {
        let err = CpuCollector
            .parse(&[Err(TransportError::Timeout { seconds: 5 })])
            .expect_err("must fail");
        assert!(matches!(err, CollectError::Transport(_)));
        assert!(err.affects_server_health());
    }

    #[test]
    fn a_nonzero_exit_code_is_reported_as_a_parse_failure() {
        let err = CpuCollector
            .parse(&[Ok(CommandOutput::failure(
                1,
                "cat: /proc/stat: No such file",
            ))])
            .expect_err("must fail");
        assert!(matches!(err, CollectError::Parse { .. }));
    }

    #[test]
    fn counters_that_appear_to_go_backwards_do_not_underflow() {
        // Can happen across a CPU hotplug event. Saturating arithmetic keeps it sane.
        let output = run(joined(SAMPLE_2, SAMPLE_1)).expect("parses");
        let CollectorOutput::Cpu(cpu) = output else {
            panic!("expected cpu output")
        };
        // Total went backwards ⇒ delta is zero ⇒ nothing measured.
        assert_eq!(cpu.total_percent, MetricValue::NotAvailable);
    }
}
