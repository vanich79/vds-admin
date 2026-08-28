//! Chart geometry.
//!
//! Slint has no chart widget, so charts are drawn with `Path` elements. All the maths
//! happens here — scaling, axis selection, downsampling — and the `.slint` file receives
//! a finished SVG path string and some labels. That keeps the interesting part testable
//! and the view purely declarative.
//!
//! See `docs/adr/001-technology-stack.md`; this is the cost of the framework choice, and
//! it is a cost paid once.

use vds_domain::analytics::AnalyticsTimeSeries;
use vds_domain::metrics::{MetricSeries, MetricUnit, SeriesPoint};

/// A chart ready to draw.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChartGeometry {
    /// SVG path for the line through the average of each point.
    pub line: String,
    /// SVG path for the filled min/max band, empty when the series has no spread.
    pub band: String,
    /// Y-axis labels, top to bottom.
    pub y_labels: Vec<String>,
    /// X-axis labels, left to right.
    pub x_labels: Vec<String>,
    /// The value at the top of the axis.
    pub max_value: f64,
    /// Whether there is anything to draw.
    pub has_data: bool,
}

/// How many gridlines a chart has.
const Y_DIVISIONS: usize = 4;

/// How many x-axis labels to show.
const X_LABELS: usize = 5;

/// Most points worth drawing.
///
/// Beyond roughly this many, adjacent points land on the same pixel and the extra work
/// is wasted. The storage tiers already keep queries near this, so this is a backstop
/// rather than the main defence.
const MAX_DRAWN_POINTS: usize = 400;

/// Builds the geometry for a series inside a box of `width` × `height` units.
///
/// Coordinates are in the same units Slint uses for layout, with the origin at the top
/// left, so the caller can hand the path straight to a `Path` element.
pub fn build(series: &MetricSeries, width: f32, height: f32) -> ChartGeometry {
    build_points(&series.points, series.kind.unit(), width, height)
}

/// Builds the geometry for a traffic series.
///
/// Analytics points carry a single value rather than a min/max spread, so the chart has
/// a line and no band. Everything else — scaling, labels, downsampling — is shared with
/// server metrics, which is what keeps the two kinds of chart looking alike.
pub fn build_analytics(series: &AnalyticsTimeSeries, width: f32, height: f32) -> ChartGeometry {
    let points: Vec<SeriesPoint> = series
        .points
        .iter()
        .map(|point| SeriesPoint::flat(point.timestamp, point.value))
        .collect();
    build_points(&points, series.metric.unit(), width, height)
}

/// The shared implementation, keyed on the unit rather than on a metric kind.
fn build_points(
    points: &[SeriesPoint],
    unit: MetricUnit,
    width: f32,
    height: f32,
) -> ChartGeometry {
    if points.is_empty() || width <= 0.0 || height <= 0.0 {
        return ChartGeometry {
            y_labels: vec![UNAVAILABLE_LABEL.to_owned()],
            ..Default::default()
        };
    }

    let points = downsample(points, MAX_DRAWN_POINTS);
    let max_value = axis_maximum(unit, &points);

    // A single point cannot make a line; it is drawn as a flat one across the width so
    // the chart shows *something* rather than looking broken.
    let last_index = points.len().saturating_sub(1).max(1) as f32;

    let x_at = |index: usize| (index as f32 / last_index) * width;
    let y_at = |value: f64| {
        let fraction = if max_value > 0.0 {
            (value / max_value).clamp(0.0, 1.0)
        } else {
            0.0
        };
        height - (fraction as f32 * height)
    };

    let mut line = String::with_capacity(points.len() * 16);
    for (index, point) in points.iter().enumerate() {
        let command = if index == 0 { 'M' } else { 'L' };
        line.push_str(&format!(
            "{command} {:.2} {:.2} ",
            x_at(index),
            y_at(point.avg)
        ));
    }

    // The band shows the min/max spread that averaging would otherwise hide — the whole
    // reason rollups store min and max. It is omitted when every point is flat, which is
    // the case for raw data.
    let has_spread = points.iter().any(|p| p.max > p.min);
    let band = if has_spread {
        let mut band = String::with_capacity(points.len() * 32);
        for (index, point) in points.iter().enumerate() {
            let command = if index == 0 { 'M' } else { 'L' };
            band.push_str(&format!(
                "{command} {:.2} {:.2} ",
                x_at(index),
                y_at(point.max)
            ));
        }
        for (index, point) in points.iter().enumerate().rev() {
            band.push_str(&format!("L {:.2} {:.2} ", x_at(index), y_at(point.min)));
        }
        band.push('Z');
        band
    } else {
        String::new()
    };

    ChartGeometry {
        line: line.trim_end().to_owned(),
        band,
        y_labels: y_axis_labels(unit, max_value),
        x_labels: x_axis_labels(&points),
        max_value,
        has_data: true,
    }
}

/// Shown on the y-axis when there is nothing to plot.
const UNAVAILABLE_LABEL: &str = "no data";

/// The value at the top of the y-axis.
///
/// Percentages are always scaled 0–100 so that two CPU charts side by side are directly
/// comparable; everything else is scaled to its own peak with headroom, because a
/// network chart auto-scaled to zero would be unreadable.
fn axis_maximum(unit: MetricUnit, points: &[SeriesPoint]) -> f64 {
    if matches!(unit, MetricUnit::Percent) {
        return 100.0;
    }

    let peak = points.iter().map(|p| p.max).fold(0.0_f64, f64::max);
    if peak <= 0.0 {
        // A flat-zero series still needs a non-zero axis, or every point sits on the
        // baseline and the chart looks empty rather than idle.
        return 1.0;
    }

    // Round up to something legible rather than to the exact peak.
    let magnitude = 10.0_f64.powf(peak.log10().floor());
    let steps = (peak / magnitude).ceil();
    (steps * magnitude).max(peak * 1.05)
}

/// Y-axis labels, top to bottom.
fn y_axis_labels(unit: MetricUnit, max_value: f64) -> Vec<String> {
    (0..=Y_DIVISIONS)
        .rev()
        .map(|division| {
            let value = max_value * division as f64 / Y_DIVISIONS as f64;
            crate::format::metric(vds_domain::metrics::MetricValue::available(value), unit)
        })
        .collect()
}

/// X-axis labels, evenly spaced across the series.
fn x_axis_labels(points: &[vds_domain::metrics::SeriesPoint]) -> Vec<String> {
    if points.is_empty() {
        return Vec::new();
    }
    if points.len() == 1 {
        return vec![points[0].timestamp.format("%H:%M").to_string()];
    }

    // Whether to show a date depends on how much time the series covers: "14:32" is
    // useless on a 30-day chart, and "26 Aug" is useless on a one-hour one.
    let span = points[points.len() - 1].timestamp - points[0].timestamp;
    let format = if span.num_days() >= 2 {
        "%d %b"
    } else {
        "%H:%M"
    };

    let labels = X_LABELS.min(points.len());
    (0..labels)
        .map(|index| {
            let position = index * (points.len() - 1) / labels.saturating_sub(1).max(1);
            points[position.min(points.len() - 1)]
                .timestamp
                .format(format)
                .to_string()
        })
        .collect()
}

/// Reduces a series to at most `limit` points, keeping the extremes of each bucket.
///
/// Plain sampling would drop spikes, which is the opposite of what a monitoring chart is
/// for; this keeps the highest and lowest value in each bucket.
fn downsample(
    points: &[vds_domain::metrics::SeriesPoint],
    limit: usize,
) -> Vec<vds_domain::metrics::SeriesPoint> {
    if points.len() <= limit || limit == 0 {
        return points.to_vec();
    }

    let bucket_size = points.len().div_ceil(limit);
    points
        .chunks(bucket_size)
        .filter_map(|chunk| {
            let first = chunk.first()?;
            let avg = chunk.iter().map(|p| p.avg).sum::<f64>() / chunk.len() as f64;
            Some(vds_domain::metrics::SeriesPoint {
                timestamp: first.timestamp,
                avg,
                min: chunk.iter().map(|p| p.min).fold(f64::INFINITY, f64::min),
                max: chunk
                    .iter()
                    .map(|p| p.max)
                    .fold(f64::NEG_INFINITY, f64::max),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};
    use vds_domain::metrics::{MetricKind, Resolution, SeriesPoint, TimeWindow};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn series_of(kind: MetricKind, values: &[f64]) -> MetricSeries {
        MetricSeries {
            kind,
            resolution: Resolution::Raw,
            window: TimeWindow::new(at(0), at(values.len() as i64 * 60)),
            points: values
                .iter()
                .enumerate()
                .map(|(index, value)| SeriesPoint::flat(at(index as i64 * 60), *value))
                .collect(),
        }
    }

    #[test]
    fn a_series_becomes_a_path_with_one_command_per_point() {
        let chart = build(
            &series_of(MetricKind::CpuUsage, &[10.0, 50.0, 20.0]),
            300.0,
            100.0,
        );

        assert!(chart.has_data);
        assert!(chart.line.starts_with("M "));
        assert_eq!(chart.line.matches('L').count(), 2);
        assert_eq!(chart.line.matches('M').count(), 1);
    }

    #[test]
    fn the_first_point_is_at_the_left_edge_and_the_last_at_the_right() {
        let chart = build(
            &series_of(MetricKind::CpuUsage, &[0.0, 50.0, 100.0]),
            300.0,
            100.0,
        );
        assert!(chart.line.contains("M 0.00"), "path was {}", chart.line);
        assert!(chart.line.contains("L 300.00"), "path was {}", chart.line);
    }

    #[test]
    fn a_higher_value_is_drawn_higher_on_the_screen() {
        // Slint's origin is top left, so a larger value means a *smaller* y.
        let chart = build(
            &series_of(MetricKind::CpuUsage, &[0.0, 100.0]),
            100.0,
            100.0,
        );

        // 0% sits on the baseline (y = height), 100% at the top (y = 0).
        assert!(
            chart.line.contains("M 0.00 100.00"),
            "path was {}",
            chart.line
        );
        assert!(
            chart.line.contains("L 100.00 0.00"),
            "path was {}",
            chart.line
        );
    }

    #[test]
    fn percentage_charts_are_always_scaled_zero_to_a_hundred() {
        // So two CPU charts side by side are directly comparable.
        let quiet = build(
            &series_of(MetricKind::CpuUsage, &[1.0, 2.0, 3.0]),
            100.0,
            100.0,
        );
        let busy = build(
            &series_of(MetricKind::CpuUsage, &[90.0, 95.0, 99.0]),
            100.0,
            100.0,
        );

        assert_eq!(quiet.max_value, 100.0);
        assert_eq!(busy.max_value, 100.0);
    }

    #[test]
    fn non_percentage_charts_scale_to_their_own_peak() {
        // A network chart pinned to 100 would be a flat line at the bottom.
        let chart = build(
            &series_of(MetricKind::NetworkRxBytesPerSec, &[1_000.0, 8_000.0]),
            100.0,
            100.0,
        );
        assert!(chart.max_value >= 8_000.0, "axis was {}", chart.max_value);
        assert!(
            chart.max_value < 100_000.0,
            "axis was absurdly high: {}",
            chart.max_value
        );
    }

    #[test]
    fn an_all_zero_series_still_gets_a_usable_axis() {
        // Otherwise an idle machine's chart looks broken rather than idle.
        let chart = build(
            &series_of(MetricKind::NetworkRxBytesPerSec, &[0.0, 0.0, 0.0]),
            100.0,
            100.0,
        );
        assert!(chart.max_value > 0.0);
        assert!(chart.has_data);
    }

    #[test]
    fn an_empty_series_produces_no_path_and_says_so() {
        let empty = MetricSeries::empty(
            MetricKind::CpuUsage,
            Resolution::Raw,
            TimeWindow::new(at(0), at(60)),
        );
        let chart = build(&empty, 300.0, 100.0);

        assert!(!chart.has_data);
        assert!(chart.line.is_empty());
        assert_eq!(chart.y_labels, vec!["no data".to_owned()]);
    }

    #[test]
    fn a_single_point_still_draws_something() {
        let chart = build(&series_of(MetricKind::CpuUsage, &[42.0]), 300.0, 100.0);
        assert!(chart.has_data);
        assert!(chart.line.starts_with("M "));
        assert_eq!(chart.x_labels.len(), 1);
    }

    #[test]
    fn a_zero_sized_chart_area_does_not_divide_by_zero() {
        let chart = build(&series_of(MetricKind::CpuUsage, &[10.0, 20.0]), 0.0, 0.0);
        assert!(!chart.has_data);
    }

    #[test]
    fn a_rollup_series_gets_a_min_max_band() {
        // The band is why rollups store min and max at all.
        let series = MetricSeries {
            kind: MetricKind::CpuUsage,
            resolution: Resolution::FiveMinutes,
            window: TimeWindow::new(at(0), at(600)),
            points: vec![
                SeriesPoint {
                    timestamp: at(0),
                    avg: 20.0,
                    min: 5.0,
                    max: 90.0,
                },
                SeriesPoint {
                    timestamp: at(300),
                    avg: 25.0,
                    min: 10.0,
                    max: 40.0,
                },
            ],
        };

        let chart = build(&series, 100.0, 100.0);
        assert!(!chart.band.is_empty());
        assert!(chart.band.ends_with('Z'), "the band must be a closed shape");
    }

    #[test]
    fn a_flat_series_gets_no_band() {
        // Raw samples have min == avg == max; a band would be an invisible zero-height
        // shape and pure wasted drawing.
        let chart = build(
            &series_of(MetricKind::CpuUsage, &[10.0, 20.0]),
            100.0,
            100.0,
        );
        assert!(chart.band.is_empty());
    }

    #[test]
    fn a_very_long_series_is_downsampled_but_keeps_its_spikes() {
        // The failure this guards against: a plain "every nth point" sample dropping the
        // one spike that mattered.
        let mut values = vec![10.0; 5_000];
        values[2_500] = 99.0;

        let chart = build(&series_of(MetricKind::CpuUsage, &values), 800.0, 200.0);
        let commands = chart.line.matches(['M', 'L']).count();
        assert!(commands <= MAX_DRAWN_POINTS, "{commands} points were drawn");

        // The spike survives: at 99% of a 0-100 axis its y is close to zero.
        let has_peak = chart
            .line
            .split_whitespace()
            .filter_map(|token| token.parse::<f32>().ok())
            .any(|value| value < 5.0);
        assert!(has_peak, "the spike was lost in downsampling");
    }

    #[test]
    fn y_labels_run_from_the_maximum_down_to_zero() {
        let chart = build(&series_of(MetricKind::CpuUsage, &[50.0]), 100.0, 100.0);
        assert_eq!(chart.y_labels.len(), Y_DIVISIONS + 1);
        assert_eq!(chart.y_labels.first().map(String::as_str), Some("100%"));
        assert_eq!(chart.y_labels.last().map(String::as_str), Some("0%"));
    }

    #[test]
    fn y_labels_are_formatted_in_the_metrics_own_unit() {
        let chart = build(
            &series_of(MetricKind::NetworkRxBytesPerSec, &[1_048_576.0]),
            100.0,
            100.0,
        );
        assert!(
            chart.y_labels.iter().any(|label| label.contains("/s")),
            "labels were {:?}",
            chart.y_labels
        );
    }

    #[test]
    fn a_short_window_gets_clock_labels_and_a_long_one_gets_dates() {
        let hourly = build(
            &series_of(MetricKind::CpuUsage, &[1.0, 2.0, 3.0]),
            100.0,
            100.0,
        );
        assert!(
            hourly.x_labels[0].contains(':'),
            "labels were {:?}",
            hourly.x_labels
        );

        let month = MetricSeries {
            kind: MetricKind::CpuUsage,
            resolution: Resolution::OneDay,
            window: TimeWindow::new(at(0), at(30 * 86_400)),
            points: (0..30)
                .map(|day| SeriesPoint::flat(at(0) + Duration::days(day), 10.0))
                .collect(),
        };
        let chart = build(&month, 100.0, 100.0);
        assert!(
            !chart.x_labels[0].contains(':'),
            "labels were {:?}",
            chart.x_labels
        );
    }

    #[test]
    fn x_labels_are_bounded_even_for_a_long_series() {
        let chart = build(
            &series_of(MetricKind::CpuUsage, &vec![1.0; 1_000]),
            800.0,
            200.0,
        );
        assert!(chart.x_labels.len() <= X_LABELS);
        assert!(!chart.x_labels.is_empty());
    }

    #[test]
    fn a_value_above_the_axis_maximum_is_clamped_rather_than_drawn_off_the_chart() {
        // A CPU reading of 101% across a sampling boundary must not paint outside the box.
        let chart = build(&series_of(MetricKind::CpuUsage, &[150.0]), 100.0, 100.0);
        let ys: Vec<f32> = chart
            .line
            .split_whitespace()
            .filter_map(|token| token.parse::<f32>().ok())
            .collect();
        assert!(
            ys.iter().all(|y| *y >= -0.01),
            "a point was drawn above the chart: {ys:?}"
        );
    }

    fn traffic_series(
        metric: vds_domain::analytics::AnalyticsMetric,
        values: &[f64],
    ) -> vds_domain::analytics::AnalyticsTimeSeries {
        use vds_domain::analytics::{AnalyticsInterval, AnalyticsPoint, DateRange};
        use vds_domain::ids::{ProviderId, WebsiteId};

        let range = DateRange::new(
            chrono::NaiveDate::from_ymd_opt(2026, 8, 20).expect("valid"),
            chrono::NaiveDate::from_ymd_opt(2026, 8, 26).expect("valid"),
        );
        vds_domain::analytics::AnalyticsTimeSeries {
            website_id: WebsiteId::new(),
            provider: ProviderId::new("stub"),
            metric,
            interval: AnalyticsInterval::Day,
            range,
            fetched_at: at(0),
            points: values
                .iter()
                .enumerate()
                .map(|(index, value)| AnalyticsPoint {
                    timestamp: at(index as i64 * 86_400),
                    value: *value,
                })
                .collect(),
        }
    }

    #[test]
    fn a_traffic_series_becomes_a_line_without_a_band() {
        // Analytics points carry one value each, so there is no spread to shade.
        let series = traffic_series(
            vds_domain::analytics::AnalyticsMetric::Visitors,
            &[10.0, 40.0, 25.0],
        );
        let geometry = build_analytics(&series, 100.0, 50.0);

        assert!(geometry.has_data);
        assert_eq!(geometry.line.matches('L').count() + 1, 3);
        assert!(
            geometry.band.is_empty(),
            "a flat series must not get a band"
        );
    }

    #[test]
    fn a_visitor_chart_scales_to_its_own_peak_not_to_a_hundred() {
        // Pinning a visitor count to a 0-100 axis would flatten every real site.
        let series = traffic_series(
            vds_domain::analytics::AnalyticsMetric::Visitors,
            &[1_000.0, 24_821.0],
        );
        let geometry = build_analytics(&series, 100.0, 50.0);
        assert!(geometry.max_value >= 24_821.0);
    }

    #[test]
    fn a_bounce_rate_chart_is_pinned_to_a_hundred_percent() {
        // So two rate charts side by side are directly comparable.
        let series = traffic_series(
            vds_domain::analytics::AnalyticsMetric::BounceRate,
            &[41.0, 43.0],
        );
        let geometry = build_analytics(&series, 100.0, 50.0);
        assert_eq!(geometry.max_value, 100.0);
        assert_eq!(geometry.y_labels.first().map(String::as_str), Some("100%"));
    }

    #[test]
    fn an_empty_traffic_series_says_so_rather_than_drawing_a_flat_line() {
        let series = traffic_series(vds_domain::analytics::AnalyticsMetric::Visitors, &[]);
        let geometry = build_analytics(&series, 100.0, 50.0);
        assert!(!geometry.has_data);
        assert!(geometry.line.is_empty());
    }
}
