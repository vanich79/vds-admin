//! Load average and uptime from `/proc/loadavg` and `/proc/uptime`.

use vds_domain::ids::CollectorId;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};
use vds_domain::server::LoadAverage;

/// Reads load average and uptime.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoadCollector;

const LOADAVG: usize = 0;
const UPTIME: usize = 1;

impl Collector for LoadCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("load")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::ProcFs]
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            Command::read("/proc/loadavg"),
            Command::read("/proc/uptime"),
        ]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let id = self.id();
        let loadavg = outputs
            .get(LOADAVG)
            .ok_or_else(|| CollectError::parse(&id, "no output for /proc/loadavg"))?
            .as_ref()
            .map_err(|e| CollectError::Transport(e.clone()))?;

        if !loadavg.is_success() {
            return Err(CollectError::parse(
                &id,
                format!("reading /proc/loadavg failed: {}", loadavg.stderr.trim()),
            ));
        }

        let load = parse_loadavg(&loadavg.stdout)
            .ok_or_else(|| CollectError::parse(&id, "malformed /proc/loadavg"))?;

        // Uptime is a bonus: a missing or unreadable /proc/uptime must not discard a
        // perfectly good load average.
        let uptime_secs = match outputs.get(UPTIME) {
            Some(Ok(output)) if output.is_success() => parse_uptime(&output.stdout),
            _ => None,
        };

        Ok(CollectorOutput::Load { load, uptime_secs })
    }
}

/// Parses `/proc/loadavg`: `"0.52 0.58 0.59 2/1234 5678"`.
pub fn parse_loadavg(text: &str) -> Option<LoadAverage> {
    let mut fields = text.split_whitespace();
    let one: f64 = fields.next()?.parse().ok()?;
    let five: f64 = fields.next()?.parse().ok()?;
    let fifteen: f64 = fields.next()?.parse().ok()?;
    if !(one.is_finite() && five.is_finite() && fifteen.is_finite()) {
        return None;
    }
    if one < 0.0 || five < 0.0 || fifteen < 0.0 {
        return None;
    }
    Some(LoadAverage { one, five, fifteen })
}

/// Parses `/proc/uptime`: `"12345.67 98765.43"`. The first field is seconds since boot.
pub fn parse_uptime(text: &str) -> Option<u64> {
    let seconds: f64 = text.split_whitespace().next()?.parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(seconds as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(
        outputs: Vec<Result<CommandOutput, TransportError>>,
    ) -> Result<(LoadAverage, Option<u64>), CollectError> {
        match LoadCollector.parse(&outputs)? {
            CollectorOutput::Load { load, uptime_secs } => Ok((load, uptime_secs)),
            other => panic!("expected load output, got {other:?}"),
        }
    }

    fn ok(text: &str) -> Result<CommandOutput, TransportError> {
        Ok(CommandOutput::success(text))
    }

    #[test]
    fn load_and_uptime_are_read_together() {
        let (load, uptime) = collect(vec![
            ok("0.52 0.58 0.59 2/1234 5678\n"),
            ok("12345.67 98765.43\n"),
        ])
        .expect("parses");
        assert_eq!(load.one, 0.52);
        assert_eq!(load.five, 0.58);
        assert_eq!(load.fifteen, 0.59);
        assert_eq!(uptime, Some(12_345));
    }

    #[test]
    fn a_missing_uptime_does_not_discard_the_load_average() {
        let (load, uptime) = collect(vec![
            ok("1.00 2.00 3.00 1/100 200\n"),
            Err(TransportError::Execution("no /proc/uptime".into())),
        ])
        .expect("parses");
        assert_eq!(load.one, 1.0);
        assert_eq!(uptime, None);
    }

    #[test]
    fn a_missing_loadavg_is_an_error() {
        let err = collect(vec![
            Err(TransportError::Execution("gone".into())),
            ok("1.0 2.0"),
        ])
        .expect_err("must fail");
        assert!(matches!(err, CollectError::Transport(_)));
    }

    #[test]
    fn malformed_loadavg_is_rejected_rather_than_reported_as_zero() {
        assert_eq!(parse_loadavg("not a load average"), None);
        assert_eq!(parse_loadavg("0.5 0.6"), None);
        assert_eq!(parse_loadavg(""), None);
    }

    #[test]
    fn negative_or_infinite_values_are_rejected() {
        assert_eq!(parse_loadavg("-1.0 0.0 0.0 1/1 1"), None);
        assert_eq!(parse_loadavg("inf 0.0 0.0 1/1 1"), None);
        assert_eq!(parse_uptime("-5.0 1.0"), None);
    }

    #[test]
    fn a_freshly_booted_machine_reports_zero_uptime_not_none() {
        assert_eq!(parse_uptime("0.31 0.29"), Some(0));
    }

    #[test]
    fn a_long_running_machine_keeps_full_precision_in_seconds() {
        // 143 days, matching the example in the brief.
        let seconds = 143 * 86_400 + 3_600;
        assert_eq!(
            parse_uptime(&format!("{seconds}.42 999.0")),
            Some(seconds as u64)
        );
    }
}
