//! Memory and swap from `/proc/meminfo`.

use crate::parse::key_value;
use vds_domain::ids::CollectorId;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};
use vds_domain::server::MemoryUsage;

/// Reads `/proc/meminfo`.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryCollector;

impl Collector for MemoryCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("memory")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::ProcFs]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command::read("/proc/meminfo")]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let id = self.id();
        let output = outputs
            .first()
            .ok_or_else(|| CollectError::parse(&id, "no output for /proc/meminfo"))?
            .as_ref()
            .map_err(|e| CollectError::Transport(e.clone()))?;

        if !output.is_success() {
            return Err(CollectError::parse(
                &id,
                format!("reading /proc/meminfo failed: {}", output.stderr.trim()),
            ));
        }

        parse_meminfo(&output.stdout)
            .map(CollectorOutput::Memory)
            .ok_or_else(|| CollectError::parse(&id, "MemTotal is missing from /proc/meminfo"))
    }
}

/// Parses `/proc/meminfo` into a [`MemoryUsage`].
///
/// Returns `None` only when `MemTotal` is absent, which means the input is not
/// `/proc/meminfo` at all.
pub fn parse_meminfo(text: &str) -> Option<MemoryUsage> {
    let mut total = None;
    let mut free = None;
    let mut available = None;
    let mut buffers = None;
    let mut cached = None;
    let mut reclaimable = None;
    let mut swap_total = None;
    let mut swap_free = None;

    for line in text.lines() {
        let Some((key, value)) = key_value(line, ':') else {
            continue;
        };
        // Values are "<number> kB"; the unit has been kB since forever, but taking the
        // first field rather than trusting a fixed offset keeps it robust.
        let Some(kb) = value
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
        else {
            continue;
        };
        let bytes = kb.saturating_mul(1_024);
        match key {
            "MemTotal" => total = Some(bytes),
            "MemFree" => free = Some(bytes),
            "MemAvailable" => available = Some(bytes),
            "Buffers" => buffers = Some(bytes),
            "Cached" => cached = Some(bytes),
            "SReclaimable" => reclaimable = Some(bytes),
            "SwapTotal" => swap_total = Some(bytes),
            "SwapFree" => swap_free = Some(bytes),
            _ => {}
        }
    }

    let total = total?;

    // `MemAvailable` is the kernel's own estimate of what a new workload could get, and
    // it is what `free` reports as "available". Prefer it. Only fall back to the manual
    // computation on kernels older than 3.14, which do not publish it.
    let available = available.or_else(|| {
        Some(
            free?
                .saturating_add(buffers.unwrap_or(0))
                .saturating_add(cached.unwrap_or(0))
                .saturating_add(reclaimable.unwrap_or(0)),
        )
    });

    let used = available.map(|avail| total.saturating_sub(avail));
    let swap_used = match (swap_total, swap_free) {
        (Some(t), Some(f)) => Some(t.saturating_sub(f)),
        _ => None,
    };

    Some(MemoryUsage {
        total_bytes: Some(total),
        used_bytes: used,
        available_bytes: available,
        swap_total_bytes: swap_total,
        swap_used_bytes: swap_used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::metrics::MetricValue;

    const MEMINFO: &str = "\
MemTotal:       16316456 kB
MemFree:          234140 kB
MemAvailable:   12000000 kB
Buffers:          123456 kB
Cached:          4567890 kB
SwapCached:            0 kB
Active:          8000000 kB
SReclaimable:     300000 kB
SwapTotal:       2097148 kB
SwapFree:        1097148 kB
Dirty:               128 kB";

    fn parse(text: &str) -> MemoryUsage {
        let output = MemoryCollector
            .parse(&[Ok(CommandOutput::success(text))])
            .expect("parses");
        let CollectorOutput::Memory(memory) = output else {
            panic!("expected memory output")
        };
        memory
    }

    #[test]
    fn used_memory_is_derived_from_mem_available() {
        let memory = parse(MEMINFO);
        assert_eq!(memory.total_bytes, Some(16_316_456 * 1_024));
        assert_eq!(memory.available_bytes, Some(12_000_000 * 1_024));
        assert_eq!(memory.used_bytes, Some((16_316_456 - 12_000_000) * 1_024));
    }

    #[test]
    fn cache_is_not_counted_as_used() {
        // The classic mistake is `total - free`, which on this input would report 98%
        // used on a machine that has 12 GB available.
        let memory = parse(MEMINFO);
        let percent = memory.used_percent().value().expect("percentage available");
        assert!(percent > 25.0 && percent < 27.0, "unexpected {percent}%");
    }

    #[test]
    fn swap_usage_is_total_minus_free() {
        let memory = parse(MEMINFO);
        assert_eq!(memory.swap_total_bytes, Some(2_097_148 * 1_024));
        assert_eq!(memory.swap_used_bytes, Some(1_000_000 * 1_024));
    }

    #[test]
    fn kernels_without_mem_available_fall_back_to_the_manual_sum() {
        let old = "\
MemTotal:       1000000 kB
MemFree:         100000 kB
Buffers:          50000 kB
Cached:          200000 kB
SReclaimable:     50000 kB";
        let memory = parse(old);
        // available = free + buffers + cached + reclaimable = 400000 kB
        assert_eq!(memory.available_bytes, Some(400_000 * 1_024));
        assert_eq!(memory.used_bytes, Some(600_000 * 1_024));
    }

    #[test]
    fn a_machine_without_swap_reports_swap_as_unavailable_not_as_zero_percent() {
        let no_swap = "\
MemTotal:       1000000 kB
MemAvailable:    400000 kB
SwapTotal:            0 kB
SwapFree:             0 kB";
        let memory = parse(no_swap);
        assert_eq!(memory.swap_used_bytes, Some(0));
        // 0/0 must not become 0% healthy — there is no swap to be a percentage of.
        assert_eq!(memory.swap_used_percent(), MetricValue::NotAvailable);
    }

    #[test]
    fn unparsable_input_is_rejected_rather_than_yielding_zeroes() {
        let err = MemoryCollector
            .parse(&[Ok(CommandOutput::success("this is not meminfo"))])
            .expect_err("must fail");
        assert!(matches!(err, CollectError::Parse { .. }));
    }

    #[test]
    fn unexpected_lines_are_skipped_without_derailing_the_parse() {
        let messy = format!("{MEMINFO}\nGarbage line without colon\nAnotherKey: not-a-number kB");
        let memory = parse(&messy);
        assert_eq!(memory.total_bytes, Some(16_316_456 * 1_024));
    }

    #[test]
    fn a_transport_failure_propagates() {
        let err = MemoryCollector
            .parse(&[Err(TransportError::NotConnected)])
            .expect_err("must fail");
        assert!(matches!(err, CollectError::Transport(_)));
    }
}
