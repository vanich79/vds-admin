//! CPU/board temperature from the kernel thermal zones.

use vds_domain::ids::CollectorId;
use vds_domain::metrics::MetricValue;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};

/// Values outside this range are sensor noise, not temperatures.
///
/// Disconnected or unsupported sensors commonly report 0 or absurd values; publishing
/// those as a measurement would trigger nonsense alerts.
const PLAUSIBLE_RANGE: std::ops::RangeInclusive<f64> = 1.0..=150.0;

/// Reads `/sys/class/thermal/thermal_zone*/temp`.
///
/// Reports the hottest plausible zone, because that is the one that will throttle or
/// shut the machine down. Many hosts — most VPS instances, all containers — expose no
/// thermal zones at all, which is reported as unavailable rather than as an error.
#[derive(Debug, Clone, Copy, Default)]
pub struct TemperatureCollector;

impl Collector for TemperatureCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("temperature")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::ThermalSensors]
    }

    fn commands(&self) -> Vec<Command> {
        // `head -c` bounds the output in case a host exposes hundreds of zones.
        vec![Command::shell(
            "cat /sys/class/thermal/thermal_zone*/temp 2>/dev/null | head -c 4096",
        )]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let Some(Ok(output)) = outputs.first() else {
            // A failed temperature read is never worth failing a cycle over.
            return Ok(CollectorOutput::Temperature(MetricValue::NotAvailable));
        };
        Ok(CollectorOutput::Temperature(parse_thermal_zones(
            &output.stdout,
        )))
    }
}

/// Parses thermal-zone readings, which the kernel publishes in millidegrees Celsius.
pub fn parse_thermal_zones(text: &str) -> MetricValue {
    let hottest = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| line.parse::<i64>().ok())
        .map(|millidegrees| millidegrees as f64 / 1_000.0)
        .filter(|celsius| PLAUSIBLE_RANGE.contains(celsius))
        .fold(None, |acc: Option<f64>, v| {
            Some(acc.map_or(v, |a| if v > a { v } else { a }))
        });

    hottest.map_or(MetricValue::NotAvailable, MetricValue::available)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(text: &str) -> MetricValue {
        let output = TemperatureCollector
            .parse(&[Ok(CommandOutput::success(text))])
            .expect("never fails");
        let CollectorOutput::Temperature(value) = output else {
            panic!("expected temperature")
        };
        value
    }

    #[test]
    fn millidegrees_are_converted_to_celsius() {
        assert_eq!(collect("42500\n"), MetricValue::Available(42.5));
    }

    #[test]
    fn the_hottest_zone_wins() {
        assert_eq!(
            collect("35000\n68000\n41000\n"),
            MetricValue::Available(68.0)
        );
    }

    #[test]
    fn a_host_with_no_sensors_reports_unavailable_not_zero() {
        // Every VPS and container lands here. Zero would read as a suspiciously cold,
        // perfectly healthy machine.
        assert_eq!(collect(""), MetricValue::NotAvailable);
    }

    #[test]
    fn implausible_readings_are_discarded() {
        // Disconnected sensors report 0; some report the raw sentinel -274000.
        assert_eq!(collect("0\n"), MetricValue::NotAvailable);
        assert_eq!(collect("-274000\n"), MetricValue::NotAvailable);
        assert_eq!(collect("9999000\n"), MetricValue::NotAvailable);
    }

    #[test]
    fn one_bad_sensor_does_not_hide_a_good_one() {
        assert_eq!(collect("0\n55000\n9999000\n"), MetricValue::Available(55.0));
    }

    #[test]
    fn non_numeric_lines_are_ignored() {
        assert_eq!(
            collect("cat: permission denied\n61000\n"),
            MetricValue::Available(61.0)
        );
    }

    #[test]
    fn a_transport_failure_yields_unavailable_rather_than_an_error() {
        let output = TemperatureCollector
            .parse(&[Err(TransportError::Timeout { seconds: 2 })])
            .expect("temperature never fails a cycle");
        assert_eq!(
            output,
            CollectorOutput::Temperature(MetricValue::NotAvailable)
        );
    }
}
