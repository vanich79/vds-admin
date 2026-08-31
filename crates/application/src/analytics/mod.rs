//! Analytics use cases: refreshing provider data and reading it back.
//!
//! The provider registry is the extension point. Adding Google Analytics means
//! implementing [`AnalyticsProvider`] and registering it — nothing in this module,
//! the domain, the database or the UI changes.

mod anomaly;
mod registry;

pub use anomaly::{AnomalyConfig, BaselineStrategy, TrafficAnomalyDetector};
pub use registry::ProviderRegistry;

use crate::scheduler::{JobOutcome, RateLimitManager};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use vds_domain::analytics::{
    AnalyticsIntegration, AnalyticsInterval, AnalyticsMetric, AnalyticsPeriod, AnalyticsSnapshot,
    AnalyticsTimeSeries, DateRange, TopPage,
};
use vds_domain::events::DomainEvent;
use vds_domain::ids::{IntegrationId, ProviderId, WebsiteId};
use vds_domain::ports::{
    AnalyticsQuery, AnalyticsRepository, Clock, EventPublisher, ProviderError,
};

/// Where a piece of analytics data came from.
///
/// The UI shows this so a user can tell live data from a cached copy, which matters when
/// a provider is rate-limiting us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataOrigin {
    /// Fetched from the provider just now.
    Fresh,
    /// Read from the local cache.
    Cached,
}

/// Analytics data plus its provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct Sourced<T> {
    pub value: T,
    pub origin: DataOrigin,
    /// When the underlying data was fetched from the provider.
    pub fetched_at: DateTime<Utc>,
}

impl<T> Sourced<T> {
    pub fn age(&self, now: DateTime<Utc>) -> chrono::Duration {
        now - self.fetched_at
    }

    pub fn is_stale(&self, now: DateTime<Utc>, max_age: chrono::Duration) -> bool {
        self.age(now) > max_age
    }
}

/// Refreshes analytics from providers and serves it cache-first.
pub struct AnalyticsService {
    providers: Arc<ProviderRegistry>,
    repository: Arc<dyn AnalyticsRepository>,
    events: Arc<dyn EventPublisher>,
    clock: Arc<dyn Clock>,
    rate_limits: Arc<RateLimitManager>,
    detector: TrafficAnomalyDetector,
    /// Why the last refresh failed, if it did.
    ///
    /// The one piece of state this service keeps, and it earns its place: a refresh runs
    /// on a schedule with nobody watching, so without somewhere to leave the reason, a
    /// provider that has been rejecting the token for a fortnight shows up as an empty
    /// screen and nothing else. Holding the *kind* rather than the message is what lets
    /// the interface say it in the user's language.
    last_failure: parking_lot::Mutex<Option<&'static str>>,
}

impl AnalyticsService {
    pub fn new(
        providers: Arc<ProviderRegistry>,
        repository: Arc<dyn AnalyticsRepository>,
        events: Arc<dyn EventPublisher>,
        clock: Arc<dyn Clock>,
        rate_limits: Arc<RateLimitManager>,
        detector: TrafficAnomalyDetector,
    ) -> Self {
        Self {
            providers,
            repository,
            events,
            clock,
            rate_limits,
            detector,
            last_failure: parking_lot::Mutex::new(None),
        }
    }

    /// Why the most recent refresh failed, as a code the interface can translate.
    ///
    /// `None` once a refresh has succeeded: a stale error is worse than none, because it
    /// sends someone to fix something that is already working.
    pub fn last_failure(&self) -> Option<&'static str> {
        *self.last_failure.lock()
    }

    /// Reads an overview, serving the cache immediately and refreshing behind it.
    ///
    /// This is the stale-while-revalidate path from the architecture: the UI never waits
    /// on a provider round trip.
    pub async fn overview(
        &self,
        website_id: WebsiteId,
        period: AnalyticsPeriod,
    ) -> Option<Sourced<AnalyticsSnapshot>> {
        let range = period.resolve(self.clock.today_local());
        let integration = self.enabled_integration(website_id).await?;

        let cached = self
            .repository
            .snapshot(website_id, &integration.provider, range)
            .await
            .ok()
            .flatten();

        match cached {
            Some(snapshot) => {
                let fetched_at = snapshot.fetched_at;
                Some(Sourced {
                    value: snapshot,
                    origin: DataOrigin::Cached,
                    fetched_at,
                })
            }
            None => {
                // Nothing cached, so there is no choice but to wait for the provider.
                let snapshot = self.fetch_overview(&integration, range).await.ok()?;
                let fetched_at = snapshot.fetched_at;
                Some(Sourced {
                    value: snapshot,
                    origin: DataOrigin::Fresh,
                    fetched_at,
                })
            }
        }
    }

    /// The provider a new integration should use.
    ///
    /// Exists so the interface does not have to name a provider crate: the composition
    /// root decides what is registered, and the UI asks. The demo provider is skipped
    /// even when it is compiled in, so a development build cannot connect a real website
    /// to fabricated data by accident.
    pub fn default_provider(&self) -> Option<ProviderId> {
        self.providers
            .available()
            .into_iter()
            .map(|(id, _)| id)
            .find(|id| id.as_str() != "demo")
    }

    /// One website's series for a metric, cache-first.
    ///
    /// Falls back to the provider only when nothing is cached, and returns `None` — never
    /// an empty series — when the provider cannot serve time series at all, so the UI can
    /// tell "no chart available here" from "no traffic".
    pub async fn series(
        &self,
        website_id: WebsiteId,
        period: AnalyticsPeriod,
        metric: AnalyticsMetric,
    ) -> Option<Sourced<AnalyticsTimeSeries>> {
        let today = self.clock.today_local();
        let range = period.resolve(today);
        let interval = period.natural_interval(today);
        let integration = self.enabled_integration(website_id).await?;

        if let Ok(Some(series)) = self
            .repository
            .time_series(website_id, &integration.provider, metric, interval, range)
            .await
        {
            let fetched_at = series.fetched_at;
            return Some(Sourced {
                value: series,
                origin: DataOrigin::Cached,
                fetched_at,
            });
        }

        let provider = self.providers.get(&integration.provider)?;
        if !provider.capabilities().supports_time_series
            || !provider.capabilities().supports(metric)
        {
            return None;
        }

        let series = self
            .fetch_series(&integration, range, metric, interval)
            .await
            .ok()?;
        let fetched_at = series.fetched_at;
        Some(Sourced {
            value: series,
            origin: DataOrigin::Fresh,
            fetched_at,
        })
    }

    /// The most visited pages of one website.
    ///
    /// There is no local cache for this report, so it goes to the provider — which is
    /// acceptable because it is only requested when a user opens one website's Analytics
    /// tab, not on every dashboard refresh. Providers that do not offer the report yield
    /// `None` and the UI hides the table, rather than an `Unsupported` error appearing in
    /// the log on every tab switch.
    pub async fn top_pages(
        &self,
        website_id: WebsiteId,
        period: AnalyticsPeriod,
        limit: u32,
    ) -> Option<Vec<TopPage>> {
        let range = period.resolve(self.clock.today_local());
        let integration = self.enabled_integration(website_id).await?;
        let provider = self.providers.get(&integration.provider)?;

        if !provider.capabilities().supports_top_pages {
            return None;
        }
        self.check_rate_limit(&integration.provider).ok()?;

        match provider
            .top_pages(&query_for(&integration, range), limit)
            .await
        {
            Ok(pages) => Some(pages),
            Err(err) => {
                tracing::debug!(error = %err, "top pages unavailable");
                None
            }
        }
    }

    /// The enabled integration for a website, if it has one.
    async fn enabled_integration(&self, website_id: WebsiteId) -> Option<AnalyticsIntegration> {
        self.repository
            .list_integrations_for_website(website_id)
            .await
            .ok()?
            .into_iter()
            .find(|i| i.enabled)
    }

    /// Refreshes one integration from its provider and stores the result.
    pub async fn refresh(&self, integration_id: IntegrationId) -> JobOutcome {
        let integration = match self.repository.get_integration(integration_id).await {
            Ok(integration) => integration,
            Err(_) => return JobOutcome::Skipped,
        };

        if !integration.enabled {
            return JobOutcome::Skipped;
        }

        let today = self.clock.today_local();
        let range = AnalyticsPeriod::LastThirtyDays.resolve(today);

        match self.fetch_overview(&integration, range).await {
            Ok(_) => {}
            Err(err) => {
                // The reason is kept where the interface can reach it. A refresh that has
                // been failing since the token was entered has to be able to say so;
                // until it could, the only symptom was an empty screen.
                *self.last_failure.lock() = Some(err.kind());
                return self.outcome_for(&integration, err);
            }
        }

        // The time series feeds both the chart and the anomaly detector, so it is worth
        // a second call — but only if the provider supports it.
        let Some(provider) = self.providers.get(&integration.provider) else {
            return JobOutcome::Permanent(format!("no such provider: {}", integration.provider));
        };

        if provider.capabilities().supports_time_series {
            let interval = AnalyticsPeriod::LastThirtyDays.natural_interval(today);
            match self
                .fetch_series(&integration, range, AnalyticsMetric::Visitors, interval)
                .await
            {
                Ok(series) => self.detect_anomaly(&series),
                Err(err) => {
                    tracing::debug!(error = %err, "time series refresh failed");
                }
            }
        }

        *self.last_failure.lock() = None;
        self.events.publish(DomainEvent::AnalyticsUpdated {
            website_id: integration.website_id,
            provider: integration.provider.clone(),
        });

        JobOutcome::Success
    }

    async fn fetch_overview(
        &self,
        integration: &AnalyticsIntegration,
        range: DateRange,
    ) -> Result<AnalyticsSnapshot, ProviderError> {
        let provider = self
            .providers
            .get(&integration.provider)
            .ok_or_else(|| ProviderError::NotFound(integration.provider.to_string()))?;

        self.check_rate_limit(&integration.provider)?;

        let query = query_for(integration, range);
        let snapshot = provider.overview(&query).await?;

        if let Err(err) = self.repository.save_snapshot(&snapshot).await {
            tracing::warn!(error = %err, "could not cache analytics snapshot");
        }
        Ok(snapshot)
    }

    async fn fetch_series(
        &self,
        integration: &AnalyticsIntegration,
        range: DateRange,
        metric: AnalyticsMetric,
        interval: AnalyticsInterval,
    ) -> Result<AnalyticsTimeSeries, ProviderError> {
        let provider = self
            .providers
            .get(&integration.provider)
            .ok_or_else(|| ProviderError::NotFound(integration.provider.to_string()))?;

        self.check_rate_limit(&integration.provider)?;

        let query = query_for(integration, range);
        let series = provider.time_series(&query, metric, interval).await?;

        if let Err(err) = self.repository.save_time_series(&series).await {
            tracing::warn!(error = %err, "could not cache analytics series");
        }
        Ok(series)
    }

    fn check_rate_limit(&self, provider: &ProviderId) -> Result<(), ProviderError> {
        let decision = self
            .rate_limits
            .acquire(provider.as_str(), self.clock.now());
        if decision.is_allowed() {
            Ok(())
        } else {
            Err(ProviderError::RateLimited {
                retry_after_secs: Some(decision.delay().num_seconds().max(1) as u64),
            })
        }
    }

    fn detect_anomaly(&self, series: &AnalyticsTimeSeries) {
        let Some(comparison) = self.detector.detect(series) else {
            return;
        };
        if comparison.trend == vds_domain::analytics::TrafficTrend::Anomaly {
            self.events.publish(DomainEvent::TrafficAnomalyDetected {
                website_id: comparison.website_id,
                metric: comparison.metric,
                current: comparison.current,
                baseline: comparison.baseline,
                change_percent: comparison.change_percent,
            });
        }
    }

    /// Maps a provider failure onto a scheduler outcome, obeying any back-off request.
    fn outcome_for(&self, integration: &AnalyticsIntegration, err: ProviderError) -> JobOutcome {
        if let Some(retry_after) = err.retry_after() {
            self.rate_limits
                .penalise(integration.provider.as_str(), retry_after, self.clock.now());
        }

        // Logged as well as published. An event reaches the activity feed, which is the
        // right place for a user to see this; the log is where it can be read after the
        // fact, and a provider that has been failing since the token was entered leaves
        // no other trace.
        tracing::warn!(
            provider = %integration.provider,
            counter = %integration.external_id,
            error = %err,
            "an analytics refresh failed"
        );

        self.events.publish(DomainEvent::AnalyticsRefreshFailed {
            website_id: integration.website_id,
            provider: integration.provider.clone(),
            error: err.to_string(),
        });

        if err.is_retryable() {
            JobOutcome::Retry(err.to_string())
        } else {
            // A revoked OAuth token will not fix itself; hammering the API only burns
            // quota and can get the credential locked.
            JobOutcome::Permanent(err.to_string())
        }
    }
}

/// Builds a provider query from an integration.
fn query_for(integration: &AnalyticsIntegration, range: DateRange) -> AnalyticsQuery {
    AnalyticsQuery {
        website_id: integration.website_id,
        external_id: integration.external_id.clone(),
        credential_ref: integration.credential_ref,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeAnalyticsRepository;
    use async_trait::async_trait;
    use chrono::NaiveDate;
    use parking_lot::Mutex;
    use vds_domain::analytics::{AnalyticsCapabilities, AnalyticsPoint};
    use vds_domain::ids::CredentialRef;
    use vds_domain::metrics::MetricValue;
    use vds_domain::ports::{
        AnalyticsProvider, FixedClock, ProviderHealth, RecordingEventPublisher,
    };

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    /// A provider that returns scripted answers and counts calls.
    struct StubProvider {
        overview: Mutex<Result<AnalyticsSnapshot, ProviderError>>,
        series: Mutex<Option<Result<AnalyticsTimeSeries, ProviderError>>>,
        pages: Mutex<Option<Result<Vec<TopPage>, ProviderError>>>,
        calls: Mutex<u32>,
        capabilities: AnalyticsCapabilities,
    }

    impl StubProvider {
        fn new(snapshot: AnalyticsSnapshot) -> Self {
            Self {
                overview: Mutex::new(Ok(snapshot)),
                series: Mutex::new(None),
                pages: Mutex::new(None),
                calls: Mutex::new(0),
                capabilities: AnalyticsCapabilities {
                    supported_metrics: vec![AnalyticsMetric::Visitors],
                    supports_time_series: true,
                    supports_top_pages: false,
                    supports_referrers: false,
                    supports_realtime: false,
                    min_interval: AnalyticsInterval::Day,
                    max_history_days: None,
                },
            }
        }

        fn failing(err: ProviderError) -> Self {
            let mut stub = Self::new(empty_snapshot(WebsiteId::new()));
            *stub.overview.lock() = Err(err);
            stub.capabilities.supports_time_series = false;
            stub
        }

        fn calls(&self) -> u32 {
            *self.calls.lock()
        }
    }

    #[async_trait]
    impl AnalyticsProvider for StubProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("stub")
        }

        fn display_name(&self) -> &'static str {
            "Stub"
        }

        fn capabilities(&self) -> AnalyticsCapabilities {
            self.capabilities.clone()
        }

        async fn validate_connection(
            &self,
            _credential_ref: CredentialRef,
        ) -> Result<ProviderHealth, ProviderError> {
            Ok(ProviderHealth::Ok)
        }

        async fn overview(
            &self,
            _query: &AnalyticsQuery,
        ) -> Result<AnalyticsSnapshot, ProviderError> {
            *self.calls.lock() += 1;
            self.overview.lock().clone()
        }

        async fn time_series(
            &self,
            _query: &AnalyticsQuery,
            _metric: AnalyticsMetric,
            _interval: AnalyticsInterval,
        ) -> Result<AnalyticsTimeSeries, ProviderError> {
            *self.calls.lock() += 1;
            self.series
                .lock()
                .clone()
                .unwrap_or(Err(ProviderError::Unsupported("time series")))
        }

        async fn top_pages(
            &self,
            _query: &AnalyticsQuery,
            limit: u32,
        ) -> Result<Vec<TopPage>, ProviderError> {
            *self.calls.lock() += 1;
            let mut pages = self
                .pages
                .lock()
                .clone()
                .unwrap_or(Err(ProviderError::Unsupported("top pages")))?;
            pages.truncate(limit as usize);
            Ok(pages)
        }
    }

    fn page(url: &str, views: f64) -> TopPage {
        TopPage {
            url: url.into(),
            page_views: views,
            visitors: MetricValue::Available(views),
        }
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
    }

    fn empty_snapshot(website: WebsiteId) -> AnalyticsSnapshot {
        AnalyticsSnapshot::new(
            website,
            ProviderId::new("stub"),
            DateRange::new(day(2026, 7, 28), day(2026, 8, 26)),
            at(0),
        )
    }

    struct Harness {
        service: AnalyticsService,
        repository: Arc<FakeAnalyticsRepository>,
        provider: Arc<StubProvider>,
        events: Arc<RecordingEventPublisher>,
        rate_limits: Arc<RateLimitManager>,
        integration: AnalyticsIntegration,
    }

    fn harness_with(provider: Arc<StubProvider>, website: WebsiteId) -> Harness {
        let repository = Arc::new(FakeAnalyticsRepository::new());
        let integration = AnalyticsIntegration::new(
            website,
            ProviderId::new("stub"),
            "12345",
            CredentialRef::new(),
            at(0),
        );
        repository.insert(integration.clone());

        let mut registry = ProviderRegistry::new();
        registry.register(Arc::clone(&provider) as Arc<dyn AnalyticsProvider>);

        let events = Arc::new(RecordingEventPublisher::new());
        let rate_limits = Arc::new(RateLimitManager::new());
        // The clock is set to a date that makes `today_local` deterministic.
        let clock = FixedClock::new(
            day(2026, 8, 26)
                .and_hms_opt(12, 0, 0)
                .expect("valid")
                .and_utc(),
        );

        let service = AnalyticsService::new(
            Arc::new(registry),
            Arc::clone(&repository) as Arc<dyn AnalyticsRepository>,
            Arc::clone(&events) as Arc<dyn EventPublisher>,
            Arc::new(clock),
            Arc::clone(&rate_limits),
            TrafficAnomalyDetector::default(),
        );

        Harness {
            service,
            repository,
            provider,
            events,
            rate_limits,
            integration,
        }
    }

    fn harness() -> Harness {
        let website = WebsiteId::new();
        let snapshot = empty_snapshot(website)
            .with(AnalyticsMetric::Visitors, MetricValue::Available(24_821.0));
        harness_with(Arc::new(StubProvider::new(snapshot)), website)
    }

    #[tokio::test]
    async fn a_refresh_fetches_and_caches_the_overview() {
        let h = harness();
        assert_eq!(
            h.service.refresh(h.integration.id).await,
            JobOutcome::Success
        );

        assert_eq!(h.repository.snapshot_count(), 1);
        assert!(h.events.contains(|e| e.kind() == "analytics_updated"));
    }

    #[tokio::test]
    async fn reads_are_served_from_the_cache_without_calling_the_provider() {
        // The stale-while-revalidate requirement: opening the dashboard must not block
        // on a provider round trip.
        let h = harness();
        h.service.refresh(h.integration.id).await;
        let calls_after_refresh = h.provider.calls();

        let sourced = h
            .service
            .overview(h.integration.website_id, AnalyticsPeriod::LastThirtyDays)
            .await
            .expect("cached data available");

        assert_eq!(sourced.origin, DataOrigin::Cached);
        assert_eq!(
            h.provider.calls(),
            calls_after_refresh,
            "no extra provider call"
        );
        assert_eq!(
            sourced.value.get(AnalyticsMetric::Visitors),
            MetricValue::Available(24_821.0)
        );
    }

    #[tokio::test]
    async fn a_cold_cache_falls_back_to_a_live_fetch() {
        let h = harness();
        let sourced = h
            .service
            .overview(h.integration.website_id, AnalyticsPeriod::LastThirtyDays)
            .await
            .expect("live data available");
        assert_eq!(sourced.origin, DataOrigin::Fresh);
    }

    #[tokio::test]
    async fn a_disabled_integration_is_skipped() {
        let h = harness();
        let mut integration = h.integration.clone();
        integration.enabled = false;
        h.repository.insert(integration);

        assert_eq!(
            h.service.refresh(h.integration.id).await,
            JobOutcome::Skipped
        );
        assert_eq!(h.provider.calls(), 0);
    }

    #[tokio::test]
    async fn a_rate_limit_response_is_retried_and_penalises_the_provider() {
        let h = harness_with(
            Arc::new(StubProvider::failing(ProviderError::RateLimited {
                retry_after_secs: Some(120),
            })),
            WebsiteId::new(),
        );

        let outcome = h.service.refresh(h.integration.id).await;
        assert!(matches!(outcome, JobOutcome::Retry(_)), "got {outcome:?}");

        // The penalty must actually be applied, or we would immediately retry into
        // another 429.
        assert!(!h.rate_limits.acquire("stub", at(0)).is_allowed());
        assert!(
            h.events
                .contains(|e| e.kind() == "analytics_refresh_failed")
        );
    }

    #[tokio::test]
    async fn a_revoked_token_is_a_permanent_failure() {
        let h = harness_with(
            Arc::new(StubProvider::failing(ProviderError::Authentication(
                "token revoked".into(),
            ))),
            WebsiteId::new(),
        );
        let outcome = h.service.refresh(h.integration.id).await;
        assert!(
            matches!(outcome, JobOutcome::Permanent(_)),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn the_local_rate_limiter_stops_a_refresh_before_it_reaches_the_provider() {
        let h = harness();
        h.rate_limits.configure("stub", 1, at(0));
        let now = day(2026, 8, 26)
            .and_hms_opt(12, 0, 0)
            .expect("valid")
            .and_utc();
        h.rate_limits
            .penalise("stub", chrono::Duration::seconds(600), now);

        let outcome = h.service.refresh(h.integration.id).await;
        assert!(matches!(outcome, JobOutcome::Retry(_)));
        assert_eq!(
            h.provider.calls(),
            0,
            "the request must not leave the machine"
        );
    }

    #[tokio::test]
    async fn a_traffic_collapse_publishes_an_anomaly_event() {
        let website = WebsiteId::new();
        let provider = Arc::new(StubProvider::new(
            empty_snapshot(website)
                .with(AnalyticsMetric::Visitors, MetricValue::Available(6_500.0)),
        ));
        *provider.series.lock() = Some(Ok(AnalyticsTimeSeries {
            website_id: website,
            provider: ProviderId::new("stub"),
            metric: AnalyticsMetric::Visitors,
            interval: AnalyticsInterval::Day,
            range: DateRange::new(day(2026, 7, 28), day(2026, 8, 26)),
            fetched_at: at(0),
            points: vec![
                AnalyticsPoint {
                    timestamp: at(0),
                    value: 10_000.0,
                },
                AnalyticsPoint {
                    timestamp: at(86_400),
                    value: 10_000.0,
                },
                AnalyticsPoint {
                    timestamp: at(2 * 86_400),
                    value: 10_000.0,
                },
                AnalyticsPoint {
                    timestamp: at(3 * 86_400),
                    value: 6_500.0,
                },
            ],
        }));

        let h = harness_with(provider, website);
        h.service.refresh(h.integration.id).await;

        assert!(h.events.contains(|e| matches!(
            e,
            DomainEvent::TrafficAnomalyDetected { change_percent, .. } if *change_percent < -30.0
        )));
    }

    #[tokio::test]
    async fn steady_traffic_publishes_no_anomaly() {
        let website = WebsiteId::new();
        let provider = Arc::new(StubProvider::new(empty_snapshot(website)));
        *provider.series.lock() = Some(Ok(AnalyticsTimeSeries {
            website_id: website,
            provider: ProviderId::new("stub"),
            metric: AnalyticsMetric::Visitors,
            interval: AnalyticsInterval::Day,
            range: DateRange::new(day(2026, 7, 28), day(2026, 8, 26)),
            fetched_at: at(0),
            points: vec![
                AnalyticsPoint {
                    timestamp: at(0),
                    value: 1_000.0,
                },
                AnalyticsPoint {
                    timestamp: at(86_400),
                    value: 1_010.0,
                },
                AnalyticsPoint {
                    timestamp: at(2 * 86_400),
                    value: 995.0,
                },
                AnalyticsPoint {
                    timestamp: at(3 * 86_400),
                    value: 1_005.0,
                },
            ],
        }));

        let h = harness_with(provider, website);
        h.service.refresh(h.integration.id).await;
        assert!(
            !h.events
                .contains(|e| e.kind() == "traffic_anomaly_detected")
        );
    }

    #[tokio::test]
    async fn a_provider_without_time_series_support_is_never_asked_for_one() {
        let mut provider = StubProvider::new(empty_snapshot(WebsiteId::new()));
        provider.capabilities.supports_time_series = false;
        let h = harness_with(Arc::new(provider), WebsiteId::new());

        assert_eq!(
            h.service.refresh(h.integration.id).await,
            JobOutcome::Success
        );
        // Exactly one call: the overview. No wasted, guaranteed-to-fail series request.
        assert_eq!(h.provider.calls(), 1);
    }

    #[tokio::test]
    async fn a_deleted_integration_is_skipped() {
        let h = harness();
        h.repository
            .delete_integration(h.integration.id)
            .await
            .expect("deleted");
        assert_eq!(
            h.service.refresh(h.integration.id).await,
            JobOutcome::Skipped
        );
    }

    #[test]
    fn sourced_data_reports_its_age() {
        let sourced = Sourced {
            value: 42,
            origin: DataOrigin::Cached,
            fetched_at: at(0),
        };
        assert_eq!(sourced.age(at(480)), chrono::Duration::seconds(480));
        assert!(sourced.is_stale(at(1_000), chrono::Duration::seconds(600)));
        assert!(!sourced.is_stale(at(500), chrono::Duration::seconds(600)));
    }

    #[tokio::test]
    async fn a_cached_series_is_served_without_calling_the_provider() {
        let website = WebsiteId::new();
        let h = harness_with(
            Arc::new(StubProvider::new(empty_snapshot(website))),
            website,
        );
        let today = day(2026, 8, 26);
        let period = AnalyticsPeriod::LastThirtyDays;

        h.repository
            .save_time_series(&AnalyticsTimeSeries {
                website_id: website,
                provider: ProviderId::new("stub"),
                metric: AnalyticsMetric::Visitors,
                interval: period.natural_interval(today),
                range: period.resolve(today),
                fetched_at: at(0),
                points: vec![AnalyticsPoint {
                    timestamp: at(0),
                    value: 42.0,
                }],
            })
            .await
            .expect("saved");

        let series = h
            .service
            .series(website, period, AnalyticsMetric::Visitors)
            .await
            .expect("a series");
        assert_eq!(series.origin, DataOrigin::Cached);
        assert_eq!(series.value.points[0].value, 42.0);
        assert_eq!(h.provider.calls(), 0, "the cache must answer on its own");
    }

    #[tokio::test]
    async fn a_cold_series_cache_falls_back_to_the_provider() {
        let website = WebsiteId::new();
        let provider = Arc::new(StubProvider::new(empty_snapshot(website)));
        let period = AnalyticsPeriod::LastThirtyDays;
        *provider.series.lock() = Some(Ok(AnalyticsTimeSeries {
            website_id: website,
            provider: ProviderId::new("stub"),
            metric: AnalyticsMetric::Visitors,
            interval: period.natural_interval(day(2026, 8, 26)),
            range: period.resolve(day(2026, 8, 26)),
            fetched_at: at(0),
            points: vec![AnalyticsPoint {
                timestamp: at(0),
                value: 7.0,
            }],
        }));

        let h = harness_with(provider, website);
        let series = h
            .service
            .series(website, period, AnalyticsMetric::Visitors)
            .await
            .expect("a series");
        assert_eq!(series.origin, DataOrigin::Fresh);
        assert_eq!(h.provider.calls(), 1);
    }

    #[tokio::test]
    async fn a_provider_without_time_series_is_not_asked_for_a_chart() {
        // The capability check is what keeps an `Unsupported` error out of the log on
        // every tab switch.
        let website = WebsiteId::new();
        let mut stub = StubProvider::new(empty_snapshot(website));
        stub.capabilities.supports_time_series = false;
        let h = harness_with(Arc::new(stub), website);

        assert!(
            h.service
                .series(
                    website,
                    AnalyticsPeriod::LastThirtyDays,
                    AnalyticsMetric::Visitors
                )
                .await
                .is_none()
        );
        assert_eq!(h.provider.calls(), 0);
    }

    #[tokio::test]
    async fn a_metric_the_provider_does_not_support_yields_no_chart_rather_than_zeros() {
        let website = WebsiteId::new();
        let h = harness_with(
            Arc::new(StubProvider::new(empty_snapshot(website))),
            website,
        );

        // The stub advertises Visitors only.
        assert!(
            h.service
                .series(
                    website,
                    AnalyticsPeriod::LastThirtyDays,
                    AnalyticsMetric::BounceRate
                )
                .await
                .is_none()
        );
        assert_eq!(h.provider.calls(), 0);
    }

    #[tokio::test]
    async fn top_pages_come_back_when_the_provider_offers_them() {
        let website = WebsiteId::new();
        let mut stub = StubProvider::new(empty_snapshot(website));
        stub.capabilities.supports_top_pages = true;
        let stub = Arc::new(stub);
        *stub.pages.lock() = Some(Ok(vec![page("/", 900.0), page("/pricing", 120.0)]));

        let h = harness_with(stub, website);
        let pages = h
            .service
            .top_pages(website, AnalyticsPeriod::LastThirtyDays, 10)
            .await
            .expect("pages");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].url, "/");
    }

    #[tokio::test]
    async fn the_limit_is_passed_through_to_the_provider() {
        let website = WebsiteId::new();
        let mut stub = StubProvider::new(empty_snapshot(website));
        stub.capabilities.supports_top_pages = true;
        let stub = Arc::new(stub);
        *stub.pages.lock() = Some(Ok(vec![page("/", 900.0), page("/a", 5.0), page("/b", 4.0)]));

        let h = harness_with(stub, website);
        let pages = h
            .service
            .top_pages(website, AnalyticsPeriod::LastThirtyDays, 2)
            .await
            .expect("pages");
        assert_eq!(pages.len(), 2);
    }

    #[tokio::test]
    async fn a_provider_without_top_pages_is_never_asked_for_them() {
        let website = WebsiteId::new();
        let h = harness_with(
            Arc::new(StubProvider::new(empty_snapshot(website))),
            website,
        );

        assert!(
            h.service
                .top_pages(website, AnalyticsPeriod::LastThirtyDays, 10)
                .await
                .is_none()
        );
        assert_eq!(
            h.provider.calls(),
            0,
            "an unsupported report must not be requested"
        );
    }

    #[tokio::test]
    async fn a_failed_top_pages_call_yields_nothing_rather_than_an_empty_table() {
        // An empty table reads as "this site has no pages", which is a different claim.
        let website = WebsiteId::new();
        let mut stub = StubProvider::new(empty_snapshot(website));
        stub.capabilities.supports_top_pages = true;
        let stub = Arc::new(stub);
        *stub.pages.lock() = Some(Err(ProviderError::Network("dns".into())));

        let h = harness_with(stub, website);
        assert!(
            h.service
                .top_pages(website, AnalyticsPeriod::LastThirtyDays, 10)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_website_without_an_integration_has_neither_chart_nor_pages() {
        let website = WebsiteId::new();
        let h = harness_with(
            Arc::new(StubProvider::new(empty_snapshot(website))),
            website,
        );
        let stranger = WebsiteId::new();

        assert!(
            h.service
                .series(
                    stranger,
                    AnalyticsPeriod::LastThirtyDays,
                    AnalyticsMetric::Visitors
                )
                .await
                .is_none()
        );
        assert!(
            h.service
                .top_pages(stranger, AnalyticsPeriod::LastThirtyDays, 10)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_reason_a_refresh_failed_outlives_the_refresh() {
        // A refresh runs on a schedule with nobody watching. Without somewhere to leave
        // the reason, a provider that has been rejecting the token for a fortnight is
        // indistinguishable from a site with no traffic — which is exactly how an expired
        // Yandex token went unnoticed here.
        let website = WebsiteId::new();
        let provider = Arc::new(StubProvider::failing(ProviderError::Forbidden(
            "Invalid oauth_token".into(),
        )));
        let h = harness_with(Arc::clone(&provider), website);

        assert_eq!(h.service.last_failure(), None, "nothing has run yet");

        h.service.refresh(h.integration.id).await;

        // The *kind*, not the sentence: the provider's own words are English and cannot
        // be translated after the fact.
        assert_eq!(h.service.last_failure(), Some("forbidden"));
    }

    #[tokio::test]
    async fn a_successful_refresh_clears_the_previous_reason() {
        // A stale error is worse than none: it sends someone to fix what already works.
        let h = harness();
        h.service.refresh(h.integration.id).await;
        assert_eq!(h.service.last_failure(), None);
    }
}
