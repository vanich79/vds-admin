//! Provider-independent web analytics.
//!
//! Nothing in this module knows that Yandex.Metrica exists. The UI works exclusively
//! with [`AnalyticsSnapshot`], [`AnalyticsTimeSeries`] and [`AnalyticsCapabilities`],
//! which is what allows a second provider to be added without touching the dashboard.
//! See `docs/adr/003-analytics-provider-architecture.md`.

use crate::ids::CredentialRef;
use crate::ids::{IntegrationId, ProviderId, WebsiteId};
use crate::metrics::{MetricValue, TimeWindow};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// The metrics the domain can talk about.
///
/// Providers map their own vocabulary onto this set and report
/// [`MetricValue::NotAvailable`] for anything they cannot serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsMetric {
    Visitors,
    Visits,
    PageViews,
    Sessions,
    UniqueVisitors,
    NewVisitors,
    ReturningVisitors,
    BounceRate,
    AverageSessionDuration,
    PagesPerSession,
}

impl AnalyticsMetric {
    pub fn as_str(self) -> &'static str {
        match self {
            AnalyticsMetric::Visitors => "visitors",
            AnalyticsMetric::Visits => "visits",
            AnalyticsMetric::PageViews => "page_views",
            AnalyticsMetric::Sessions => "sessions",
            AnalyticsMetric::UniqueVisitors => "unique_visitors",
            AnalyticsMetric::NewVisitors => "new_visitors",
            AnalyticsMetric::ReturningVisitors => "returning_visitors",
            AnalyticsMetric::BounceRate => "bounce_rate",
            AnalyticsMetric::AverageSessionDuration => "avg_session_duration",
            AnalyticsMetric::PagesPerSession => "pages_per_session",
        }
    }

    pub fn parse(raw: &str) -> Option<AnalyticsMetric> {
        let metric = match raw {
            "visitors" => AnalyticsMetric::Visitors,
            "visits" => AnalyticsMetric::Visits,
            "page_views" => AnalyticsMetric::PageViews,
            "sessions" => AnalyticsMetric::Sessions,
            "unique_visitors" => AnalyticsMetric::UniqueVisitors,
            "new_visitors" => AnalyticsMetric::NewVisitors,
            "returning_visitors" => AnalyticsMetric::ReturningVisitors,
            "bounce_rate" => AnalyticsMetric::BounceRate,
            "avg_session_duration" => AnalyticsMetric::AverageSessionDuration,
            "pages_per_session" => AnalyticsMetric::PagesPerSession,
            _ => return None,
        };
        Some(metric)
    }

    pub fn label(self) -> &'static str {
        match self {
            AnalyticsMetric::Visitors => "Visitors",
            AnalyticsMetric::Visits => "Visits",
            AnalyticsMetric::PageViews => "Page views",
            AnalyticsMetric::Sessions => "Sessions",
            AnalyticsMetric::UniqueVisitors => "Unique visitors",
            AnalyticsMetric::NewVisitors => "New visitors",
            AnalyticsMetric::ReturningVisitors => "Returning visitors",
            AnalyticsMetric::BounceRate => "Bounce rate",
            AnalyticsMetric::AverageSessionDuration => "Avg. session duration",
            AnalyticsMetric::PagesPerSession => "Pages per session",
        }
    }

    /// Whether summing across websites is meaningful.
    ///
    /// Totals can be added; rates and averages cannot, and a dashboard that sums bounce
    /// rates is lying. Aggregation code must consult this.
    /// The unit the metric is expressed in.
    ///
    /// Lives here rather than in the UI because it decides both how a value is formatted
    /// and how a chart axis is scaled, and those two must not be allowed to disagree.
    pub fn unit(self) -> crate::metrics::MetricUnit {
        match self {
            AnalyticsMetric::BounceRate => crate::metrics::MetricUnit::Percent,
            AnalyticsMetric::AverageSessionDuration => crate::metrics::MetricUnit::Seconds,
            // Pages per session is a bare ratio, not a percentage: 2.4 pages, not 240%.
            AnalyticsMetric::PagesPerSession => crate::metrics::MetricUnit::Ratio,
            _ => crate::metrics::MetricUnit::Count,
        }
    }

    pub fn is_additive(self) -> bool {
        matches!(
            self,
            AnalyticsMetric::Visitors
                | AnalyticsMetric::Visits
                | AnalyticsMetric::PageViews
                | AnalyticsMetric::Sessions
                | AnalyticsMetric::UniqueVisitors
                | AnalyticsMetric::NewVisitors
                | AnalyticsMetric::ReturningVisitors
        )
    }

    pub const ALL: &'static [AnalyticsMetric] = &[
        AnalyticsMetric::Visitors,
        AnalyticsMetric::Visits,
        AnalyticsMetric::PageViews,
        AnalyticsMetric::Sessions,
        AnalyticsMetric::UniqueVisitors,
        AnalyticsMetric::NewVisitors,
        AnalyticsMetric::ReturningVisitors,
        AnalyticsMetric::BounceRate,
        AnalyticsMetric::AverageSessionDuration,
        AnalyticsMetric::PagesPerSession,
    ];
}

impl fmt::Display for AnalyticsMetric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Bucket width for an analytics time series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsInterval {
    Hour,
    Day,
    Week,
    Month,
}

impl AnalyticsInterval {
    pub fn as_str(self) -> &'static str {
        match self {
            AnalyticsInterval::Hour => "hour",
            AnalyticsInterval::Day => "day",
            AnalyticsInterval::Week => "week",
            AnalyticsInterval::Month => "month",
        }
    }

    pub fn parse(raw: &str) -> Option<AnalyticsInterval> {
        match raw {
            "hour" => Some(AnalyticsInterval::Hour),
            "day" => Some(AnalyticsInterval::Day),
            "week" => Some(AnalyticsInterval::Week),
            "month" => Some(AnalyticsInterval::Month),
            _ => None,
        }
    }
}

/// The period an analytics query covers.
///
/// Analytics providers work in whole days, so this is expressed in dates rather than
/// instants — mixing the two is a common source of off-by-one traffic reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyticsPeriod {
    Today,
    Yesterday,
    LastSevenDays,
    LastThirtyDays,
    LastNinetyDays,
    Custom { from: NaiveDate, to: NaiveDate },
}

impl AnalyticsPeriod {
    /// Resolves to an inclusive date range in the viewer's frame of reference.
    pub fn resolve(self, today: NaiveDate) -> DateRange {
        match self {
            AnalyticsPeriod::Today => DateRange {
                from: today,
                to: today,
            },
            AnalyticsPeriod::Yesterday => {
                let yesterday = today.pred_opt().unwrap_or(today);
                DateRange {
                    from: yesterday,
                    to: yesterday,
                }
            }
            AnalyticsPeriod::LastSevenDays => DateRange {
                from: today.checked_sub_signed(Duration::days(6)).unwrap_or(today),
                to: today,
            },
            AnalyticsPeriod::LastThirtyDays => DateRange {
                from: today
                    .checked_sub_signed(Duration::days(29))
                    .unwrap_or(today),
                to: today,
            },
            AnalyticsPeriod::LastNinetyDays => DateRange {
                from: today
                    .checked_sub_signed(Duration::days(89))
                    .unwrap_or(today),
                to: today,
            },
            AnalyticsPeriod::Custom { from, to } => DateRange::new(from, to),
        }
    }

    /// Sensible bucket width for charting this period.
    pub fn natural_interval(self, today: NaiveDate) -> AnalyticsInterval {
        let days = self.resolve(today).days();
        match days {
            0..=2 => AnalyticsInterval::Hour,
            3..=60 => AnalyticsInterval::Day,
            61..=365 => AnalyticsInterval::Week,
            _ => AnalyticsInterval::Month,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AnalyticsPeriod::Today => "Today",
            AnalyticsPeriod::Yesterday => "Yesterday",
            AnalyticsPeriod::LastSevenDays => "7 days",
            AnalyticsPeriod::LastThirtyDays => "30 days",
            AnalyticsPeriod::LastNinetyDays => "90 days",
            AnalyticsPeriod::Custom { .. } => "Custom",
        }
    }

    pub const PRESETS: &'static [AnalyticsPeriod] = &[
        AnalyticsPeriod::Today,
        AnalyticsPeriod::Yesterday,
        AnalyticsPeriod::LastSevenDays,
        AnalyticsPeriod::LastThirtyDays,
        AnalyticsPeriod::LastNinetyDays,
    ];
}

/// An inclusive range of whole days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DateRange {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

impl DateRange {
    pub fn new(from: NaiveDate, to: NaiveDate) -> Self {
        if from <= to {
            Self { from, to }
        } else {
            Self { from: to, to: from }
        }
    }

    /// Number of days covered, inclusive of both ends.
    pub fn days(&self) -> i64 {
        (self.to - self.from).num_days() + 1
    }

    /// The equally long range immediately before this one, for comparisons.
    pub fn previous(&self) -> DateRange {
        let span = Duration::days(self.days());
        DateRange {
            from: self.from.checked_sub_signed(span).unwrap_or(self.from),
            to: self.to.checked_sub_signed(span).unwrap_or(self.to),
        }
    }
}

/// What a provider can actually do.
///
/// The UI reads this and hides unsupported features instead of rendering broken panels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsCapabilities {
    pub supported_metrics: Vec<AnalyticsMetric>,
    pub supports_time_series: bool,
    pub supports_top_pages: bool,
    pub supports_referrers: bool,
    pub supports_realtime: bool,
    /// Finest interval the provider will serve.
    pub min_interval: AnalyticsInterval,
    /// Furthest back the provider will serve data, in days. `None` means unlimited.
    pub max_history_days: Option<u32>,
}

impl AnalyticsCapabilities {
    pub fn supports(&self, metric: AnalyticsMetric) -> bool {
        self.supported_metrics.contains(&metric)
    }

    /// Minimal capability set: nothing but a total visitor count.
    pub fn minimal() -> Self {
        Self {
            supported_metrics: vec![AnalyticsMetric::Visitors],
            supports_time_series: false,
            supports_top_pages: false,
            supports_referrers: false,
            supports_realtime: false,
            min_interval: AnalyticsInterval::Day,
            max_history_days: None,
        }
    }
}

/// Aggregate figures for one website over one period.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsSnapshot {
    pub website_id: WebsiteId,
    pub provider: ProviderId,
    pub range: DateRange,
    /// When this snapshot was fetched, which the UI shows as "updated N minutes ago".
    pub fetched_at: DateTime<Utc>,
    metrics: BTreeMap<AnalyticsMetric, MetricValue>,
}

impl AnalyticsSnapshot {
    pub fn new(
        website_id: WebsiteId,
        provider: ProviderId,
        range: DateRange,
        fetched_at: DateTime<Utc>,
    ) -> Self {
        Self {
            website_id,
            provider,
            range,
            fetched_at,
            metrics: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, metric: AnalyticsMetric, value: MetricValue) {
        self.metrics.insert(metric, value);
    }

    pub fn with(mut self, metric: AnalyticsMetric, value: MetricValue) -> Self {
        self.set(metric, value);
        self
    }

    /// Reads a metric. A metric that was never set is [`MetricValue::NotAvailable`] —
    /// the same as one the provider explicitly could not serve, which is correct: in
    /// both cases we do not have the number.
    pub fn get(&self, metric: AnalyticsMetric) -> MetricValue {
        self.metrics
            .get(&metric)
            .copied()
            .unwrap_or(MetricValue::NotAvailable)
    }

    pub fn iter(&self) -> impl Iterator<Item = (AnalyticsMetric, MetricValue)> + '_ {
        self.metrics.iter().map(|(k, v)| (*k, *v))
    }

    pub fn is_empty(&self) -> bool {
        self.metrics.values().all(|v| !v.is_available())
    }
}

/// One point of an analytics series.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsPoint {
    pub timestamp: DateTime<Utc>,
    pub value: f64,
}

/// A time series for one metric of one website.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsTimeSeries {
    pub website_id: WebsiteId,
    pub provider: ProviderId,
    pub metric: AnalyticsMetric,
    pub interval: AnalyticsInterval,
    pub range: DateRange,
    pub fetched_at: DateTime<Utc>,
    pub points: Vec<AnalyticsPoint>,
}

impl AnalyticsTimeSeries {
    pub fn total(&self) -> f64 {
        self.points.iter().map(|p| p.value).sum()
    }

    pub fn peak(&self) -> Option<f64> {
        self.points
            .iter()
            .map(|p| p.value)
            .fold(None, |acc: Option<f64>, v| {
                Some(acc.map_or(v, |a| if v > a { v } else { a }))
            })
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// One row of a "top pages" report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopPage {
    pub url: String,
    pub page_views: f64,
    pub visitors: MetricValue,
}

/// One row of a traffic-sources report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Referrer {
    pub source: String,
    pub visits: f64,
    pub share_percent: MetricValue,
}

/// An analytics account exposed by a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsAccount {
    pub id: String,
    pub name: String,
}

/// A counter/property/site within a provider account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsCounter {
    pub id: String,
    pub name: String,
    /// The site this counter tracks, when the provider reports one.
    pub site_url: Option<String>,
}

/// Binds one website to one analytics provider.
///
/// `settings` is deliberately an opaque, versioned JSON blob: provider-specific
/// configuration must not leak into the schema. See ADR-003.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyticsIntegration {
    pub id: IntegrationId,
    pub website_id: WebsiteId,
    pub provider: ProviderId,
    /// The provider's identifier for the tracked entity — a Metrica counter ID, a GA4
    /// property ID, a Plausible site ID.
    pub external_id: String,
    /// Handle into the secret store holding the OAuth token or API key.
    pub credential_ref: CredentialRef,
    pub enabled: bool,
    /// Minutes between refreshes.
    pub refresh_interval_mins: u32,
    pub settings: ProviderSettings,
    pub created_at: DateTime<Utc>,
}

pub const DEFAULT_ANALYTICS_REFRESH_MINS: u32 = 15;

impl AnalyticsIntegration {
    pub fn new(
        website_id: WebsiteId,
        provider: ProviderId,
        external_id: impl Into<String>,
        credential_ref: CredentialRef,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: IntegrationId::new(),
            website_id,
            provider,
            external_id: external_id.into(),
            credential_ref,
            enabled: true,
            refresh_interval_mins: DEFAULT_ANALYTICS_REFRESH_MINS,
            settings: ProviderSettings::empty(),
            created_at: now,
        }
    }

    pub fn refresh_interval(&self) -> Duration {
        Duration::minutes(i64::from(self.refresh_interval_mins.max(1)))
    }

    pub fn validate(&self) -> Result<(), IntegrationValidationError> {
        if self.external_id.trim().is_empty() {
            return Err(IntegrationValidationError::MissingExternalId);
        }
        if self.refresh_interval_mins == 0 {
            return Err(IntegrationValidationError::InvalidRefreshInterval);
        }
        Ok(())
    }
}

/// Why an integration was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IntegrationValidationError {
    #[error("the provider's counter or property identifier must not be empty")]
    MissingExternalId,
    #[error("refresh interval must be at least 1 minute")]
    InvalidRefreshInterval,
}

/// Versioned, provider-specific configuration.
///
/// Versioning it at the type level is what makes a future settings migration a
/// contained change rather than a schema rewrite.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSettings {
    pub version: u32,
    pub values: serde_json::Value,
}

impl ProviderSettings {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn empty() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            values: serde_json::Value::Object(Default::default()),
        }
    }

    pub fn new(values: serde_json::Value) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            values,
        }
    }

    /// Reads a string setting, if present.
    pub fn string(&self, key: &str) -> Option<&str> {
        self.values.get(key).and_then(serde_json::Value::as_str)
    }

    /// Reads a boolean setting, falling back to `default` when absent or of the wrong
    /// type — a malformed setting must not crash a monitoring run.
    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        self.values
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default)
    }
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self::empty()
    }
}

/// Verdict of the traffic anomaly detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficTrend {
    Normal,
    Warning,
    Anomaly,
    /// Not enough history to judge. Deliberately distinct from `Normal`.
    Insufficient,
}

/// A period-over-period traffic comparison.
///
/// The language is deliberately non-causal: this type reports that traffic changed, not
/// why. Attribution belongs to the correlation engine, and even there only as a
/// *possible* correlation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrafficComparison {
    pub website_id: WebsiteId,
    pub metric: AnalyticsMetric,
    pub current: f64,
    pub baseline: f64,
    pub change_percent: f64,
    pub trend: TrafficTrend,
    pub window: TimeWindow,
}

impl TrafficComparison {
    pub fn is_drop(&self) -> bool {
        self.change_percent < 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
    }

    #[test]
    fn every_metric_round_trips_through_its_stored_form() {
        for metric in AnalyticsMetric::ALL {
            assert_eq!(AnalyticsMetric::parse(metric.as_str()), Some(*metric));
        }
    }

    #[test]
    fn rates_and_averages_are_not_additive() {
        assert_eq!(
            AnalyticsMetric::Visitors.unit(),
            crate::metrics::MetricUnit::Count
        );
        assert!(AnalyticsMetric::Visitors.is_additive());
        assert!(AnalyticsMetric::PageViews.is_additive());
        assert!(!AnalyticsMetric::BounceRate.is_additive());
        assert!(!AnalyticsMetric::AverageSessionDuration.is_additive());
        assert!(!AnalyticsMetric::PagesPerSession.is_additive());
    }

    #[test]
    fn periods_resolve_to_inclusive_ranges() {
        let today = day(2026, 8, 26);
        assert_eq!(
            AnalyticsPeriod::Today.resolve(today),
            DateRange {
                from: today,
                to: today
            }
        );
        assert_eq!(
            AnalyticsPeriod::Yesterday.resolve(today),
            DateRange {
                from: day(2026, 8, 25),
                to: day(2026, 8, 25)
            }
        );
        // Seven days means today plus the six before it, not today minus seven.
        let week = AnalyticsPeriod::LastSevenDays.resolve(today);
        assert_eq!(
            week,
            DateRange {
                from: day(2026, 8, 20),
                to: today
            }
        );
        assert_eq!(week.days(), 7);
    }

    #[test]
    fn thirty_and_ninety_day_periods_have_the_advertised_length() {
        let today = day(2026, 8, 26);
        assert_eq!(AnalyticsPeriod::LastThirtyDays.resolve(today).days(), 30);
        assert_eq!(AnalyticsPeriod::LastNinetyDays.resolve(today).days(), 90);
    }

    #[test]
    fn previous_range_abuts_and_matches_length() {
        let range = DateRange::new(day(2026, 8, 20), day(2026, 8, 26));
        let previous = range.previous();
        assert_eq!(previous.days(), range.days());
        assert_eq!(previous.to, day(2026, 8, 19));
        assert_eq!(previous.from, day(2026, 8, 13));
    }

    #[test]
    fn custom_range_normalises_reversed_dates() {
        let range = AnalyticsPeriod::Custom {
            from: day(2026, 8, 26),
            to: day(2026, 8, 20),
        }
        .resolve(day(2026, 8, 26));
        assert_eq!(range.from, day(2026, 8, 20));
        assert_eq!(range.to, day(2026, 8, 26));
    }

    #[test]
    fn natural_interval_widens_with_the_period() {
        let today = day(2026, 8, 26);
        assert_eq!(
            AnalyticsPeriod::Today.natural_interval(today),
            AnalyticsInterval::Hour
        );
        assert_eq!(
            AnalyticsPeriod::LastSevenDays.natural_interval(today),
            AnalyticsInterval::Day
        );
        assert_eq!(
            AnalyticsPeriod::LastNinetyDays.natural_interval(today),
            AnalyticsInterval::Week
        );
    }

    #[test]
    fn an_unset_metric_reads_as_unavailable_never_zero() {
        let snapshot = AnalyticsSnapshot::new(
            WebsiteId::new(),
            ProviderId::new("test"),
            DateRange::new(day(2026, 8, 1), day(2026, 8, 2)),
            DateTime::UNIX_EPOCH,
        );
        assert_eq!(
            snapshot.get(AnalyticsMetric::Visitors),
            MetricValue::NotAvailable
        );
        assert!(snapshot.is_empty());
    }

    #[test]
    fn an_explicitly_unavailable_metric_stays_unavailable() {
        let snapshot = AnalyticsSnapshot::new(
            WebsiteId::new(),
            ProviderId::new("test"),
            DateRange::new(day(2026, 8, 1), day(2026, 8, 2)),
            DateTime::UNIX_EPOCH,
        )
        .with(AnalyticsMetric::Visitors, MetricValue::Available(10.0))
        .with(AnalyticsMetric::BounceRate, MetricValue::NotAvailable);

        assert_eq!(
            snapshot.get(AnalyticsMetric::Visitors),
            MetricValue::Available(10.0)
        );
        assert_eq!(
            snapshot.get(AnalyticsMetric::BounceRate),
            MetricValue::NotAvailable
        );
        assert!(!snapshot.is_empty());
    }

    #[test]
    fn capabilities_gate_metric_availability() {
        let caps = AnalyticsCapabilities::minimal();
        assert!(caps.supports(AnalyticsMetric::Visitors));
        assert!(!caps.supports(AnalyticsMetric::BounceRate));
    }

    #[test]
    fn integration_requires_a_counter_id() {
        let mut integration = AnalyticsIntegration::new(
            WebsiteId::new(),
            ProviderId::new("yandex_metrica"),
            "  ",
            CredentialRef::new(),
            DateTime::UNIX_EPOCH,
        );
        assert_eq!(
            integration.validate(),
            Err(IntegrationValidationError::MissingExternalId)
        );

        integration.external_id = "12345".into();
        assert_eq!(integration.validate(), Ok(()));
    }

    #[test]
    fn provider_settings_tolerate_missing_and_mistyped_keys() {
        let settings = ProviderSettings::new(serde_json::json!({ "goal_id": "42", "bad": 7 }));
        assert_eq!(settings.string("goal_id"), Some("42"));
        assert_eq!(settings.string("absent"), None);
        assert!(settings.bool_or("bad", true));
        assert!(!settings.bool_or("absent", false));
    }

    #[test]
    fn time_series_totals_and_peaks() {
        let series = AnalyticsTimeSeries {
            website_id: WebsiteId::new(),
            provider: ProviderId::new("test"),
            metric: AnalyticsMetric::Visitors,
            interval: AnalyticsInterval::Day,
            range: DateRange::new(day(2026, 8, 1), day(2026, 8, 3)),
            fetched_at: DateTime::UNIX_EPOCH,
            points: vec![
                AnalyticsPoint {
                    timestamp: DateTime::UNIX_EPOCH,
                    value: 10.0,
                },
                AnalyticsPoint {
                    timestamp: DateTime::UNIX_EPOCH,
                    value: 30.0,
                },
                AnalyticsPoint {
                    timestamp: DateTime::UNIX_EPOCH,
                    value: 20.0,
                },
            ],
        };
        assert_eq!(series.total(), 60.0);
        assert_eq!(series.peak(), Some(30.0));
    }

    #[test]
    fn a_rate_is_a_percentage_and_a_duration_is_seconds() {
        // The axis scaling depends on this: a percentage chart is pinned to 0-100, and
        // pinning a visitor count to 100 would flatten every real site.
        assert_eq!(
            AnalyticsMetric::BounceRate.unit(),
            crate::metrics::MetricUnit::Percent
        );
        assert_eq!(
            AnalyticsMetric::AverageSessionDuration.unit(),
            crate::metrics::MetricUnit::Seconds
        );
        assert_eq!(
            AnalyticsMetric::PagesPerSession.unit(),
            crate::metrics::MetricUnit::Ratio
        );
        assert_eq!(
            AnalyticsMetric::PageViews.unit(),
            crate::metrics::MetricUnit::Count
        );
    }

    #[test]
    fn no_additive_metric_is_a_percentage() {
        // Summing percentages across sites is exactly the bug `is_additive` prevents;
        // the two must never disagree.
        for metric in [
            AnalyticsMetric::Visitors,
            AnalyticsMetric::Visits,
            AnalyticsMetric::PageViews,
            AnalyticsMetric::Sessions,
            AnalyticsMetric::UniqueVisitors,
            AnalyticsMetric::NewVisitors,
            AnalyticsMetric::ReturningVisitors,
            AnalyticsMetric::BounceRate,
            AnalyticsMetric::AverageSessionDuration,
            AnalyticsMetric::PagesPerSession,
        ] {
            if metric.is_additive() {
                assert_eq!(
                    metric.unit(),
                    crate::metrics::MetricUnit::Count,
                    "{metric:?} is summed across websites, so it must be a count"
                );
            }
        }
    }
}
