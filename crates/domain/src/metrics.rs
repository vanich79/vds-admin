//! Metric vocabulary: what can be measured, how a measurement is represented, and how
//! measurements are rolled up over time.

use crate::ids::{CollectorId, ServerId};
use crate::status::Status;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// A measurement that may legitimately be absent.
///
/// The whole point of this type is that "the provider does not expose this number" is a
/// first-class outcome, distinct from zero. Substituting a fabricated value anywhere is
/// a bug.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MetricValue {
    Available(f64),
    #[default]
    NotAvailable,
}

impl MetricValue {
    /// Wraps a number, rejecting non-finite values as unavailable.
    pub fn available(value: f64) -> Self {
        if value.is_finite() {
            MetricValue::Available(value)
        } else {
            MetricValue::NotAvailable
        }
    }

    pub fn is_available(self) -> bool {
        matches!(self, MetricValue::Available(_))
    }

    pub fn value(self) -> Option<f64> {
        match self {
            MetricValue::Available(v) => Some(v),
            MetricValue::NotAvailable => None,
        }
    }

    /// The number, or `default` when unavailable.
    ///
    /// Only for presentation-adjacent maths where an explicit fallback is intended;
    /// never use it to invent data for storage.
    pub fn value_or(self, default: f64) -> f64 {
        self.value().unwrap_or(default)
    }
}

impl From<f64> for MetricValue {
    fn from(value: f64) -> Self {
        MetricValue::available(value)
    }
}

impl From<Option<f64>> for MetricValue {
    fn from(value: Option<f64>) -> Self {
        value.map_or(MetricValue::NotAvailable, MetricValue::available)
    }
}

impl fmt::Display for MetricValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MetricValue::Available(v) => write!(f, "{v}"),
            MetricValue::NotAvailable => f.write_str("—"),
        }
    }
}

/// The unit a metric is expressed in, so the UI can format without special-casing names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricUnit {
    Percent,
    Bytes,
    BytesPerSecond,
    Seconds,
    Milliseconds,
    Count,
    Ratio,
    Celsius,
}

/// Every server-side metric the system knows how to store.
///
/// A stable string form is used in the database so that adding a variant never
/// renumbers existing rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    CpuUsage,
    MemoryUsage,
    MemoryUsedBytes,
    SwapUsage,
    DiskUsage,
    DiskUsedBytes,
    NetworkRxBytesPerSec,
    NetworkTxBytesPerSec,
    LoadAverage1,
    LoadAverage5,
    LoadAverage15,
    UptimeSeconds,
    ProcessCount,
    TemperatureCelsius,
    /// Website round-trip time.
    ResponseTimeMs,
    /// Days remaining until the TLS certificate expires.
    SslDaysRemaining,
}

impl MetricKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MetricKind::CpuUsage => "cpu_usage",
            MetricKind::MemoryUsage => "memory_usage",
            MetricKind::MemoryUsedBytes => "memory_used_bytes",
            MetricKind::SwapUsage => "swap_usage",
            MetricKind::DiskUsage => "disk_usage",
            MetricKind::DiskUsedBytes => "disk_used_bytes",
            MetricKind::NetworkRxBytesPerSec => "network_rx_bps",
            MetricKind::NetworkTxBytesPerSec => "network_tx_bps",
            MetricKind::LoadAverage1 => "load_avg_1",
            MetricKind::LoadAverage5 => "load_avg_5",
            MetricKind::LoadAverage15 => "load_avg_15",
            MetricKind::UptimeSeconds => "uptime_seconds",
            MetricKind::ProcessCount => "process_count",
            MetricKind::TemperatureCelsius => "temperature_celsius",
            MetricKind::ResponseTimeMs => "response_time_ms",
            MetricKind::SslDaysRemaining => "ssl_days_remaining",
        }
    }

    pub fn parse(raw: &str) -> Option<MetricKind> {
        let kind = match raw {
            "cpu_usage" => MetricKind::CpuUsage,
            "memory_usage" => MetricKind::MemoryUsage,
            "memory_used_bytes" => MetricKind::MemoryUsedBytes,
            "swap_usage" => MetricKind::SwapUsage,
            "disk_usage" => MetricKind::DiskUsage,
            "disk_used_bytes" => MetricKind::DiskUsedBytes,
            "network_rx_bps" => MetricKind::NetworkRxBytesPerSec,
            "network_tx_bps" => MetricKind::NetworkTxBytesPerSec,
            "load_avg_1" => MetricKind::LoadAverage1,
            "load_avg_5" => MetricKind::LoadAverage5,
            "load_avg_15" => MetricKind::LoadAverage15,
            "uptime_seconds" => MetricKind::UptimeSeconds,
            "process_count" => MetricKind::ProcessCount,
            "temperature_celsius" => MetricKind::TemperatureCelsius,
            "response_time_ms" => MetricKind::ResponseTimeMs,
            "ssl_days_remaining" => MetricKind::SslDaysRemaining,
            _ => return None,
        };
        Some(kind)
    }

    pub fn unit(self) -> MetricUnit {
        match self {
            MetricKind::CpuUsage
            | MetricKind::MemoryUsage
            | MetricKind::SwapUsage
            | MetricKind::DiskUsage => MetricUnit::Percent,
            MetricKind::MemoryUsedBytes | MetricKind::DiskUsedBytes => MetricUnit::Bytes,
            MetricKind::NetworkRxBytesPerSec | MetricKind::NetworkTxBytesPerSec => {
                MetricUnit::BytesPerSecond
            }
            MetricKind::LoadAverage1 | MetricKind::LoadAverage5 | MetricKind::LoadAverage15 => {
                MetricUnit::Ratio
            }
            MetricKind::UptimeSeconds => MetricUnit::Seconds,
            MetricKind::ProcessCount => MetricUnit::Count,
            MetricKind::TemperatureCelsius => MetricUnit::Celsius,
            MetricKind::ResponseTimeMs => MetricUnit::Milliseconds,
            MetricKind::SslDaysRemaining => MetricUnit::Count,
        }
    }

    /// Human-readable label. Presentation uses it; nothing depends on it.
    pub fn label(self) -> &'static str {
        match self {
            MetricKind::CpuUsage => "CPU",
            MetricKind::MemoryUsage => "RAM",
            MetricKind::MemoryUsedBytes => "RAM used",
            MetricKind::SwapUsage => "Swap",
            MetricKind::DiskUsage => "Disk",
            MetricKind::DiskUsedBytes => "Disk used",
            MetricKind::NetworkRxBytesPerSec => "Network in",
            MetricKind::NetworkTxBytesPerSec => "Network out",
            MetricKind::LoadAverage1 => "Load 1m",
            MetricKind::LoadAverage5 => "Load 5m",
            MetricKind::LoadAverage15 => "Load 15m",
            MetricKind::UptimeSeconds => "Uptime",
            MetricKind::ProcessCount => "Processes",
            MetricKind::TemperatureCelsius => "Temperature",
            MetricKind::ResponseTimeMs => "Response time",
            MetricKind::SslDaysRemaining => "SSL expiry",
        }
    }

    /// All variants, for UI pickers and for exhaustive iteration in tests.
    pub const ALL: &'static [MetricKind] = &[
        MetricKind::CpuUsage,
        MetricKind::MemoryUsage,
        MetricKind::MemoryUsedBytes,
        MetricKind::SwapUsage,
        MetricKind::DiskUsage,
        MetricKind::DiskUsedBytes,
        MetricKind::NetworkRxBytesPerSec,
        MetricKind::NetworkTxBytesPerSec,
        MetricKind::LoadAverage1,
        MetricKind::LoadAverage5,
        MetricKind::LoadAverage15,
        MetricKind::UptimeSeconds,
        MetricKind::ProcessCount,
        MetricKind::TemperatureCelsius,
        MetricKind::ResponseTimeMs,
        MetricKind::SslDaysRemaining,
    ];
}

impl fmt::Display for MetricKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The result of one collector observation: a value, its interpretation, and why.
///
/// Collectors return this; they never decide what the UI shows, and the UI never
/// recomputes the status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricResult {
    pub kind: MetricKind,
    pub status: Status,
    pub value: MetricValue,
    pub timestamp: DateTime<Utc>,
    /// Short human-readable explanation, shown in tooltips and event logs.
    pub message: Option<String>,
}

impl MetricResult {
    pub fn new(kind: MetricKind, value: MetricValue, status: Status, at: DateTime<Utc>) -> Self {
        Self {
            kind,
            status,
            value,
            timestamp: at,
            message: None,
        }
    }

    /// A measurement we could not obtain. Status is `Unknown`, never `Healthy`.
    pub fn unavailable(kind: MetricKind, at: DateTime<Utc>, reason: impl Into<String>) -> Self {
        Self {
            kind,
            status: Status::Unknown,
            value: MetricValue::NotAvailable,
            timestamp: at,
            message: Some(reason.into()),
        }
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

/// One stored point of raw time-series data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSample {
    pub server_id: ServerId,
    pub kind: MetricKind,
    pub value: f64,
    pub timestamp: DateTime<Utc>,
}

/// Resolution tier of stored time-series data.
///
/// Ordered coarsest-last so that `>` means "coarser".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// Whatever the polling interval produced.
    Raw,
    FiveMinutes,
    OneHour,
    OneDay,
}

impl Resolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::Raw => "raw",
            Resolution::FiveMinutes => "m5",
            Resolution::OneHour => "h1",
            Resolution::OneDay => "d1",
        }
    }

    pub fn parse(raw: &str) -> Option<Resolution> {
        match raw {
            "raw" => Some(Resolution::Raw),
            "m5" => Some(Resolution::FiveMinutes),
            "h1" => Some(Resolution::OneHour),
            "d1" => Some(Resolution::OneDay),
            _ => None,
        }
    }

    /// Bucket width. `Raw` has none, since it follows the polling interval.
    pub fn bucket_width(self) -> Option<Duration> {
        match self {
            Resolution::Raw => None,
            Resolution::FiveMinutes => Some(Duration::minutes(5)),
            Resolution::OneHour => Some(Duration::hours(1)),
            Resolution::OneDay => Some(Duration::days(1)),
        }
    }

    /// The tier this tier is computed from.
    ///
    /// Rollups cascade (`raw → m5 → h1 → d1`) so aggregation cost stays constant as
    /// history grows.
    pub fn source(self) -> Option<Resolution> {
        match self {
            Resolution::Raw => None,
            Resolution::FiveMinutes => Some(Resolution::Raw),
            Resolution::OneHour => Some(Resolution::FiveMinutes),
            Resolution::OneDay => Some(Resolution::OneHour),
        }
    }

    /// Truncates a timestamp to the start of its bucket.
    pub fn bucket_start(self, at: DateTime<Utc>) -> DateTime<Utc> {
        let Some(width) = self.bucket_width() else {
            return at;
        };
        let width_secs = width.num_seconds().max(1);
        let secs = at.timestamp();
        // `rem_euclid` keeps pre-epoch timestamps bucketing downwards rather than
        // towards zero, which would put them in the wrong bucket.
        let start = secs - secs.rem_euclid(width_secs);
        DateTime::from_timestamp(start, 0).unwrap_or(at)
    }

    /// Coarsest-first list, useful when picking a tier for a query window.
    pub const ALL: &'static [Resolution] = &[
        Resolution::Raw,
        Resolution::FiveMinutes,
        Resolution::OneHour,
        Resolution::OneDay,
    ];
}

/// A time range requested by the UI.
///
/// The shared `Last` prefix is deliberate: these are ranges ending *now*, and bare
/// `Hour`/`Day` would be indistinguishable from
/// [`AnalyticsInterval`](crate::analytics::AnalyticsInterval)'s bucket sizes at the call
/// site, which is exactly the confusion that produces a chart with the wrong axis.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeRange {
    LastHour,
    LastSixHours,
    LastDay,
    LastWeek,
    LastMonth,
    LastQuarter,
    LastYear,
}

impl TimeRange {
    pub fn duration(self) -> Duration {
        match self {
            TimeRange::LastHour => Duration::hours(1),
            TimeRange::LastSixHours => Duration::hours(6),
            TimeRange::LastDay => Duration::days(1),
            TimeRange::LastWeek => Duration::days(7),
            TimeRange::LastMonth => Duration::days(30),
            TimeRange::LastQuarter => Duration::days(90),
            TimeRange::LastYear => Duration::days(365),
        }
    }

    /// Which stored tier answers this range while keeping the point count sane.
    ///
    /// The mapping is chosen so no query returns more than [`MAX_CHART_POINTS`]; see
    /// `docs/adr/005-metrics-storage.md`. `every_range_stays_under_the_point_budget`
    /// holds this honest.
    pub fn resolution(self) -> Resolution {
        match self {
            TimeRange::LastHour => Resolution::Raw,
            TimeRange::LastSixHours | TimeRange::LastDay => Resolution::FiveMinutes,
            TimeRange::LastWeek | TimeRange::LastMonth => Resolution::OneHour,
            TimeRange::LastQuarter | TimeRange::LastYear => Resolution::OneDay,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TimeRange::LastHour => "1 hour",
            TimeRange::LastSixHours => "6 hours",
            TimeRange::LastDay => "24 hours",
            TimeRange::LastWeek => "7 days",
            TimeRange::LastMonth => "30 days",
            TimeRange::LastQuarter => "90 days",
            TimeRange::LastYear => "1 year",
        }
    }

    /// Resolves to an absolute window ending at `now`.
    pub fn window(self, now: DateTime<Utc>) -> TimeWindow {
        TimeWindow {
            from: now - self.duration(),
            to: now,
        }
    }

    pub const ALL: &'static [TimeRange] = &[
        TimeRange::LastHour,
        TimeRange::LastSixHours,
        TimeRange::LastDay,
        TimeRange::LastWeek,
        TimeRange::LastMonth,
        TimeRange::LastQuarter,
        TimeRange::LastYear,
    ];
}

/// Upper bound on how many points any chart query may return.
///
/// The UI is expected to render this many without struggling; the tier mapping in
/// [`TimeRange::resolution`] exists to keep every range under it.
pub const MAX_CHART_POINTS: i64 = 750;

/// An absolute, half-open time window `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

impl TimeWindow {
    pub fn new(from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        if from <= to {
            Self { from, to }
        } else {
            Self { from: to, to: from }
        }
    }

    pub fn duration(&self) -> Duration {
        self.to - self.from
    }

    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        at >= self.from && at < self.to
    }

    /// The window of the same length immediately preceding this one, for
    /// period-over-period comparisons.
    pub fn previous(&self) -> TimeWindow {
        let len = self.duration();
        TimeWindow {
            from: self.from - len,
            to: self.from,
        }
    }
}

/// An aggregated bucket of a single series.
///
/// `min`/`max` are retained so long-range charts still show spikes instead of averaging
/// them into nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricRollup {
    pub server_id: ServerId,
    pub kind: MetricKind,
    pub resolution: Resolution,
    pub bucket_start: DateTime<Utc>,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub sum: f64,
    pub count: u32,
}

/// A series ready for rendering: already at the right resolution and point count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricSeries {
    pub kind: MetricKind,
    pub resolution: Resolution,
    pub window: TimeWindow,
    pub points: Vec<SeriesPoint>,
}

impl MetricSeries {
    pub fn empty(kind: MetricKind, resolution: Resolution, window: TimeWindow) -> Self {
        Self {
            kind,
            resolution,
            window,
            points: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Largest `max` across the series, used to scale a chart's Y axis.
    pub fn peak(&self) -> Option<f64> {
        self.points
            .iter()
            .map(|p| p.max)
            .fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| if v > a { v } else { a }))
            })
    }

    /// Time-unweighted mean of the point averages.
    pub fn mean(&self) -> Option<f64> {
        if self.points.is_empty() {
            return None;
        }
        let sum: f64 = self.points.iter().map(|p| p.avg).sum();
        Some(sum / self.points.len() as f64)
    }

    pub fn latest(&self) -> Option<&SeriesPoint> {
        self.points.last()
    }
}

/// One point of a renderable series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SeriesPoint {
    pub timestamp: DateTime<Utc>,
    pub avg: f64,
    pub min: f64,
    pub max: f64,
}

impl SeriesPoint {
    /// A point from a single raw sample, where min == avg == max.
    pub fn flat(timestamp: DateTime<Utc>, value: f64) -> Self {
        Self {
            timestamp,
            avg: value,
            min: value,
            max: value,
        }
    }
}

/// Which collector produced a result, and whether it succeeded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectorOutcome {
    pub collector: CollectorId,
    pub status: Status,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    #[test]
    fn metric_value_rejects_non_finite_numbers() {
        assert_eq!(MetricValue::available(f64::NAN), MetricValue::NotAvailable);
        assert_eq!(
            MetricValue::available(f64::INFINITY),
            MetricValue::NotAvailable
        );
        assert_eq!(MetricValue::available(1.5), MetricValue::Available(1.5));
    }

    #[test]
    fn unavailable_renders_as_a_dash_not_a_zero() {
        assert_eq!(MetricValue::NotAvailable.to_string(), "—");
        assert_eq!(MetricValue::NotAvailable.value(), None);
    }

    #[test]
    fn every_metric_kind_round_trips_through_its_stored_form() {
        for kind in MetricKind::ALL {
            assert_eq!(
                MetricKind::parse(kind.as_str()),
                Some(*kind),
                "{kind} failed"
            );
        }
    }

    #[test]
    fn metric_kind_names_are_unique() {
        let mut names: Vec<&str> = MetricKind::ALL.iter().map(|k| k.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate metric identifier");
    }

    #[test]
    fn unavailable_measurement_is_unknown_not_healthy() {
        let result = MetricResult::unavailable(MetricKind::CpuUsage, at(0), "no /proc/stat");
        assert_eq!(result.status, Status::Unknown);
        assert!(!result.value.is_available());
    }

    #[test]
    fn resolutions_round_trip_and_cascade() {
        for res in Resolution::ALL {
            assert_eq!(Resolution::parse(res.as_str()), Some(*res));
        }
        assert_eq!(Resolution::Raw.source(), None);
        assert_eq!(Resolution::FiveMinutes.source(), Some(Resolution::Raw));
        assert_eq!(Resolution::OneHour.source(), Some(Resolution::FiveMinutes));
        assert_eq!(Resolution::OneDay.source(), Some(Resolution::OneHour));
    }

    #[test]
    fn bucket_start_truncates_downwards() {
        // 1970-01-01T00:07:30Z truncates to 00:05:00 in a 5-minute bucket.
        assert_eq!(Resolution::FiveMinutes.bucket_start(at(450)), at(300));
        assert_eq!(Resolution::OneHour.bucket_start(at(3_601)), at(3_600));
        // Raw data is not bucketed at all.
        assert_eq!(Resolution::Raw.bucket_start(at(450)), at(450));
    }

    #[test]
    fn bucket_start_handles_pre_epoch_timestamps_without_rounding_up() {
        let before_epoch = Utc.with_ymd_and_hms(1969, 12, 31, 23, 57, 30).unwrap();
        let bucket = Resolution::FiveMinutes.bucket_start(before_epoch);
        assert!(
            bucket <= before_epoch,
            "bucket start must not be after the sample"
        );
        assert_eq!(
            bucket,
            Utc.with_ymd_and_hms(1969, 12, 31, 23, 55, 0).unwrap()
        );
    }

    #[test]
    fn long_ranges_select_coarse_resolutions() {
        assert_eq!(TimeRange::LastHour.resolution(), Resolution::Raw);
        assert_eq!(TimeRange::LastDay.resolution(), Resolution::FiveMinutes);
        assert_eq!(TimeRange::LastMonth.resolution(), Resolution::OneHour);
        assert_eq!(TimeRange::LastYear.resolution(), Resolution::OneDay);
    }

    #[test]
    fn every_range_stays_under_the_point_budget() {
        // The contract from ADR-005. This test is the reason `LastWeek` is served from
        // hourly rollups rather than five-minute ones: the finer tier would be 2016
        // points for a week, which is not a chart, it is a smear.
        const FASTEST_POLL_SECS: i64 = 15;
        for range in TimeRange::ALL {
            let points = match range.resolution().bucket_width() {
                // Raw is bounded by the polling interval, not by a bucket width.
                None => range.duration().num_seconds() / FASTEST_POLL_SECS,
                Some(width) => range.duration().num_seconds() / width.num_seconds(),
            };
            assert!(
                points <= MAX_CHART_POINTS,
                "{} would return {} points, over the {} budget",
                range.label(),
                points,
                MAX_CHART_POINTS
            );
        }
    }

    #[test]
    fn time_window_normalises_reversed_bounds() {
        let window = TimeWindow::new(at(100), at(0));
        assert_eq!(window.from, at(0));
        assert_eq!(window.to, at(100));
    }

    #[test]
    fn previous_window_abuts_the_current_one() {
        let window = TimeWindow::new(at(1_000), at(2_000));
        let previous = window.previous();
        assert_eq!(previous.to, window.from);
        assert_eq!(previous.duration(), window.duration());
    }

    #[test]
    fn window_is_half_open() {
        let window = TimeWindow::new(at(10), at(20));
        assert!(window.contains(at(10)));
        assert!(!window.contains(at(20)));
    }

    #[test]
    fn series_peak_and_mean_ignore_ordering() {
        let series = MetricSeries {
            kind: MetricKind::CpuUsage,
            resolution: Resolution::Raw,
            window: TimeWindow::new(at(0), at(100)),
            points: vec![
                SeriesPoint {
                    timestamp: at(0),
                    avg: 10.0,
                    min: 5.0,
                    max: 40.0,
                },
                SeriesPoint {
                    timestamp: at(50),
                    avg: 20.0,
                    min: 15.0,
                    max: 25.0,
                },
            ],
        };
        assert_eq!(series.peak(), Some(40.0));
        assert_eq!(series.mean(), Some(15.0));
        assert_eq!(series.latest().map(|p| p.avg), Some(20.0));
    }

    #[test]
    fn empty_series_has_no_peak_or_mean() {
        let series = MetricSeries::empty(
            MetricKind::CpuUsage,
            Resolution::Raw,
            TimeWindow::new(at(0), at(1)),
        );
        assert_eq!(series.peak(), None);
        assert_eq!(series.mean(), None);
    }
}
