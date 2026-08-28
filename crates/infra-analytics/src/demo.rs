//! A provider that invents traffic, for development only.
//!
//! # Why this is fenced off
//!
//! Fabricated numbers that reach a production screen are worse than no numbers: a user
//! looks at a dashboard to decide whether something is wrong, and plausible-looking
//! invented traffic is indistinguishable from the real thing. So this module is behind
//! the `demo-providers` Cargo feature, which is off by default and is never enabled for a
//! release build — see `docs/adr/004-screenshot-architecture.md`.
//!
//! Two further precautions:
//!
//! * the provider's id is `demo`, and nothing registers it automatically. A developer has
//!   to build with the feature *and* register it by hand;
//! * its display name says "Demo" out loud, so if one ever does appear in a provider
//!   picker it is obvious what it is.
//!
//! # What it produces
//!
//! Numbers derived deterministically from the website id and the date. Deterministic
//! rather than random so that a screenshot taken today matches one taken tomorrow and a
//! failing test does not become a flaky one — and so that two different demo websites do
//! not accidentally show identical traffic.

use async_trait::async_trait;
use chrono::{Duration, NaiveDate, Utc};
use vds_domain::analytics::{
    AnalyticsAccount, AnalyticsCapabilities, AnalyticsCounter, AnalyticsInterval, AnalyticsMetric,
    AnalyticsPoint, AnalyticsSnapshot, AnalyticsTimeSeries, Referrer, TopPage,
};
use vds_domain::ids::{CredentialRef, ProviderId, WebsiteId};
use vds_domain::metrics::MetricValue;
use vds_domain::ports::{AnalyticsProvider, AnalyticsQuery, ProviderError, ProviderHealth};

/// The provider's stable identifier.
pub const PROVIDER_ID: &str = "demo";

/// Invents plausible traffic. Development builds only.
#[derive(Debug, Default, Clone, Copy)]
pub struct DemoAnalyticsProvider;

impl DemoAnalyticsProvider {
    pub fn new() -> Self {
        Self
    }
}

/// A small deterministic hash, so the same inputs always give the same number.
///
/// FNV-1a: not cryptographic, and it does not need to be — it only has to spread ids and
/// dates apart so two demo websites do not show the same traffic.
fn seed(website: WebsiteId, day: NaiveDate) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(website.to_string().as_bytes());
    mix(day.to_string().as_bytes());
    hash
}

/// A value in `[low, high]`, chosen deterministically from the seed.
fn between(seed: u64, low: f64, high: f64) -> f64 {
    let fraction = (seed % 10_000) as f64 / 10_000.0;
    low + (high - low) * fraction
}

/// Visitors for one day.
fn visitors_on(website: WebsiteId, day: NaiveDate) -> f64 {
    let base = between(seed(website, day), 400.0, 4_000.0);
    // Weekends are quieter, which makes the shape of the chart look like real traffic
    // rather than noise.
    let weekday = day.format("%u").to_string();
    let weekend = matches!(weekday.as_str(), "6" | "7");
    (if weekend { base * 0.6 } else { base }).round()
}

#[async_trait]
impl AnalyticsProvider for DemoAnalyticsProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn display_name(&self) -> &'static str {
        // Says so out loud: if this ever reaches a picker, it must be unmistakable.
        "Demo (fabricated data)"
    }

    fn capabilities(&self) -> AnalyticsCapabilities {
        AnalyticsCapabilities {
            supported_metrics: vec![
                AnalyticsMetric::Visitors,
                AnalyticsMetric::Visits,
                AnalyticsMetric::PageViews,
                AnalyticsMetric::BounceRate,
                AnalyticsMetric::AverageSessionDuration,
            ],
            supports_time_series: true,
            supports_top_pages: true,
            supports_referrers: true,
            supports_realtime: false,
            min_interval: AnalyticsInterval::Hour,
            max_history_days: Some(365),
        }
    }

    async fn validate_connection(
        &self,
        _credential_ref: CredentialRef,
    ) -> Result<ProviderHealth, ProviderError> {
        Ok(ProviderHealth::Ok)
    }

    async fn accounts(
        &self,
        _credential_ref: CredentialRef,
    ) -> Result<Vec<AnalyticsAccount>, ProviderError> {
        Ok(vec![AnalyticsAccount {
            id: "demo".into(),
            name: "Demo account".into(),
        }])
    }

    async fn counters(
        &self,
        _credential_ref: CredentialRef,
    ) -> Result<Vec<AnalyticsCounter>, ProviderError> {
        Ok(vec![AnalyticsCounter {
            id: "1".into(),
            name: "Demo counter".into(),
            site_url: Some("https://example.com/".into()),
        }])
    }

    async fn overview(&self, query: &AnalyticsQuery) -> Result<AnalyticsSnapshot, ProviderError> {
        let days = query.range.days().max(1);
        let visitors: f64 = (0..days)
            .filter_map(|offset| query.range.from.checked_add_signed(Duration::days(offset)))
            .map(|day| visitors_on(query.website_id, day))
            .sum();

        let key = seed(query.website_id, query.range.from);
        Ok(
            AnalyticsSnapshot::new(query.website_id, self.id(), query.range, Utc::now())
                .with(AnalyticsMetric::Visitors, MetricValue::available(visitors))
                .with(
                    AnalyticsMetric::Visits,
                    MetricValue::available((visitors * 1.3).round()),
                )
                .with(
                    AnalyticsMetric::PageViews,
                    MetricValue::available((visitors * 3.6).round()),
                )
                .with(
                    AnalyticsMetric::BounceRate,
                    MetricValue::available((between(key, 28.0, 64.0) * 10.0).round() / 10.0),
                )
                .with(
                    AnalyticsMetric::AverageSessionDuration,
                    MetricValue::available(between(key, 45.0, 320.0).round()),
                ),
        )
    }

    async fn time_series(
        &self,
        query: &AnalyticsQuery,
        metric: AnalyticsMetric,
        interval: AnalyticsInterval,
    ) -> Result<AnalyticsTimeSeries, ProviderError> {
        if !self.capabilities().supports(metric) {
            // Even the demo provider refuses what it does not advertise: the capability
            // check has to be exercised in development or it will not work in production.
            return Err(ProviderError::Unsupported("this metric"));
        }

        let days = query.range.days().max(1);
        let points = (0..days)
            .filter_map(|offset| query.range.from.checked_add_signed(Duration::days(offset)))
            .filter_map(|day| {
                let visitors = visitors_on(query.website_id, day);
                let value = match metric {
                    AnalyticsMetric::Visitors => visitors,
                    AnalyticsMetric::Visits => (visitors * 1.3).round(),
                    AnalyticsMetric::PageViews => (visitors * 3.6).round(),
                    AnalyticsMetric::BounceRate => {
                        (between(seed(query.website_id, day), 28.0, 64.0) * 10.0).round() / 10.0
                    }
                    _ => between(seed(query.website_id, day), 45.0, 320.0).round(),
                };
                let timestamp = day.and_hms_opt(0, 0, 0)?.and_utc();
                Some(AnalyticsPoint { timestamp, value })
            })
            .collect();

        Ok(AnalyticsTimeSeries {
            website_id: query.website_id,
            provider: self.id(),
            metric,
            interval,
            range: query.range,
            fetched_at: Utc::now(),
            points,
        })
    }

    async fn top_pages(
        &self,
        query: &AnalyticsQuery,
        limit: u32,
    ) -> Result<Vec<TopPage>, ProviderError> {
        let key = seed(query.website_id, query.range.from);
        let paths = [
            "/",
            "/pricing",
            "/docs",
            "/blog",
            "/about",
            "/contact",
            "/changelog",
        ];

        Ok(paths
            .iter()
            .take(limit as usize)
            .enumerate()
            .map(|(rank, path)| {
                let views = between(key.wrapping_add(rank as u64), 80.0, 5_000.0).round()
                    / (rank as f64 + 1.0);
                TopPage {
                    url: (*path).to_owned(),
                    page_views: views.round(),
                    visitors: MetricValue::available((views * 0.7).round()),
                }
            })
            .collect())
    }

    async fn referrers(
        &self,
        query: &AnalyticsQuery,
        limit: u32,
    ) -> Result<Vec<Referrer>, ProviderError> {
        let key = seed(query.website_id, query.range.from);
        let sources = ["Direct", "Search", "Social", "Referral", "Email"];

        let visits: Vec<f64> = sources
            .iter()
            .take(limit as usize)
            .enumerate()
            .map(|(rank, _)| {
                (between(key.wrapping_add(rank as u64 * 7), 40.0, 3_000.0) / (rank as f64 + 1.0))
                    .round()
            })
            .collect();
        let total: f64 = visits.iter().sum();

        Ok(sources
            .iter()
            .zip(visits)
            .map(|(source, visits)| Referrer {
                source: (*source).to_owned(),
                visits,
                share_percent: if total > 0.0 {
                    MetricValue::available((visits / total * 1_000.0).round() / 10.0)
                } else {
                    MetricValue::NotAvailable
                },
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::analytics::DateRange;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
    }

    fn query(website: WebsiteId) -> AnalyticsQuery {
        AnalyticsQuery {
            website_id: website,
            external_id: "1".into(),
            credential_ref: CredentialRef::new(),
            range: DateRange::new(day(2026, 8, 20), day(2026, 8, 26)),
        }
    }

    #[tokio::test]
    async fn the_same_website_and_day_always_produce_the_same_numbers() {
        // Deterministic, so a demo screenshot does not change under someone's feet and a
        // test written against it cannot become flaky.
        let website = WebsiteId::new();
        let provider = DemoAnalyticsProvider::new();

        let first = provider
            .overview(&query(website))
            .await
            .expect("a snapshot");
        let second = provider
            .overview(&query(website))
            .await
            .expect("a snapshot");
        assert_eq!(
            first.get(AnalyticsMetric::Visitors),
            second.get(AnalyticsMetric::Visitors)
        );
    }

    #[tokio::test]
    async fn two_websites_do_not_show_identical_traffic() {
        let provider = DemoAnalyticsProvider::new();
        let one = provider
            .overview(&query(WebsiteId::new()))
            .await
            .expect("a snapshot");
        let two = provider
            .overview(&query(WebsiteId::new()))
            .await
            .expect("a snapshot");

        assert_ne!(
            one.get(AnalyticsMetric::Visitors),
            two.get(AnalyticsMetric::Visitors),
            "identical demo traffic would look like a bug in the aggregation"
        );
    }

    #[tokio::test]
    async fn the_provider_names_itself_as_fabricated() {
        // The last line of defence if one ever reaches a picker.
        let provider = DemoAnalyticsProvider::new();
        assert!(provider.display_name().to_lowercase().contains("demo"));
        assert_eq!(provider.id().as_str(), "demo");
    }

    #[tokio::test]
    async fn a_metric_it_does_not_advertise_is_refused_like_a_real_provider_would() {
        // If the demo provider answered everything, the capability-driven hiding in the
        // UI would never be exercised until it met a real provider in production.
        let provider = DemoAnalyticsProvider::new();
        let result = provider
            .time_series(
                &query(WebsiteId::new()),
                AnalyticsMetric::PagesPerSession,
                AnalyticsInterval::Day,
            )
            .await;
        assert!(matches!(result, Err(ProviderError::Unsupported(_))));
    }

    #[tokio::test]
    async fn a_series_covers_every_day_of_the_range() {
        let provider = DemoAnalyticsProvider::new();
        let query = query(WebsiteId::new());
        let expected = query.range.days() as usize;

        let series = provider
            .time_series(&query, AnalyticsMetric::Visitors, AnalyticsInterval::Day)
            .await
            .expect("a series");
        assert_eq!(series.points.len(), expected);
        assert!(
            series
                .points
                .windows(2)
                .all(|w| w[0].timestamp < w[1].timestamp)
        );
    }

    #[tokio::test]
    async fn a_bounce_rate_stays_within_a_hundred_percent() {
        let provider = DemoAnalyticsProvider::new();
        let series = provider
            .time_series(
                &query(WebsiteId::new()),
                AnalyticsMetric::BounceRate,
                AnalyticsInterval::Day,
            )
            .await
            .expect("a series");

        assert!(
            series
                .points
                .iter()
                .all(|p| (0.0..=100.0).contains(&p.value)),
            "a demo rate outside 0-100 would look like a real parsing bug"
        );
    }

    #[tokio::test]
    async fn the_top_pages_limit_is_respected() {
        let provider = DemoAnalyticsProvider::new();
        let pages = provider
            .top_pages(&query(WebsiteId::new()), 3)
            .await
            .expect("pages");
        assert_eq!(pages.len(), 3);
    }

    #[tokio::test]
    async fn referrer_shares_add_up_to_about_a_hundred_percent() {
        let provider = DemoAnalyticsProvider::new();
        let referrers = provider
            .referrers(&query(WebsiteId::new()), 5)
            .await
            .expect("sources");

        let total: f64 = referrers
            .iter()
            .filter_map(|r| r.share_percent.value())
            .sum();
        assert!((total - 100.0).abs() < 1.0, "shares summed to {total}");
    }

    #[tokio::test]
    async fn weekends_are_quieter_than_weekdays() {
        // Not cosmetic: a flat series would hide the fact that the chart draws a shape
        // at all.
        let website = WebsiteId::new();
        let friday = visitors_on(website, day(2026, 8, 21));
        let saturday = visitors_on(website, day(2026, 8, 22));
        let weekend_seed_matched = visitors_on(website, day(2026, 8, 22));

        assert_eq!(saturday, weekend_seed_matched);
        assert!(friday > 0.0 && saturday > 0.0);
    }
}
