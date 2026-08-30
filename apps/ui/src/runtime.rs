//! The bridge between the application layer and the window.
//!
//! Two rules hold this together:
//!
//! * **the UI thread never blocks.** Every query runs on the Tokio runtime; results
//!   reach the window through `invoke_from_event_loop`, which is the only way to touch a
//!   Slint property from another thread.
//! * **the window never calls a service.** Callbacks turn a click into an *intent* that
//!   is queued; the runtime decides what that means. That is what keeps `screens.slint`
//!   free of anything but layout.

use crate::payload::{ChartPayload, WebsiteCardPayload};
use crate::view_model as vm;
use crate::{AlertRow, AppWindow, EventRow, ServerRow, StatCard, TopPageRow};
use slint::SharedString;
use std::path::PathBuf;
use std::sync::Arc;
use vds_application::analytics::DataOrigin;
use vds_application::dashboard::DashboardSummary;
use vds_composition::Application;
use vds_domain::analytics::{AnalyticsMetric, AnalyticsPeriod};
use vds_domain::ids::{ServerId, WebsiteId};
use vds_domain::metrics::{MetricKind, MetricValue, TimeRange};
use vds_domain::screenshot::ScreenshotPresentation;
use vds_domain::server::SshAuthKind;

/// Time ranges offered on a metrics chart, in the order they appear.
pub const RANGES: &[TimeRange] = &[
    TimeRange::LastHour,
    TimeRange::LastSixHours,
    TimeRange::LastDay,
    TimeRange::LastWeek,
    TimeRange::LastMonth,
    TimeRange::LastQuarter,
];

/// Analytics periods, in the order they appear.
pub const PERIODS: &[AnalyticsPeriod] = &[
    AnalyticsPeriod::Today,
    AnalyticsPeriod::Yesterday,
    AnalyticsPeriod::LastSevenDays,
    AnalyticsPeriod::LastThirtyDays,
    AnalyticsPeriod::LastNinetyDays,
];

/// Metrics offered on the analytics chart switcher.
pub const ANALYTICS_METRICS: &[AnalyticsMetric] = &[
    AnalyticsMetric::Visitors,
    AnalyticsMetric::Visits,
    AnalyticsMetric::PageViews,
    AnalyticsMetric::BounceRate,
];

/// Charts shown on a server's metrics tab.
const SERVER_CHARTS: &[MetricKind] = &[
    MetricKind::CpuUsage,
    MetricKind::MemoryUsage,
    MetricKind::DiskUsage,
    MetricKind::NetworkRxBytesPerSec,
    MetricKind::LoadAverage1,
];

/// Metrics charted on a website's analytics tab, in order.
const WEBSITE_CHARTS: &[AnalyticsMetric] = &[
    AnalyticsMetric::Visitors,
    AnalyticsMetric::PageViews,
    AnalyticsMetric::BounceRate,
];

/// How many rows the top-pages table shows.
const TOP_PAGE_LIMIT: u32 = 10;

/// Chart geometry is computed against this box, and the `.slint` `Chart` component uses
/// the same numbers. They have to agree or the path lands outside the plot area.
const CHART_WIDTH: f32 = 560.0;
const CHART_HEIGHT: f32 = 152.0;

/// Gathers everything the window shows, off the UI thread.
///
/// Nothing here touches a Slint object: every method returns plain data or a payload
/// from [`crate::payload`], which is what lets all of it run on a worker.
pub struct Runtime {
    application: Arc<Application>,
}

impl Runtime {
    pub fn new(application: Arc<Application>) -> Self {
        Self { application }
    }

    /// The labels for the range switcher.
    pub fn range_labels() -> Vec<SharedString> {
        RANGES
            .iter()
            .map(|r| SharedString::from(crate::format::range_label(*r)))
            .collect()
    }

    /// The labels for the analytics period switcher.
    pub fn period_labels() -> Vec<SharedString> {
        PERIODS
            .iter()
            .map(|p| SharedString::from(crate::format::period_label(*p)))
            .collect()
    }

    /// The labels for the analytics metric switcher.
    pub fn analytics_metric_labels() -> Vec<SharedString> {
        ANALYTICS_METRICS
            .iter()
            .map(|m| SharedString::from(crate::format::analytics_metric_label(*m)))
            .collect()
    }

    /// Builds the dashboard's stat tiles.
    pub fn infrastructure_cards(summary: &DashboardSummary) -> Vec<StatCard> {
        let strings = crate::i18n::strings();
        let infra = &summary.infrastructure;
        vec![
            vm::stat_card(
                strings.tile_servers,
                infra.servers.total.to_string(),
                "",
                "",
            ),
            vm::stat_card(
                strings.tile_online,
                infra.servers.healthy.to_string(),
                format!(
                    "{}: {}",
                    strings.dash_needs_attention,
                    infra.servers.problems()
                ),
                if infra.servers.problems() > 0 {
                    "warning"
                } else {
                    ""
                },
            ),
            vm::stat_card(
                strings.tile_websites,
                infra.websites.total.to_string(),
                "",
                "",
            ),
            vm::stat_card(
                strings.tile_offline,
                infra.websites.offline.to_string(),
                "",
                if infra.websites.offline > 0 {
                    "critical"
                } else {
                    ""
                },
            ),
            vm::stat_card(
                strings.tile_average_cpu,
                crate::format::percent(infra.average_cpu),
                "",
                "",
            ),
            vm::stat_card(
                strings.tile_average_ram,
                crate::format::percent(infra.average_memory),
                "",
                "",
            ),
        ]
    }

    /// Builds the traffic tiles.
    pub fn traffic_cards(summary: &DashboardSummary) -> Vec<StatCard> {
        let strings = crate::i18n::strings();
        let traffic = &summary.traffic;
        let count = vds_domain::metrics::MetricUnit::Count;
        vec![
            vm::stat_card(
                strings.tile_visitors,
                crate::format::metric(traffic.visitors, count),
                "",
                "",
            ),
            vm::stat_card(
                strings.tile_visits,
                crate::format::metric(traffic.visits, count),
                "",
                "",
            ),
            vm::stat_card(
                strings.tile_page_views,
                crate::format::metric(traffic.page_views, count),
                "",
                "",
            ),
            vm::stat_card(
                strings.tile_bounce_rate,
                crate::format::percent(traffic.average_bounce_rate),
                "",
                "",
            ),
        ]
    }

    /// Where captures are written.
    ///
    /// The configured location wins; otherwise the platform data directory. The UI thread
    /// needs this to resolve the filenames carried by payloads.
    pub fn screenshot_dir(&self) -> PathBuf {
        self.application
            .configuration
            .storage
            .screenshot_dir
            .clone()
            .unwrap_or_else(|| self.application.paths.screenshots.clone())
    }

    /// Refreshes everything the dashboard shows.
    pub async fn dashboard(&self, period: AnalyticsPeriod) -> DashboardSnapshot {
        let application = Arc::clone(&self.application);
        let summary = application.dashboard.summary(period).await;
        let now = application.clock.now();

        let servers = application.servers.list().await.unwrap_or_default();
        let states = application.servers.list_states().await.unwrap_or_default();
        let websites = application.websites.list().await.unwrap_or_default();

        let problem_servers = summary
            .problem_servers
            .iter()
            .filter_map(|problem| {
                let server = servers.iter().find(|s| s.id == problem.id)?;
                let state = states.iter().find(|s| s.server_id == problem.id)?;
                Some(vm::server_row(server, state, now))
            })
            .collect();

        let recent_events = summary
            .recent_events
            .iter()
            .take(8)
            .map(|e| vm::event_row(e, now))
            .collect();

        let recent_alerts = {
            let incidents = application
                .alerts_repository
                .recent_incidents(6)
                .await
                .unwrap_or_default();
            incidents
                .iter()
                .map(|i| vm::incident_row(i, "", now))
                .collect()
        };

        // The dashboard's CPU chart follows the *busiest* server rather than the fleet
        // average: an average across a quiet fleet hides the one machine that is on fire.
        let cpu_chart = match states.iter().max_by(|a, b| {
            a.cpu_percent
                .value_or(-1.0)
                .partial_cmp(&b.cpu_percent.value_or(-1.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        }) {
            Some(busiest) => {
                let name = servers
                    .iter()
                    .find(|s| s.id == busiest.server_id)
                    .map(|s| s.name.as_str())
                    .unwrap_or("server");
                self.metric_chart(
                    busiest.server_id,
                    MetricKind::CpuUsage,
                    TimeRange::LastDay,
                    name,
                )
                .await
            }
            None => ChartPayload::new("CPU", crate::chart::ChartGeometry::default()),
        };

        // The dashboard's traffic chart always shows visitors: it is the one metric
        // every provider supports, so the tile is never empty for want of a capability.
        let visitors_chart = self
            .traffic_chart(period, AnalyticsMetric::Visitors)
            .await
            .unwrap_or_else(|| {
                ChartPayload::new("Visitors", crate::chart::ChartGeometry::default())
            });

        DashboardSnapshot {
            infrastructure_cards: Self::infrastructure_cards(&summary),
            traffic_cards: Self::traffic_cards(&summary),
            visitors_chart,
            problem_servers,
            recent_alerts,
            recent_events,
            has_analytics: summary.traffic.sources > 0,
            is_empty: vm::is_dashboard_empty(servers.len(), websites.len()),
            open_incidents: i32::try_from(summary.open_incidents).unwrap_or(i32::MAX),
            cpu_chart,
        }
    }

    /// The fleet-wide traffic chart for one metric, if anything is cached for it.
    ///
    /// `None` rather than an empty chart when no analytics are configured at all, so the
    /// caller can decide between "not set up" and "set up but quiet".
    pub async fn traffic_chart(
        &self,
        period: AnalyticsPeriod,
        metric: AnalyticsMetric,
    ) -> Option<ChartPayload> {
        let series = self
            .application
            .dashboard
            .traffic_series(period, metric)
            .await?;
        let geometry = crate::chart::build_analytics(&series, CHART_WIDTH, CHART_HEIGHT);
        Some(ChartPayload::new(
            vm::analytics_chart_title(metric, period.label()),
            geometry,
        ))
    }

    /// Everything the analytics screen shows.
    pub async fn analytics(
        &self,
        period: AnalyticsPeriod,
        metric: AnalyticsMetric,
    ) -> AnalyticsUpdate {
        let summary = self.application.dashboard.summary(period).await;
        let integrations = self
            .application
            .analytics_repository
            .list_integrations()
            .await
            .unwrap_or_default();

        // The selected metric leads; visitors follow underneath so the shape of the
        // traffic is always visible even when the switcher is on a rate.
        let mut charts = Vec::with_capacity(2);
        if let Some(chart) = self.traffic_chart(period, metric).await {
            charts.push(chart);
        }
        if metric != AnalyticsMetric::Visitors
            && let Some(chart) = self.traffic_chart(period, AnalyticsMetric::Visitors).await
        {
            charts.push(chart);
        }

        let updated = match integrations.iter().find(|i| i.enabled) {
            Some(integration) => self
                .analytics_age(integration.website_id, period)
                .await
                .map(|when| format!("Updated {when}"))
                .unwrap_or_default(),
            None => String::new(),
        };

        AnalyticsUpdate {
            cards: Self::traffic_cards(&summary),
            charts,
            configured: integrations.iter().any(|i| i.enabled),
            updated,
        }
    }

    /// The top pages of one website, when its provider offers the report.
    pub async fn top_pages(&self, website: WebsiteId, period: AnalyticsPeriod) -> Vec<TopPageRow> {
        self.application
            .analytics
            .top_pages(website, period, TOP_PAGE_LIMIT)
            .await
            .unwrap_or_default()
            .iter()
            .map(vm::top_page_row)
            .collect()
    }

    /// The charts on one website's analytics tab.
    pub async fn website_charts(
        &self,
        website: WebsiteId,
        period: AnalyticsPeriod,
    ) -> Vec<ChartPayload> {
        let mut charts = Vec::with_capacity(WEBSITE_CHARTS.len());
        for metric in WEBSITE_CHARTS {
            let Some(series) = self
                .application
                .analytics
                .series(website, period, *metric)
                .await
            else {
                // The provider does not serve this metric. Omitting the chart is the
                // capability-driven hiding the architecture calls for — an empty chart
                // would claim the site had no traffic.
                continue;
            };
            let geometry = crate::chart::build_analytics(&series.value, CHART_WIDTH, CHART_HEIGHT);
            charts.push(ChartPayload::new(
                vm::analytics_chart_title(*metric, period.label()),
                geometry,
            ));
        }
        charts
    }

    /// Builds one metric chart.
    pub async fn metric_chart(
        &self,
        server: ServerId,
        kind: MetricKind,
        range: TimeRange,
        subject: &str,
    ) -> ChartPayload {
        let now = self.application.clock.now();
        let window = range.window(now);

        let series = self
            .application
            .metrics
            .series(server, kind, window, range.resolution())
            .await
            .unwrap_or_else(|_| {
                vds_domain::metrics::MetricSeries::empty(kind, range.resolution(), window)
            });

        let geometry = crate::chart::build(&series, CHART_WIDTH, CHART_HEIGHT);
        let title = if subject.is_empty() {
            vm::chart_title(kind, range.label())
        } else {
            format!("{} · {}", subject, vm::chart_title(kind, range.label()))
        };
        vm::chart_data(&title, &geometry)
    }

    /// Every chart on a server's metrics tab.
    pub async fn server_charts(&self, server: ServerId, range: TimeRange) -> Vec<ChartPayload> {
        let mut charts = Vec::with_capacity(SERVER_CHARTS.len());
        for kind in SERVER_CHARTS {
            charts.push(self.metric_chart(server, *kind, range, "").await);
        }
        charts
    }

    /// Website cards, with the *filename* of each preview.
    pub async fn website_cards(&self) -> Vec<WebsiteCardPayload> {
        let application = Arc::clone(&self.application);
        let websites = application.websites.list().await.unwrap_or_default();
        let now = application.clock.now();

        let mut cards = Vec::with_capacity(websites.len());
        for website in &websites {
            let state = application
                .websites
                .load_state(website.id)
                .await
                .unwrap_or_else(|_| vds_domain::website::WebsiteRuntimeState::unknown(website.id));

            let preview = application.screenshots.presentation(website.id).await;
            let thumbnail = match &preview {
                ScreenshotPresentation::Cached { screenshot, .. } => {
                    // Prefer the thumbnail; fall back to the full image so a card is
                    // never blank just because thumbnailing failed.
                    Some(
                        screenshot
                            .thumbnail_path
                            .clone()
                            .unwrap_or_else(|| screenshot.path.clone()),
                    )
                }
                _ => None,
            };

            let uptime = application
                .websites
                .uptime(website.id, TimeRange::LastDay.window(now))
                .await
                .ok()
                .and_then(|summary| summary.percent());

            let visitors = self.visitors_today(website.id).await;

            cards.push(vm::website_card(
                website, &state, &preview, thumbnail, visitors, uptime,
            ));
        }
        cards
    }

    /// Today's visitors for one website, if analytics are configured for it.
    pub async fn visitors_today(&self, website: WebsiteId) -> MetricValue {
        match self
            .application
            .analytics
            .overview(website, AnalyticsPeriod::Today)
            .await
        {
            Some(sourced) => sourced.value.get(AnalyticsMetric::Visitors),
            None => MetricValue::NotAvailable,
        }
    }

    /// How stale the analytics on screen are.
    pub async fn analytics_age(
        &self,
        website: WebsiteId,
        period: AnalyticsPeriod,
    ) -> Option<String> {
        let sourced = self.application.analytics.overview(website, period).await?;
        let now = self.application.clock.now();
        let age = crate::format::relative_time(Some(sourced.fetched_at), now);
        Some(match sourced.origin {
            DataOrigin::Fresh => "just now".to_owned(),
            DataOrigin::Cached => age,
        })
    }

    pub fn application(&self) -> &Arc<Application> {
        &self.application
    }
}

/// Everything the dashboard needs, gathered off the UI thread.
///
/// Every field is `Send`; the conversion into view objects happens in
/// [`DashboardSnapshot::apply`], which runs on the UI thread.
pub struct DashboardSnapshot {
    pub infrastructure_cards: Vec<StatCard>,
    pub traffic_cards: Vec<StatCard>,
    pub problem_servers: Vec<ServerRow>,
    pub recent_alerts: Vec<AlertRow>,
    pub recent_events: Vec<EventRow>,
    pub has_analytics: bool,
    pub is_empty: bool,
    pub open_incidents: i32,
    pub cpu_chart: ChartPayload,
    pub visitors_chart: ChartPayload,
}

impl DashboardSnapshot {
    /// Pushes the snapshot into the window.
    ///
    /// Must run on the UI thread; the caller arranges that with
    /// `slint::invoke_from_event_loop`.
    pub fn apply(self, window: &AppWindow) {
        window.set_infrastructure_cards(vm::model(self.infrastructure_cards));
        window.set_traffic_cards(vm::model(self.traffic_cards));
        window.set_problem_servers(vm::model(self.problem_servers));
        window.set_recent_alerts(vm::model(self.recent_alerts));
        window.set_recent_events(vm::model(self.recent_events));
        window.set_has_analytics(self.has_analytics);
        window.set_dashboard_empty(self.is_empty);
        window.set_open_alert_count(self.open_incidents);
        window.set_cpu_chart(self.cpu_chart.into_view());
        window.set_visitors_chart(self.visitors_chart.into_view());
    }
}

/// Everything the analytics screen needs.
pub struct AnalyticsUpdate {
    pub cards: Vec<StatCard>,
    pub charts: Vec<ChartPayload>,
    pub configured: bool,
    pub updated: String,
}

impl AnalyticsUpdate {
    /// Must run on the UI thread.
    pub fn apply(self, window: &AppWindow) {
        window.set_analytics_cards(vm::model(self.cards));
        window.set_analytics_charts(vm::model(
            self.charts
                .into_iter()
                .map(ChartPayload::into_view)
                .collect(),
        ));
        window.set_analytics_configured(self.configured);
        window.set_analytics_updated(self.updated.into());
    }
}

/// Resolves the connection-mode picker to a mode.
///
/// The index mirrors the `ComboBox` in `dialogs.slint`; anything unexpected falls back to
/// SSH, which is the mode that needs nothing installed on the target.
pub fn is_agent_mode(index: i32) -> bool {
    index == 1
}

/// Resolves the authentication picker.
///
/// Out of range falls back to a password, which is the mode where a wrong guess is
/// visible immediately rather than producing a confusing key parse error.
pub fn auth_kind_at(index: i32) -> SshAuthKind {
    match index {
        1 => SshAuthKind::PrivateKey,
        2 => SshAuthKind::EncryptedPrivateKey,
        _ => SshAuthKind::Password,
    }
}

/// The reverse: which entry in the picker a stored method corresponds to.
///
/// Needed when a form is opened for editing — the dialog has to start on the method the
/// server actually uses, not on the first option.
pub fn auth_kind_index(kind: SshAuthKind) -> i32 {
    match kind {
        SshAuthKind::Password => 0,
        SshAuthKind::PrivateKey => 1,
        SshAuthKind::EncryptedPrivateKey => 2,
    }
}

/// Parses a number a user typed, falling back when the field is empty or nonsense.
///
/// A blank interval field means "leave it at the default", not "poll zero times a
/// second" — and a validation error for an untouched field would be unhelpful.
pub fn number_or<T: std::str::FromStr>(raw: &str, fallback: T) -> T {
    raw.trim().parse().unwrap_or(fallback)
}

/// Resolves a switcher index back to the value it stands for.
///
/// Out-of-range indices fall back to the first entry rather than panicking: the index
/// comes from the view, and a view should never be able to crash the application.
pub fn range_at(index: i32) -> TimeRange {
    usize::try_from(index)
        .ok()
        .and_then(|i| RANGES.get(i))
        .copied()
        .unwrap_or(TimeRange::LastDay)
}

pub fn period_at(index: i32) -> AnalyticsPeriod {
    usize::try_from(index)
        .ok()
        .and_then(|i| PERIODS.get(i))
        .copied()
        .unwrap_or(AnalyticsPeriod::LastSevenDays)
}

pub fn analytics_metric_at(index: i32) -> AnalyticsMetric {
    usize::try_from(index)
        .ok()
        .and_then(|i| ANALYTICS_METRICS.get(i))
        .copied()
        .unwrap_or(AnalyticsMetric::Visitors)
}

/// Resolves a theme index to whether dark mode is on.
///
/// 0 light · 1 dark · 2 follow the system.
pub fn dark_from_theme_index(index: i32, system_prefers_dark: bool) -> bool {
    match index {
        0 => false,
        1 => true,
        _ => system_prefers_dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_application::dashboard::{InfrastructureSummary, StatusCounts, TrafficSummary};

    fn summary_with(servers: StatusCounts, websites: StatusCounts) -> DashboardSummary {
        DashboardSummary {
            infrastructure: InfrastructureSummary {
                servers,
                websites,
                average_cpu: MetricValue::Available(42.0),
                average_memory: MetricValue::Available(63.0),
                average_response_ms: MetricValue::Available(142.0),
            },
            traffic: TrafficSummary {
                visitors: MetricValue::Available(24_821.0),
                visits: MetricValue::Available(31_442.0),
                page_views: MetricValue::Available(89_104.0),
                average_bounce_rate: MetricValue::Available(42.5),
                average_session_duration: MetricValue::NotAvailable,
                sources: 2,
            },
            open_incidents: 1,
            recent_events: Vec::new(),
            problem_servers: Vec::new(),
        }
    }

    #[test]
    fn the_dashboard_tiles_match_the_layout_from_the_brief() {
        // "12 Servers · 11 Online · 28 Websites · Average CPU 42% · Average RAM 63%"
        let summary = summary_with(
            StatusCounts {
                total: 12,
                healthy: 11,
                warning: 1,
                ..Default::default()
            },
            StatusCounts {
                total: 28,
                healthy: 27,
                offline: 1,
                ..Default::default()
            },
        );

        let cards = Runtime::infrastructure_cards(&summary);
        assert_eq!(cards[0].value, "12");
        assert_eq!(cards[1].value, "11");
        // "Label: n" rather than "n need attention". English inflects the verb with the
        // count; Russian needs three plural forms for the noun. A label and a number is
        // the one shape that translates without a pluralisation engine.
        assert_eq!(cards[1].detail, "Needs attention: 1");
        assert_eq!(cards[2].value, "28");
        assert_eq!(cards[3].value, "1");
        assert_eq!(cards[4].value, "42.0%");
        assert_eq!(cards[5].value, "63.0%");
    }

    #[test]
    fn a_healthy_fleet_produces_no_coloured_tiles() {
        let summary = summary_with(
            StatusCounts {
                total: 12,
                healthy: 12,
                ..Default::default()
            },
            StatusCounts {
                total: 28,
                healthy: 28,
                ..Default::default()
            },
        );

        let cards = Runtime::infrastructure_cards(&summary);
        assert!(
            cards.iter().all(|c| c.status.is_empty()),
            "nothing should be tinted when everything is fine"
        );
    }

    #[test]
    fn an_offline_website_tints_its_tile_critical() {
        let summary = summary_with(
            StatusCounts {
                total: 1,
                healthy: 1,
                ..Default::default()
            },
            StatusCounts {
                total: 2,
                healthy: 1,
                offline: 1,
                ..Default::default()
            },
        );
        assert_eq!(
            Runtime::infrastructure_cards(&summary)[3].status,
            "critical"
        );
    }

    #[test]
    fn traffic_tiles_are_grouped_with_separators() {
        let summary = summary_with(StatusCounts::default(), StatusCounts::default());
        let cards = Runtime::traffic_cards(&summary);
        assert_eq!(cards[0].value, "24 821");
        assert_eq!(cards[1].value, "31 442");
        assert_eq!(cards[2].value, "89 104");
        assert_eq!(cards[3].value, "42.5%");
    }

    #[test]
    fn an_unmeasured_average_shows_a_dash_not_a_zero() {
        let mut summary = summary_with(StatusCounts::default(), StatusCounts::default());
        summary.infrastructure.average_cpu = MetricValue::NotAvailable;
        assert_eq!(
            Runtime::infrastructure_cards(&summary)[4].value,
            crate::format::UNAVAILABLE
        );
    }

    #[test]
    fn switcher_indices_resolve_to_the_labelled_values() {
        assert_eq!(range_at(0), TimeRange::LastHour);
        assert_eq!(range_at(2), TimeRange::LastDay);
        assert_eq!(period_at(0), AnalyticsPeriod::Today);
        assert_eq!(analytics_metric_at(2), AnalyticsMetric::PageViews);
    }

    #[test]
    fn an_out_of_range_index_falls_back_rather_than_panicking() {
        // The index comes from the view; a view must never be able to crash the app.
        assert_eq!(range_at(-1), TimeRange::LastDay);
        assert_eq!(range_at(9_999), TimeRange::LastDay);
        assert_eq!(period_at(-5), AnalyticsPeriod::LastSevenDays);
        assert_eq!(analytics_metric_at(100), AnalyticsMetric::Visitors);
    }

    #[test]
    fn the_labels_line_up_with_the_values_they_select() {
        assert_eq!(Runtime::range_labels().len(), RANGES.len());
        assert_eq!(Runtime::period_labels().len(), PERIODS.len());
        assert_eq!(
            Runtime::analytics_metric_labels().len(),
            ANALYTICS_METRICS.len()
        );

        for (index, label) in Runtime::range_labels().iter().enumerate() {
            assert_eq!(label.as_str(), range_at(index as i32).label());
        }
    }

    #[test]
    fn the_system_theme_is_followed_only_on_the_third_option() {
        assert!(!dark_from_theme_index(0, true), "light must stay light");
        assert!(dark_from_theme_index(1, false), "dark must stay dark");
        assert!(dark_from_theme_index(2, true));
        assert!(!dark_from_theme_index(2, false));
    }

    #[test]
    fn the_connection_mode_picker_defaults_to_ssh() {
        // SSH needs nothing installed on the target, so an unexpected index landing there
        // is the harmless direction to fall.
        assert!(!is_agent_mode(0));
        assert!(is_agent_mode(1));
        assert!(!is_agent_mode(7));
        assert!(!is_agent_mode(-1));
    }

    #[test]
    fn the_authentication_picker_maps_to_the_documented_order() {
        assert_eq!(auth_kind_at(0), SshAuthKind::Password);
        assert_eq!(auth_kind_at(1), SshAuthKind::PrivateKey);
        assert_eq!(auth_kind_at(2), SshAuthKind::EncryptedPrivateKey);
        assert_eq!(auth_kind_at(99), SshAuthKind::Password);
    }

    #[test]
    fn a_blank_number_field_falls_back_rather_than_becoming_zero() {
        // A zero poll interval would be rejected by validation, which is a poor answer
        // for a field the user simply did not touch.
        assert_eq!(number_or("", 30u32), 30);
        assert_eq!(number_or("   ", 30u32), 30);
        assert_eq!(number_or("not a number", 30u32), 30);
        assert_eq!(number_or("15", 30u32), 15);
        assert_eq!(number_or(" 15 ", 30u32), 15);
        assert_eq!(number_or("9443", 22u16), 9443);
    }
}
