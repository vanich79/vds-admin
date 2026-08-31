//! Reading the application layer into the shapes the window renders.
//!
//! Every function here runs on the worker, returns plain data, and performs no I/O of its
//! own beyond the repositories it asks. That is what lets the interesting part — which
//! rows appear, what an absent measurement looks like — be decided away from the UI
//! thread and tested without a window.

use crate::payload::{ChartPayload, ServerDetailPayload, WebsiteDetailPayload};
use crate::worker::into_charts;
use crate::{
    AlertRow, AppWindow, ContainerRow, EventRow, ProcessRow, ServerRow, ServiceRow, TopPageRow,
    runtime, view_model,
};
use std::path::PathBuf;
use std::sync::Arc;
use vds_composition::Application;
use vds_domain::analytics::AnalyticsPeriod;
use vds_domain::ids::{ServerId, WebsiteId};

/// Every server, as rows.
pub(crate) async fn server_rows(application: &Arc<Application>) -> Vec<ServerRow> {
    let now = application.clock.now();
    let servers = application.servers.list().await.unwrap_or_default();
    let states = application.servers.list_states().await.unwrap_or_default();

    servers
        .iter()
        .map(|server| {
            let state = states
                .iter()
                .find(|s| s.server_id == server.id)
                .cloned()
                .unwrap_or_else(|| vds_domain::server::ServerRuntimeState::unknown(server.id));
            view_model::server_row(server, &state, now)
        })
        .collect()
}

/// Incidents and rules, as rows.
pub(crate) async fn alert_rows(
    application: &Arc<Application>,
) -> (Vec<AlertRow>, Vec<AlertRow>, Vec<AlertRow>) {
    let now = application.clock.now();
    let servers = application.servers.list().await.unwrap_or_default();
    let websites = application.websites.list().await.unwrap_or_default();

    let name_of = |subject: vds_domain::events::AlertSubject| -> String {
        match subject {
            vds_domain::events::AlertSubject::Server(id) => servers
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "server".to_owned()),
            vds_domain::events::AlertSubject::Website(id) => websites
                .iter()
                .find(|w| w.id == id)
                .map(|w| w.name.clone())
                .unwrap_or_else(|| "website".to_owned()),
        }
    };

    let open = application
        .alerts_repository
        .open_incidents()
        .await
        .unwrap_or_default()
        .iter()
        .map(|incident| view_model::incident_row(incident, &name_of(incident.subject), now))
        .collect();

    let history = application
        .alerts_repository
        .recent_incidents(50)
        .await
        .unwrap_or_default()
        .iter()
        .map(|incident| view_model::incident_row(incident, &name_of(incident.subject), now))
        .collect();

    let rules = application
        .alerts_repository
        .list_rules()
        .await
        .unwrap_or_default()
        .iter()
        .map(view_model::rule_row)
        .collect();

    (open, history, rules)
}

/// A server's detail page, gathered off the UI thread.
pub(crate) struct ServerDetailUpdate {
    detail: ServerDetailPayload,
    charts: Vec<ChartPayload>,
    processes: Vec<ProcessRow>,
    containers: Vec<ContainerRow>,
    services: Vec<ServiceRow>,
    events: Vec<EventRow>,
}

impl ServerDetailUpdate {
    pub(crate) fn apply(self, window: &AppWindow) {
        window.set_server_detail(self.detail.into_view());
        window.set_server_charts(view_model::model(into_charts(self.charts)));
        window.set_processes(view_model::model(self.processes));
        window.set_containers(view_model::model(self.containers));
        window.set_services(view_model::model(self.services));
        window.set_server_events(view_model::model(self.events));
    }
}

pub(crate) async fn server_detail(
    application: &Arc<Application>,
    id: ServerId,
    range: vds_domain::metrics::TimeRange,
) -> Option<ServerDetailUpdate> {
    let now = application.clock.now();
    let server = application.servers.get(id).await.ok()?;
    let state = application.servers.load_state(id).await.ok()?;

    let events = application
        .events_repository
        .recent_for_subject(vds_domain::events::AlertSubject::Server(id), 30)
        .await
        .unwrap_or_default()
        .iter()
        .map(|e| view_model::event_row(e, now))
        .collect();

    // Processes, containers and services are not persisted — they are point-in-time
    // facts — so they come from the monitor's last collection. Before the first
    // collection there is no snapshot, and the tabs stay empty rather than claiming the
    // machine is running nothing.
    let snapshot = application.server_monitor.last_snapshot(id);
    let detail = view_model::server_detail(&server, &state, snapshot.as_deref(), now);

    let processes = snapshot
        .as_ref()
        .map(|s| s.processes.iter().map(view_model::process_row).collect())
        .unwrap_or_default();
    let containers = snapshot
        .as_ref()
        .and_then(|s| s.containers.as_ref())
        .map(|c| c.iter().map(view_model::container_row).collect())
        .unwrap_or_default();
    let services = snapshot
        .as_ref()
        .and_then(|s| s.services.as_ref())
        .map(|c| c.iter().map(view_model::service_row).collect())
        .unwrap_or_default();

    let runtime = runtime::Runtime::new(Arc::clone(application));
    let charts = runtime.server_charts(id, range).await;

    Some(ServerDetailUpdate {
        detail,
        charts,
        processes,
        containers,
        services,
        events,
    })
}

/// A website's detail page.
pub(crate) struct WebsiteDetailUpdate {
    detail: WebsiteDetailPayload,
    charts: Vec<ChartPayload>,
    top_pages: Vec<TopPageRow>,
    events: Vec<EventRow>,
    /// Where the capture named by the payload lives; the decode happens in `apply`.
    directory: PathBuf,
    /// The counter this website is connected to, empty when it is not. Shown next to the
    /// figures so a wrong number is visible rather than inferred from odd traffic.
    counter: String,
    /// Whether the shared token exists, which decides whether the connect form is usable.
    token_saved: bool,
}

impl WebsiteDetailUpdate {
    pub(crate) fn apply(self, window: &AppWindow) {
        window.set_website_detail(self.detail.into_view(&self.directory));
        window.set_website_charts(view_model::model(into_charts(self.charts)));
        window.set_top_pages(view_model::model(self.top_pages));
        window.set_website_counter(self.counter.into());
        window.set_analytics_token_saved(self.token_saved);
        window.set_website_events(view_model::model(self.events));
    }
}

pub(crate) async fn website_detail(
    runtime: &runtime::Runtime,
    id: WebsiteId,
    period: AnalyticsPeriod,
) -> Option<WebsiteDetailUpdate> {
    let application = Arc::clone(runtime.application());
    let now = application.clock.now();

    let website = application.websites.get(id).await.ok()?;
    let state = application.websites.load_state(id).await.ok()?;
    let preview = application.screenshots.presentation(id).await;

    // The detail page shows the full capture rather than the thumbnail; it travels as a
    // filename and is decoded on the UI thread.
    let directory = runtime.screenshot_dir();
    let thumbnail_file = match &preview {
        vds_domain::screenshot::ScreenshotPresentation::Cached { screenshot, .. } => {
            Some(screenshot.path.clone())
        }
        _ => None,
    };

    let last_check = application
        .websites
        .recent_checks(id, 1)
        .await
        .unwrap_or_default();
    let ssl = last_check.first().and_then(|check| check.ssl.clone());

    let uptime = application
        .websites
        .uptime(id, vds_domain::metrics::TimeRange::LastDay.window(now))
        .await
        .ok()
        .and_then(|summary| summary.percent());

    let integrations = application
        .analytics_repository
        .list_integrations_for_website(id)
        .await
        .unwrap_or_default();
    let has_analytics = integrations.iter().any(|i| i.enabled);

    let visitors = runtime.visitors_today(id).await;
    let updated = if has_analytics {
        runtime.analytics_age(id, AnalyticsPeriod::Today).await
    } else {
        None
    };

    let events = application
        .events_repository
        .recent_for_subject(vds_domain::events::AlertSubject::Website(id), 30)
        .await
        .unwrap_or_default()
        .iter()
        .map(|e| view_model::event_row(e, now))
        .collect();

    let detail = view_model::website_detail(
        &website,
        &state,
        &preview,
        thumbnail_file,
        ssl.as_ref().map(|s| s.subject.clone()),
        ssl.as_ref().map(|s| s.issuer.clone()),
        uptime,
        has_analytics,
        updated,
        visitors,
    );

    // Both are empty when the provider does not offer the report; the tab hides them
    // rather than showing an empty table that would read as "no pages".
    let (charts, top_pages) = if has_analytics {
        (
            runtime.website_charts(id, period).await,
            runtime.top_pages(id, period).await,
        )
    } else {
        (Vec::new(), Vec::new())
    };

    // Which counter, if any, and whether a token exists at all — both are needed before
    // the tab can decide between showing figures and offering to connect.
    let counter = application
        .analytics_repository
        .list_integrations_for_website(id)
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|integration| integration.enabled)
        .map(|integration| integration.external_id)
        .unwrap_or_default();
    let token_saved = application.provisioning.has_analytics_token().await;

    Some(WebsiteDetailUpdate {
        detail,
        charts,
        top_pages,
        events,
        directory,
        counter,
        token_saved,
    })
}
