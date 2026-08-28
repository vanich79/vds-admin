//! Traffic anomaly detection.
//!
//! Deliberately simple and explainable: compare the current period against a baseline
//! and report the percentage change. No statistics, no models, no machine learning —
//! those can be added later behind the same [`TrafficAnomalyDetector`] interface, and
//! the brief is explicit that a simple, testable model comes first.
//!
//! The one thing this module refuses to do is claim causation. It reports that traffic
//! changed; whether a server incident *caused* it is a question for the correlation
//! engine, which itself only ever says "possible".

use vds_domain::analytics::{
    AnalyticsMetric, AnalyticsTimeSeries, TrafficComparison, TrafficTrend,
};
use vds_domain::ids::WebsiteId;
use vds_domain::metrics::TimeWindow;

/// How the baseline is computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaselineStrategy {
    /// Compare against the immediately preceding period of equal length.
    ///
    /// Simple, but sensitive to weekly seasonality: comparing Monday with Sunday will
    /// look like a spike on most sites.
    PreviousPeriod,
    /// Compare against the mean of the preceding points, excluding the current one.
    ///
    /// Smoother than previous-period, but a mean is still dragged around by a single
    /// extreme value: one 5x spike in a four-day history moves the baseline by 100%.
    MovingAverage { window: usize },
    /// Compare against the median of the preceding points.
    ///
    /// The default, because it is the only one of the three that genuinely ignores a
    /// single unusual day - a launch, an outage, a bot crawl - instead of letting it
    /// redefine "normal" for the next week.
    MovingMedian { window: usize },
}

/// Configuration for the detector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnomalyConfig {
    /// Percentage change that counts as an anomaly.
    pub threshold_percent: f64,
    /// Percentage change that counts as merely worth noticing.
    pub warning_percent: f64,
    pub strategy: BaselineStrategy,
    /// Baselines below this are ignored.
    ///
    /// Percentage change is meaningless on tiny numbers: going from 2 visitors to 1 is a
    /// 50% drop and tells you nothing.
    pub minimum_baseline: f64,
    /// Points required before any judgement is made.
    pub minimum_points: usize,
}

impl Default for AnomalyConfig {
    fn default() -> Self {
        Self {
            threshold_percent: 30.0,
            warning_percent: 15.0,
            strategy: BaselineStrategy::MovingMedian { window: 7 },
            minimum_baseline: 20.0,
            minimum_points: 3,
        }
    }
}

/// Compares traffic against a baseline.
#[derive(Debug, Clone, Copy)]
pub struct TrafficAnomalyDetector {
    config: AnomalyConfig,
}

impl TrafficAnomalyDetector {
    pub fn new(config: AnomalyConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &AnomalyConfig {
        &self.config
    }

    /// Judges the most recent point of a series.
    ///
    /// Returns `None` when there is not enough data to say anything, which is a
    /// different and more honest answer than "normal".
    pub fn detect(&self, series: &AnalyticsTimeSeries) -> Option<TrafficComparison> {
        if series.points.len() < self.config.minimum_points {
            return None;
        }

        let (current_point, history) = series.points.split_last()?;
        let current = current_point.value;
        let baseline = self.baseline(history)?;

        if baseline < self.config.minimum_baseline {
            // Too little traffic for a percentage to mean anything.
            return Some(TrafficComparison {
                website_id: series.website_id,
                metric: series.metric,
                current,
                baseline,
                change_percent: 0.0,
                trend: TrafficTrend::Insufficient,
                window: window_of(series),
            });
        }

        let change_percent = (current - baseline) / baseline * 100.0;
        if !change_percent.is_finite() {
            return None;
        }

        Some(TrafficComparison {
            website_id: series.website_id,
            metric: series.metric,
            current,
            baseline,
            change_percent,
            trend: self.classify(change_percent),
            window: window_of(series),
        })
    }

    /// Compares two explicit totals, for the "compared to previous period" figure.
    pub fn compare(
        &self,
        website_id: WebsiteId,
        metric: AnalyticsMetric,
        current: f64,
        baseline: f64,
        window: TimeWindow,
    ) -> TrafficComparison {
        let (change_percent, trend) = if baseline < self.config.minimum_baseline {
            (0.0, TrafficTrend::Insufficient)
        } else {
            let change = (current - baseline) / baseline * 100.0;
            if change.is_finite() {
                (change, self.classify(change))
            } else {
                (0.0, TrafficTrend::Insufficient)
            }
        };

        TrafficComparison {
            website_id,
            metric,
            current,
            baseline,
            change_percent,
            trend,
            window,
        }
    }

    fn baseline(&self, history: &[vds_domain::analytics::AnalyticsPoint]) -> Option<f64> {
        match self.config.strategy {
            BaselineStrategy::PreviousPeriod => history.last().map(|p| p.value),
            BaselineStrategy::MovingAverage { window } => {
                let considered = tail(history, window);
                if considered.is_empty() {
                    return None;
                }
                let sum: f64 = considered.iter().map(|p| p.value).sum();
                Some(sum / considered.len() as f64)
            }
            BaselineStrategy::MovingMedian { window } => {
                let considered = tail(history, window);
                if considered.is_empty() {
                    return None;
                }
                let mut values: Vec<f64> = considered.iter().map(|p| p.value).collect();
                values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                Some(median_of_sorted(&values))
            }
        }
    }

    fn classify(&self, change_percent: f64) -> TrafficTrend {
        let magnitude = change_percent.abs();
        if magnitude >= self.config.threshold_percent {
            TrafficTrend::Anomaly
        } else if magnitude >= self.config.warning_percent {
            TrafficTrend::Warning
        } else {
            TrafficTrend::Normal
        }
    }
}

impl Default for TrafficAnomalyDetector {
    fn default() -> Self {
        Self::new(AnomalyConfig::default())
    }
}

/// The last `window` points, or all of them when there are fewer.
fn tail(
    history: &[vds_domain::analytics::AnalyticsPoint],
    window: usize,
) -> &[vds_domain::analytics::AnalyticsPoint] {
    let window = window.max(1);
    &history[history.len().saturating_sub(window)..]
}

/// Median of an already-sorted slice. Emptiness is the caller's responsibility.
fn median_of_sorted(values: &[f64]) -> f64 {
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values.get(middle).copied().unwrap_or(0.0)
    } else {
        let lower = values.get(middle.saturating_sub(1)).copied().unwrap_or(0.0);
        let upper = values.get(middle).copied().unwrap_or(0.0);
        (lower + upper) / 2.0
    }
}

fn window_of(series: &AnalyticsTimeSeries) -> TimeWindow {
    let from = series
        .points
        .first()
        .map(|p| p.timestamp)
        .unwrap_or(series.fetched_at);
    let to = series
        .points
        .last()
        .map(|p| p.timestamp)
        .unwrap_or(series.fetched_at);
    TimeWindow::new(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};
    use vds_domain::analytics::{AnalyticsInterval, AnalyticsPoint, DateRange};
    use vds_domain::ids::ProviderId;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn series(values: &[f64]) -> AnalyticsTimeSeries {
        AnalyticsTimeSeries {
            website_id: WebsiteId::new(),
            provider: ProviderId::new("test"),
            metric: AnalyticsMetric::Visitors,
            interval: AnalyticsInterval::Day,
            range: DateRange::new(
                chrono::NaiveDate::from_ymd_opt(2026, 8, 1).expect("valid"),
                chrono::NaiveDate::from_ymd_opt(2026, 8, 10).expect("valid"),
            ),
            fetched_at: at(0),
            points: values
                .iter()
                .enumerate()
                .map(|(i, v)| AnalyticsPoint {
                    timestamp: at(0) + Duration::days(i as i64),
                    value: *v,
                })
                .collect(),
        }
    }

    #[test]
    fn steady_traffic_is_normal() {
        let detector = TrafficAnomalyDetector::default();
        let comparison = detector
            .detect(&series(&[1_000.0, 1_010.0, 990.0, 1_005.0, 1_000.0]))
            .expect("enough data");
        assert_eq!(comparison.trend, TrafficTrend::Normal);
        assert!(comparison.change_percent.abs() < 5.0);
    }

    #[test]
    fn a_large_drop_is_an_anomaly() {
        // The example from the brief: 10 000 expected, 6 500 actual.
        let detector = TrafficAnomalyDetector::default();
        let comparison = detector
            .detect(&series(&[10_000.0, 10_000.0, 10_000.0, 10_000.0, 6_500.0]))
            .expect("enough data");

        assert_eq!(comparison.trend, TrafficTrend::Anomaly);
        assert!(comparison.is_drop());
        assert!((comparison.change_percent - -35.0).abs() < 0.01);
        assert_eq!(comparison.current, 6_500.0);
        assert_eq!(comparison.baseline, 10_000.0);
    }

    #[test]
    fn a_moderate_change_is_only_a_warning() {
        let detector = TrafficAnomalyDetector::default();
        let comparison = detector
            .detect(&series(&[1_000.0, 1_000.0, 1_000.0, 1_000.0, 800.0]))
            .expect("enough data");
        assert_eq!(comparison.trend, TrafficTrend::Warning);
    }

    #[test]
    fn a_large_increase_is_also_an_anomaly() {
        // A traffic spike matters as much as a drop — it might be a launch, or a bot.
        let detector = TrafficAnomalyDetector::default();
        let comparison = detector
            .detect(&series(&[1_000.0, 1_000.0, 1_000.0, 1_000.0, 5_000.0]))
            .expect("enough data");
        assert_eq!(comparison.trend, TrafficTrend::Anomaly);
        assert!(!comparison.is_drop());
    }

    #[test]
    fn too_few_points_yields_no_judgement_at_all() {
        // "Not enough data" and "normal" are different answers.
        let detector = TrafficAnomalyDetector::default();
        assert_eq!(detector.detect(&series(&[100.0])), None);
        assert_eq!(detector.detect(&series(&[100.0, 100.0])), None);
        assert!(detector.detect(&series(&[100.0, 100.0, 100.0])).is_some());
    }

    #[test]
    fn tiny_traffic_is_reported_as_insufficient_rather_than_as_a_50_percent_crash() {
        // Two visitors dropping to one is not an incident.
        let detector = TrafficAnomalyDetector::default();
        let comparison = detector
            .detect(&series(&[2.0, 2.0, 2.0, 1.0]))
            .expect("enough points");
        assert_eq!(comparison.trend, TrafficTrend::Insufficient);
        assert_eq!(comparison.change_percent, 0.0);
    }

    #[test]
    fn a_zero_baseline_does_not_produce_an_infinite_change() {
        let detector = TrafficAnomalyDetector::default();
        let comparison = detector
            .detect(&series(&[0.0, 0.0, 0.0, 500.0]))
            .expect("enough points");
        assert_eq!(comparison.trend, TrafficTrend::Insufficient);
        assert!(comparison.change_percent.is_finite());
    }

    #[test]
    fn the_median_baseline_absorbs_a_single_unusual_day() {
        // A one-day spike in the history must not make the next normal day look like a
        // crash. This is why the median is the default: neither of the other two
        // strategies survives this input.
        let data = series(&[1_000.0, 1_000.0, 1_000.0, 5_000.0, 1_000.0]);

        let median = TrafficAnomalyDetector::new(AnomalyConfig {
            strategy: BaselineStrategy::MovingMedian { window: 7 },
            ..Default::default()
        });
        let comparison = median.detect(&data).expect("enough data");
        assert_eq!(comparison.baseline, 1_000.0);
        assert_eq!(comparison.trend, TrafficTrend::Normal);

        // A mean is dragged to 2000 by the spike, making a perfectly normal day look
        // like a 50% collapse.
        let mean = TrafficAnomalyDetector::new(AnomalyConfig {
            strategy: BaselineStrategy::MovingAverage { window: 7 },
            ..Default::default()
        });
        assert_eq!(
            mean.detect(&data).expect("enough data").trend,
            TrafficTrend::Anomaly
        );

        // Previous-period compares against the spike itself: also a false alarm.
        let previous = TrafficAnomalyDetector::new(AnomalyConfig {
            strategy: BaselineStrategy::PreviousPeriod,
            ..Default::default()
        });
        assert_eq!(
            previous.detect(&data).expect("enough data").trend,
            TrafficTrend::Anomaly
        );
    }

    #[test]
    fn the_median_still_reports_a_genuine_sustained_drop() {
        // Robustness must not become blindness.
        let detector = TrafficAnomalyDetector::default();
        let comparison = detector
            .detect(&series(&[1_000.0, 1_000.0, 1_000.0, 1_000.0, 400.0]))
            .expect("enough data");
        assert_eq!(comparison.trend, TrafficTrend::Anomaly);
        assert!(comparison.is_drop());
    }

    #[test]
    fn the_median_of_an_even_number_of_points_averages_the_middle_two() {
        let detector = TrafficAnomalyDetector::new(AnomalyConfig {
            strategy: BaselineStrategy::MovingMedian { window: 7 },
            ..Default::default()
        });
        // History [100, 200, 300, 400] gives a median of 250.
        let comparison = detector
            .detect(&series(&[100.0, 200.0, 300.0, 400.0, 250.0]))
            .expect("enough data");
        assert_eq!(comparison.baseline, 250.0);
        assert_eq!(comparison.change_percent, 0.0);
    }

    #[test]
    fn the_baseline_window_is_bounded_by_the_available_history() {
        let detector = TrafficAnomalyDetector::new(AnomalyConfig {
            strategy: BaselineStrategy::MovingAverage { window: 30 },
            ..Default::default()
        });
        // Only four points of history exist; the detector must use what there is.
        let comparison = detector
            .detect(&series(&[100.0, 100.0, 100.0, 100.0, 50.0]))
            .expect("enough data");
        assert_eq!(comparison.baseline, 100.0);
        assert_eq!(comparison.trend, TrafficTrend::Anomaly);
    }

    #[test]
    fn thresholds_are_configurable() {
        let lenient = TrafficAnomalyDetector::new(AnomalyConfig {
            threshold_percent: 80.0,
            warning_percent: 60.0,
            ..Default::default()
        });
        let comparison = lenient
            .detect(&series(&[1_000.0, 1_000.0, 1_000.0, 650.0]))
            .expect("enough data");
        assert_eq!(comparison.trend, TrafficTrend::Normal);
    }

    #[test]
    fn direct_comparison_matches_the_series_based_result() {
        let detector = TrafficAnomalyDetector::default();
        let comparison = detector.compare(
            WebsiteId::new(),
            AnalyticsMetric::Visitors,
            6_500.0,
            10_000.0,
            TimeWindow::new(at(0), at(86_400)),
        );
        assert_eq!(comparison.trend, TrafficTrend::Anomaly);
        assert!((comparison.change_percent - -35.0).abs() < 0.01);
    }

    #[test]
    fn direct_comparison_against_a_tiny_baseline_is_insufficient() {
        let detector = TrafficAnomalyDetector::default();
        let comparison = detector.compare(
            WebsiteId::new(),
            AnalyticsMetric::Visitors,
            1.0,
            3.0,
            TimeWindow::new(at(0), at(86_400)),
        );
        assert_eq!(comparison.trend, TrafficTrend::Insufficient);
    }
}
