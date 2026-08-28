//! Dashboard queries.
//!
//! Assembles the read-model the UI renders. The UI performs no I/O and no arithmetic of
//! its own — it receives a finished [`DashboardSummary`] and draws it.

mod widgets;

pub use widgets::{DashboardLayout, WidgetConfig, WidgetKind, WidgetSize};

use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::sync::Arc;
use vds_domain::Status;
use vds_domain::analytics::{
    AnalyticsMetric, AnalyticsPeriod, AnalyticsPoint, AnalyticsTimeSeries,
};
use vds_domain::events::EventEnvelope;
use vds_domain::metrics::MetricValue;
use vds_domain::ports::{
    AlertRepository, AnalyticsRepository, Clock, EventRepository, ServerRepository,
    WebsiteRepository,
};

/// Counts of subjects by status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StatusCounts {
    pub total: usize,
    pub healthy: usize,
    pub warning: usize,
    pub critical: usize,
    pub offline: usize,
    pub unknown: usize,
}

impl StatusCounts {
    /// Tallies a set of statuses.
    pub fn tally(statuses: impl IntoIterator<Item = Status>) -> Self {
        let mut counts = StatusCounts::default();
        for status in statuses {
            counts.total += 1;
            match status {
                Status::Healthy => counts.healthy += 1,
                Status::Warning => counts.warning += 1,
                Status::Critical => counts.critical += 1,
                Status::Offline => counts.offline += 1,
                Status::Unknown => counts.unknown += 1,
            }
        }
        counts
    }

    /// Subjects that need attention.
    pub fn problems(&self) -> usize {
        self.warning + self.critical + self.offline
    }

    /// The worst status present, for a single at-a-glance indicator.
    pub fn worst(&self) -> Status {
        if self.offline > 0 {
            Status::Offline
        } else if self.critical > 0 {
            Status::Critical
        } else if self.warning > 0 {
            Status::Warning
        } else if self.healthy > 0 {
            Status::Healthy
        } else {
            Status::Unknown
        }
    }
}

/// The infrastructure half of the dashboard.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InfrastructureSummary {
    pub servers: StatusCounts,
    pub websites: StatusCounts,
    /// Mean CPU across reachable servers.
    pub average_cpu: MetricValue,
    /// Mean memory across reachable servers.
    pub average_memory: MetricValue,
    /// Mean response time across reachable websites, in milliseconds.
    pub average_response_ms: MetricValue,
}

/// The analytics half of the dashboard.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrafficSummary {
    pub visitors: MetricValue,
    pub visits: MetricValue,
    pub page_views: MetricValue,
    pub average_bounce_rate: MetricValue,
    pub average_session_duration: MetricValue,
    /// Websites contributing to these totals.
    pub sources: usize,
}

/// Everything the dashboard needs, in one value.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DashboardSummary {
    pub infrastructure: InfrastructureSummary,
    pub traffic: TrafficSummary,
    pub open_incidents: usize,
    pub recent_events: Vec<EventEnvelope>,
    /// Servers that are not healthy, worst first.
    pub problem_servers: Vec<ProblemServer>,
}

/// A server needing attention, for the dashboard's problem list.
#[derive(Debug, Clone, PartialEq)]
pub struct ProblemServer {
    pub id: vds_domain::ids::ServerId,
    pub name: String,
    pub status: Status,
    pub reason: Option<String>,
}

/// Assembles dashboard read-models.
pub struct DashboardQueryService {
    servers: Arc<dyn ServerRepository>,
    websites: Arc<dyn WebsiteRepository>,
    analytics: Arc<dyn AnalyticsRepository>,
    alerts: Arc<dyn AlertRepository>,
    events: Arc<dyn EventRepository>,
    clock: Arc<dyn Clock>,
}

/// How many recent events the dashboard shows.
const RECENT_EVENT_LIMIT: u32 = 20;

impl DashboardQueryService {
    pub fn new(
        servers: Arc<dyn ServerRepository>,
        websites: Arc<dyn WebsiteRepository>,
        analytics: Arc<dyn AnalyticsRepository>,
        alerts: Arc<dyn AlertRepository>,
        events: Arc<dyn EventRepository>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            servers,
            websites,
            analytics,
            alerts,
            events,
            clock,
        }
    }

    /// Builds the whole dashboard.
    pub async fn summary(&self, period: AnalyticsPeriod) -> DashboardSummary {
        DashboardSummary {
            infrastructure: self.infrastructure().await,
            traffic: self.traffic(period).await,
            open_incidents: self
                .alerts
                .open_incidents()
                .await
                .map(|i| i.len())
                .unwrap_or(0),
            recent_events: self
                .events
                .recent(RECENT_EVENT_LIMIT)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.event.is_noteworthy())
                .collect(),
            problem_servers: self.problem_servers().await,
        }
    }

    /// Server and website health, plus fleet averages.
    pub async fn infrastructure(&self) -> InfrastructureSummary {
        let server_states = self.servers.list_states().await.unwrap_or_default();
        let website_states = self.websites.list_states().await.unwrap_or_default();

        InfrastructureSummary {
            servers: StatusCounts::tally(server_states.iter().map(|s| s.status)),
            websites: StatusCounts::tally(website_states.iter().map(|s| s.status)),
            // Averages are taken over servers that actually reported a number. An
            // unreachable server has no CPU usage, and counting it as zero would drag
            // the fleet average down and hide a real problem.
            average_cpu: mean(server_states.iter().filter_map(|s| s.cpu_percent.value())),
            average_memory: mean(
                server_states
                    .iter()
                    .filter_map(|s| s.memory_percent.value()),
            ),
            average_response_ms: mean(
                website_states
                    .iter()
                    .filter_map(|s| s.response_ms.map(f64::from)),
            ),
        }
    }

    /// Aggregate traffic across every website with analytics configured.
    pub async fn traffic(&self, period: AnalyticsPeriod) -> TrafficSummary {
        let range = period.resolve(self.clock.today_local());
        let integrations = self.analytics.list_integrations().await.unwrap_or_default();

        let mut visitors = Vec::new();
        let mut visits = Vec::new();
        let mut page_views = Vec::new();
        let mut bounce_rates = Vec::new();
        let mut durations = Vec::new();
        let mut sources = 0;

        for integration in integrations.iter().filter(|i| i.enabled) {
            let Ok(Some(snapshot)) = self
                .analytics
                .snapshot(integration.website_id, &integration.provider, range)
                .await
            else {
                continue;
            };
            sources += 1;

            if let Some(v) = snapshot.get(AnalyticsMetric::Visitors).value() {
                visitors.push(v);
            }
            if let Some(v) = snapshot.get(AnalyticsMetric::Visits).value() {
                visits.push(v);
            }
            if let Some(v) = snapshot.get(AnalyticsMetric::PageViews).value() {
                page_views.push(v);
            }
            if let Some(v) = snapshot.get(AnalyticsMetric::BounceRate).value() {
                bounce_rates.push(v);
            }
            if let Some(v) = snapshot
                .get(AnalyticsMetric::AverageSessionDuration)
                .value()
            {
                durations.push(v);
            }
        }

        TrafficSummary {
            // Totals are summed; rates and averages are averaged. Summing bounce rates
            // across sites would be meaningless, which is why `is_additive` exists.
            visitors: sum(visitors),
            visits: sum(visits),
            page_views: sum(page_views),
            average_bounce_rate: mean(bounce_rates),
            average_session_duration: mean(durations),
            sources,
        }
    }

    /// One traffic series for the whole fleet, for the analytics chart.
    ///
    /// Reads only the local cache. The analytics screen is opened often and a provider
    /// round trip per website would blow the rate limit for nothing — the refresh
    /// scheduler is what keeps the cache current.
    ///
    /// Returns `None` when nothing is cached, which the UI renders as an empty chart
    /// with an explanation rather than as a flat line at zero.
    pub async fn traffic_series(
        &self,
        period: AnalyticsPeriod,
        metric: AnalyticsMetric,
    ) -> Option<AnalyticsTimeSeries> {
        let today = self.clock.today_local();
        let range = period.resolve(today);
        let interval = period.natural_interval(today);
        let integrations = self.analytics.list_integrations().await.unwrap_or_default();

        let mut collected = Vec::new();
        for integration in integrations.iter().filter(|i| i.enabled) {
            if let Ok(Some(series)) = self
                .analytics
                .time_series(
                    integration.website_id,
                    &integration.provider,
                    metric,
                    interval,
                    range,
                )
                .await
            {
                collected.push(series);
            }
        }

        combine_series(collected, metric)
    }

    /// Servers that are not healthy, worst first.
    pub async fn problem_servers(&self) -> Vec<ProblemServer> {
        let states = self.servers.list_states().await.unwrap_or_default();
        let servers = self.servers.list().await.unwrap_or_default();

        let mut problems: Vec<ProblemServer> = states
            .into_iter()
            .filter(|state| state.status.is_problem())
            .map(|state| {
                let name = servers
                    .iter()
                    .find(|s| s.id == state.server_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| state.server_id.to_string());
                ProblemServer {
                    id: state.server_id,
                    name,
                    status: state.status,
                    reason: state.last_error,
                }
            })
            .collect();

        problems.sort_by(|a, b| b.status.cmp(&a.status).then_with(|| a.name.cmp(&b.name)));
        problems
    }
}

/// Merges one metric's series from several websites into a fleet-wide series.
///
/// Points are matched by timestamp, so websites whose provider returned a different
/// number of buckets still line up. Additive metrics are summed; rates and averages are
/// averaged, because a fleet bounce rate of 180% would be nonsense.
///
/// A timestamp is kept even when only some websites reported it: the alternative — an
/// intersection — would silently blank the chart whenever one site was added late.
fn combine_series(
    series: Vec<AnalyticsTimeSeries>,
    metric: AnalyticsMetric,
) -> Option<AnalyticsTimeSeries> {
    let first = series.first()?;
    let mut buckets: BTreeMap<DateTime<Utc>, Vec<f64>> = BTreeMap::new();

    for one in &series {
        for point in &one.points {
            if point.value.is_finite() {
                buckets
                    .entry(point.timestamp)
                    .or_default()
                    .push(point.value);
            }
        }
    }

    if buckets.is_empty() {
        return None;
    }

    let points = buckets
        .into_iter()
        .map(|(timestamp, values)| {
            let total: f64 = values.iter().sum();
            let value = if metric.is_additive() {
                total
            } else {
                total / values.len() as f64
            };
            AnalyticsPoint { timestamp, value }
        })
        .collect();

    Some(AnalyticsTimeSeries {
        website_id: first.website_id,
        provider: first.provider.clone(),
        metric,
        interval: first.interval,
        range: first.range,
        // The fleet series is only as fresh as its stalest member.
        fetched_at: series
            .iter()
            .map(|s| s.fetched_at)
            .min()
            .unwrap_or(first.fetched_at),
        points,
    })
}

/// Mean of the values, or unavailable when there are none.
///
/// Returning `NotAvailable` rather than 0 matters: an empty fleet has no average CPU,
/// and "0%" would read as a suspiciously idle set of machines.
fn mean(values: impl IntoIterator<Item = f64>) -> MetricValue {
    let values: Vec<f64> = values.into_iter().filter(|v| v.is_finite()).collect();
    if values.is_empty() {
        return MetricValue::NotAvailable;
    }
    let total: f64 = values.iter().sum();
    MetricValue::available(total / values.len() as f64)
}

/// Sum of the values, or unavailable when there are none.
fn sum(values: Vec<f64>) -> MetricValue {
    if values.is_empty() {
        return MetricValue::NotAvailable;
    }
    MetricValue::available(values.iter().sum())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        FakeAlertRepository, FakeAnalyticsRepository, FakeEventRepository, FakeServerRepository,
        FakeWebsiteRepository,
    };
    use chrono::{DateTime, NaiveDate, Utc};
    use vds_domain::analytics::{AnalyticsIntegration, AnalyticsSnapshot};
    use vds_domain::events::DomainEvent;
    use vds_domain::ids::{CredentialRef, ProviderId, ServerId, WebsiteId};
    use vds_domain::ports::FixedClock;
    use vds_domain::server::{
        ConnectionSettings, Server, ServerRuntimeState, SshAuthKind, SshSettings,
    };
    use vds_domain::website::{Website, WebsiteRuntimeState};

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
    }

    fn now() -> DateTime<Utc> {
        day(2026, 8, 26)
            .and_hms_opt(12, 0, 0)
            .expect("valid")
            .and_utc()
    }

    struct Harness {
        service: DashboardQueryService,
        servers: Arc<FakeServerRepository>,
        websites: Arc<FakeWebsiteRepository>,
        analytics: Arc<FakeAnalyticsRepository>,
        events: Arc<FakeEventRepository>,
    }

    fn harness() -> Harness {
        let servers = Arc::new(FakeServerRepository::new());
        let websites = Arc::new(FakeWebsiteRepository::new());
        let analytics = Arc::new(FakeAnalyticsRepository::new());
        let alerts = Arc::new(FakeAlertRepository::new());
        let events = Arc::new(FakeEventRepository::new());

        let service = DashboardQueryService::new(
            Arc::clone(&servers) as Arc<dyn ServerRepository>,
            Arc::clone(&websites) as Arc<dyn WebsiteRepository>,
            Arc::clone(&analytics) as Arc<dyn AnalyticsRepository>,
            Arc::clone(&alerts) as Arc<dyn AlertRepository>,
            Arc::clone(&events) as Arc<dyn EventRepository>,
            Arc::new(FixedClock::new(now())),
        );

        Harness {
            service,
            servers,
            websites,
            analytics,
            events,
        }
    }

    fn server(name: &str) -> Server {
        Server::new(
            name,
            "10.0.0.1",
            ConnectionSettings::Ssh(SshSettings {
                username: "root".into(),
                auth_kind: SshAuthKind::PrivateKey,
                credential_ref: CredentialRef::new(),
            }),
            now(),
        )
    }

    async fn add_server(h: &Harness, name: &str, status: Status, cpu: Option<f64>) -> ServerId {
        let server = server(name);
        let id = server.id;
        h.servers.insert(server);

        let mut state = ServerRuntimeState::unknown(id);
        state.status = status;
        state.cpu_percent = cpu.map_or(MetricValue::NotAvailable, MetricValue::available);
        state.memory_percent = cpu.map_or(MetricValue::NotAvailable, MetricValue::available);
        if status.is_problem() {
            state.last_error = Some("something went wrong".into());
        }
        h.servers.save_state(&state).await.expect("saved");
        id
    }

    async fn add_website(h: &Harness, status: Status, response_ms: Option<u32>) -> WebsiteId {
        let website = Website::new("Example", "https://example.com/", now());
        let id = website.id;
        h.websites.insert(website);

        let mut state = WebsiteRuntimeState::unknown(id);
        state.status = status;
        state.response_ms = response_ms;
        h.websites.save_state(&state).await.expect("saved");
        id
    }

    #[test]
    fn status_counts_tally_correctly() {
        let counts = StatusCounts::tally([
            Status::Healthy,
            Status::Healthy,
            Status::Warning,
            Status::Offline,
        ]);
        assert_eq!(counts.total, 4);
        assert_eq!(counts.healthy, 2);
        assert_eq!(counts.warning, 1);
        assert_eq!(counts.offline, 1);
        assert_eq!(counts.problems(), 2);
        assert_eq!(counts.worst(), Status::Offline);
    }

    #[test]
    fn an_empty_tally_is_unknown_not_healthy() {
        let counts = StatusCounts::tally([]);
        assert_eq!(counts.worst(), Status::Unknown);
        assert_eq!(counts.total, 0);
    }

    #[tokio::test]
    async fn the_infrastructure_summary_counts_and_averages() {
        let h = harness();
        add_server(&h, "prod-01", Status::Healthy, Some(20.0)).await;
        add_server(&h, "prod-02", Status::Warning, Some(80.0)).await;
        add_website(&h, Status::Healthy, Some(100)).await;
        add_website(&h, Status::Healthy, Some(200)).await;

        let summary = h.service.infrastructure().await;

        assert_eq!(summary.servers.total, 2);
        assert_eq!(summary.servers.healthy, 1);
        assert_eq!(summary.servers.warning, 1);
        assert_eq!(summary.average_cpu, MetricValue::Available(50.0));
        assert_eq!(summary.websites.total, 2);
        assert_eq!(summary.average_response_ms, MetricValue::Available(150.0));
    }

    #[tokio::test]
    async fn an_offline_server_does_not_drag_the_fleet_average_towards_zero() {
        // The failure this guards against: an unreachable machine counted as 0% CPU
        // would make a struggling fleet look comfortable.
        let h = harness();
        add_server(&h, "healthy", Status::Healthy, Some(90.0)).await;
        add_server(&h, "offline", Status::Offline, None).await;

        let summary = h.service.infrastructure().await;
        assert_eq!(summary.average_cpu, MetricValue::Available(90.0));
        assert_eq!(summary.servers.offline, 1);
    }

    #[tokio::test]
    async fn an_empty_installation_reports_no_average_rather_than_zero() {
        let h = harness();
        let summary = h.service.infrastructure().await;
        assert_eq!(summary.average_cpu, MetricValue::NotAvailable);
        assert_eq!(summary.average_response_ms, MetricValue::NotAvailable);
        assert_eq!(summary.servers.total, 0);
    }

    #[tokio::test]
    async fn traffic_totals_are_summed_but_rates_are_averaged() {
        // Summing bounce rates across sites would produce a meaningless number over 100%.
        let h = harness();
        let range = AnalyticsPeriod::LastThirtyDays.resolve(day(2026, 8, 26));
        let provider = ProviderId::new("stub");

        for (visitors, bounce) in [(10_000.0, 40.0), (14_821.0, 60.0)] {
            let website = WebsiteId::new();
            h.analytics.insert(AnalyticsIntegration::new(
                website,
                provider.clone(),
                "1",
                CredentialRef::new(),
                now(),
            ));
            h.analytics
                .save_snapshot(
                    &AnalyticsSnapshot::new(website, provider.clone(), range, now())
                        .with(AnalyticsMetric::Visitors, MetricValue::Available(visitors))
                        .with(AnalyticsMetric::BounceRate, MetricValue::Available(bounce)),
                )
                .await
                .expect("saved");
        }

        let traffic = h.service.traffic(AnalyticsPeriod::LastThirtyDays).await;
        assert_eq!(traffic.visitors, MetricValue::Available(24_821.0));
        assert_eq!(traffic.average_bounce_rate, MetricValue::Available(50.0));
        assert_eq!(traffic.sources, 2);
    }

    #[tokio::test]
    async fn traffic_is_unavailable_when_no_analytics_are_configured() {
        let h = harness();
        let traffic = h.service.traffic(AnalyticsPeriod::LastThirtyDays).await;
        assert_eq!(traffic.visitors, MetricValue::NotAvailable);
        assert_eq!(traffic.sources, 0);
    }

    #[tokio::test]
    async fn a_disabled_integration_does_not_contribute_to_totals() {
        let h = harness();
        let range = AnalyticsPeriod::LastThirtyDays.resolve(day(2026, 8, 26));
        let provider = ProviderId::new("stub");
        let website = WebsiteId::new();

        let mut integration =
            AnalyticsIntegration::new(website, provider.clone(), "1", CredentialRef::new(), now());
        integration.enabled = false;
        h.analytics.insert(integration);
        h.analytics
            .save_snapshot(
                &AnalyticsSnapshot::new(website, provider, range, now())
                    .with(AnalyticsMetric::Visitors, MetricValue::Available(9_999.0)),
            )
            .await
            .expect("saved");

        let traffic = h.service.traffic(AnalyticsPeriod::LastThirtyDays).await;
        assert_eq!(traffic.sources, 0);
        assert_eq!(traffic.visitors, MetricValue::NotAvailable);
    }

    /// Registers an enabled integration and caches one series for it.
    async fn add_series(h: &Harness, metric: AnalyticsMetric, values: &[(u32, f64)]) -> WebsiteId {
        let provider = ProviderId::new("stub");
        let website = WebsiteId::new();
        h.analytics.insert(AnalyticsIntegration::new(
            website,
            provider.clone(),
            "1",
            CredentialRef::new(),
            now(),
        ));

        let period = AnalyticsPeriod::LastThirtyDays;
        h.analytics
            .save_time_series(&AnalyticsTimeSeries {
                website_id: website,
                provider,
                metric,
                interval: period.natural_interval(day(2026, 8, 26)),
                range: period.resolve(day(2026, 8, 26)),
                fetched_at: now(),
                points: values
                    .iter()
                    .map(|(d, value)| AnalyticsPoint {
                        timestamp: day(2026, 8, *d)
                            .and_hms_opt(0, 0, 0)
                            .expect("valid")
                            .and_utc(),
                        value: *value,
                    })
                    .collect(),
            })
            .await
            .expect("saved");
        website
    }

    #[tokio::test]
    async fn a_fleet_series_sums_additive_metrics_bucket_by_bucket() {
        let h = harness();
        add_series(&h, AnalyticsMetric::Visitors, &[(24, 100.0), (25, 200.0)]).await;
        add_series(&h, AnalyticsMetric::Visitors, &[(24, 40.0), (25, 60.0)]).await;

        let series = h
            .service
            .traffic_series(AnalyticsPeriod::LastThirtyDays, AnalyticsMetric::Visitors)
            .await
            .expect("a series");

        assert_eq!(series.points.len(), 2);
        assert_eq!(series.points[0].value, 140.0);
        assert_eq!(series.points[1].value, 260.0);
    }

    #[tokio::test]
    async fn a_fleet_series_averages_a_rate_rather_than_summing_it() {
        // Two sites at 40% and 60% bounce is a 50% fleet bounce rate, not 100%.
        let h = harness();
        add_series(&h, AnalyticsMetric::BounceRate, &[(24, 40.0)]).await;
        add_series(&h, AnalyticsMetric::BounceRate, &[(24, 60.0)]).await;

        let series = h
            .service
            .traffic_series(AnalyticsPeriod::LastThirtyDays, AnalyticsMetric::BounceRate)
            .await
            .expect("a series");

        assert_eq!(series.points.len(), 1);
        assert_eq!(series.points[0].value, 50.0);
    }

    #[tokio::test]
    async fn a_bucket_only_one_site_reported_is_kept_at_that_site_s_value() {
        // Intersecting instead would blank the chart whenever a site was added late.
        let h = harness();
        add_series(&h, AnalyticsMetric::Visitors, &[(24, 100.0), (25, 200.0)]).await;
        add_series(&h, AnalyticsMetric::Visitors, &[(25, 50.0)]).await;

        let series = h
            .service
            .traffic_series(AnalyticsPeriod::LastThirtyDays, AnalyticsMetric::Visitors)
            .await
            .expect("a series");

        assert_eq!(series.points.len(), 2);
        assert_eq!(series.points[0].value, 100.0);
        assert_eq!(series.points[1].value, 250.0);
    }

    #[tokio::test]
    async fn points_come_back_in_chronological_order_whatever_the_providers_did() {
        let h = harness();
        add_series(
            &h,
            AnalyticsMetric::Visitors,
            &[(26, 300.0), (24, 100.0), (25, 200.0)],
        )
        .await;

        let series = h
            .service
            .traffic_series(AnalyticsPeriod::LastThirtyDays, AnalyticsMetric::Visitors)
            .await
            .expect("a series");

        let values: Vec<f64> = series.points.iter().map(|p| p.value).collect();
        assert_eq!(values, vec![100.0, 200.0, 300.0]);
        assert!(
            series
                .points
                .windows(2)
                .all(|w| w[0].timestamp < w[1].timestamp)
        );
    }

    #[tokio::test]
    async fn an_uncached_series_is_absent_rather_than_a_flat_line_at_zero() {
        // A chart of zeros would read as "no traffic", which is a different claim from
        // "we have not fetched this yet".
        let h = harness();
        assert!(
            h.service
                .traffic_series(AnalyticsPeriod::LastThirtyDays, AnalyticsMetric::Visitors)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_disabled_integration_contributes_nothing_to_the_fleet_series() {
        let h = harness();
        let website = add_series(&h, AnalyticsMetric::Visitors, &[(24, 9_999.0)]).await;

        let mut integration = h
            .analytics
            .list_integrations_for_website(website)
            .await
            .expect("listed")
            .remove(0);
        integration.enabled = false;
        h.analytics
            .save_integration(&integration)
            .await
            .expect("saved");

        assert!(
            h.service
                .traffic_series(AnalyticsPeriod::LastThirtyDays, AnalyticsMetric::Visitors)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_fleet_series_is_only_as_fresh_as_its_stalest_member() {
        let h = harness();
        add_series(&h, AnalyticsMetric::Visitors, &[(24, 100.0)]).await;

        let stale = AnalyticsTimeSeries {
            website_id: WebsiteId::new(),
            provider: ProviderId::new("stub"),
            metric: AnalyticsMetric::Visitors,
            interval: AnalyticsPeriod::LastThirtyDays.natural_interval(day(2026, 8, 26)),
            range: AnalyticsPeriod::LastThirtyDays.resolve(day(2026, 8, 26)),
            fetched_at: now() - chrono::Duration::hours(6),
            points: vec![AnalyticsPoint {
                timestamp: day(2026, 8, 24)
                    .and_hms_opt(0, 0, 0)
                    .expect("valid")
                    .and_utc(),
                value: 10.0,
            }],
        };
        h.analytics.insert(AnalyticsIntegration::new(
            stale.website_id,
            stale.provider.clone(),
            "2",
            CredentialRef::new(),
            now(),
        ));
        h.analytics.save_time_series(&stale).await.expect("saved");

        let series = h
            .service
            .traffic_series(AnalyticsPeriod::LastThirtyDays, AnalyticsMetric::Visitors)
            .await
            .expect("a series");
        assert_eq!(series.fetched_at, now() - chrono::Duration::hours(6));
    }

    #[tokio::test]
    async fn problem_servers_are_listed_worst_first_with_their_names() {
        let h = harness();
        add_server(&h, "healthy-01", Status::Healthy, Some(10.0)).await;
        add_server(&h, "warning-01", Status::Warning, Some(85.0)).await;
        add_server(&h, "offline-01", Status::Offline, None).await;

        let problems = h.service.problem_servers().await;
        assert_eq!(problems.len(), 2);
        assert_eq!(problems[0].status, Status::Offline);
        assert_eq!(problems[0].name, "offline-01");
        assert!(problems[0].reason.is_some());
        assert_eq!(problems[1].status, Status::Warning);
    }

    #[tokio::test]
    async fn the_event_feed_omits_routine_successes() {
        let h = harness();
        h.events
            .append(&EventEnvelope::new(
                DomainEvent::WebsiteChecked {
                    website_id: WebsiteId::new(),
                    status: Status::Healthy,
                    response_ms: Some(90),
                },
                now(),
            ))
            .await
            .expect("appended");
        h.events
            .append(&EventEnvelope::new(
                DomainEvent::ServerStatusChanged {
                    server_id: ServerId::new(),
                    from: Status::Healthy,
                    to: Status::Offline,
                    reason: None,
                },
                now(),
            ))
            .await
            .expect("appended");

        let summary = h.service.summary(AnalyticsPeriod::LastThirtyDays).await;
        assert_eq!(summary.recent_events.len(), 1);
        assert_eq!(
            summary.recent_events[0].event.kind(),
            "server_status_changed"
        );
    }

    #[tokio::test]
    async fn the_summary_combines_infrastructure_and_traffic() {
        // The combined dashboard the brief asks for.
        let h = harness();
        add_server(&h, "prod-01", Status::Healthy, Some(42.0)).await;
        add_website(&h, Status::Healthy, Some(142)).await;

        let summary = h.service.summary(AnalyticsPeriod::LastThirtyDays).await;
        assert_eq!(summary.infrastructure.servers.total, 1);
        assert_eq!(
            summary.infrastructure.average_cpu,
            MetricValue::Available(42.0)
        );
        assert_eq!(summary.infrastructure.websites.total, 1);
        assert_eq!(summary.traffic.sources, 0);
        assert_eq!(summary.open_incidents, 0);
    }

    #[tokio::test]
    async fn a_failing_repository_degrades_the_dashboard_rather_than_breaking_it() {
        // A dashboard that renders partially is far better than one that renders nothing.
        let h = harness();
        add_server(&h, "prod-01", Status::Healthy, Some(42.0)).await;
        h.servers.fail_all(true);

        let summary = h.service.summary(AnalyticsPeriod::LastThirtyDays).await;
        assert_eq!(summary.infrastructure.servers.total, 0);
        assert_eq!(
            summary.infrastructure.average_cpu,
            MetricValue::NotAvailable
        );
    }
}
