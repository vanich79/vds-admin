//! Mapping domain values onto the shapes the interface renders.
//!
//! Every function here is pure: domain data in, view rows out. That is what lets the
//! presentation rules — which status a row shows, what an unmeasured metric looks like,
//! whether a Docker panel appears at all — be tested without a window.

use crate::format;
use crate::payload::{ServerDetailPayload, WebsiteCardPayload, WebsiteDetailPayload};
use crate::{
    AlertRow, ContainerRow, EventRow, FileEntry, ProcessRow, ServerRow, ServiceRow, SiteFolder,
    StatCard, TopPageRow,
};
use chrono::{DateTime, Utc};
use slint::{ModelRc, SharedString, VecModel};
use std::rc::Rc;
use vds_application::provisioning::ProvisioningError;
use vds_domain::alerts::{AlertRule, Incident};
use vds_domain::analytics::AnalyticsMetric;
use vds_domain::events::EventEnvelope;
use vds_domain::metrics::{MetricKind, MetricValue};
use vds_domain::screenshot::ScreenshotPresentation;
use vds_domain::server::{
    ContainerInfo, ProcessInfo, Server, ServerRuntimeState, ServerSnapshot, ServiceInfo,
};
use vds_domain::website::{Website, WebsiteRuntimeState};

/// Wraps a vector in the model type Slint expects.
pub fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(items)))
}

/// A fraction in `0.0..=1.0` for a meter bar, or `-1.0` when there is no measurement.
///
/// The negative sentinel is deliberate: the bar draws an empty track rather than a
/// zero-width bar, so "not measured" cannot be mistaken for "idle".
pub fn fraction(value: MetricValue) -> f32 {
    value
        .value()
        .map_or(-1.0, |percent| (percent / 100.0).clamp(0.0, 1.0) as f32)
}

/// One server, as a row.
pub fn server_row(server: &Server, state: &ServerRuntimeState, now: DateTime<Utc>) -> ServerRow {
    ServerRow {
        id: server.id.to_string().into(),
        name: server.name.clone().into(),
        host: format!("{}:{}", server.host, server.port).into(),
        status: state.status.as_str().into(),
        status_label: format::status_label(state.status).into(),
        cpu: format::metric(state.cpu_percent, MetricUnitPercent).into(),
        cpu_fraction: fraction(state.cpu_percent),
        ram: format::metric(state.memory_percent, MetricUnitPercent).into(),
        ram_fraction: fraction(state.memory_percent),
        disk: format::metric(state.disk_percent, MetricUnitPercent).into(),
        disk_fraction: fraction(state.disk_percent),
        uptime: state
            .uptime_secs
            .map_or_else(
                || format::UNAVAILABLE.to_owned(),
                |s| format::duration_secs(s as i64),
            )
            .into(),
        last_check: format::relative_time(state.last_check, now).into(),
        tags: server.tags.join(", ").into(),
    }
}

/// Shorthand: percentages are by far the most common unit in the interface.
#[allow(non_upper_case_globals)]
const MetricUnitPercent: vds_domain::metrics::MetricUnit = vds_domain::metrics::MetricUnit::Percent;

/// A server's detail page header.
///
/// Returns a payload rather than the view struct: this is built on a worker thread, and
/// a `ModelRc` cannot cross one. See [`crate::payload`].
pub fn server_detail(
    server: &Server,
    state: &ServerRuntimeState,
    snapshot: Option<&ServerSnapshot>,
    now: DateTime<Utc>,
) -> ServerDetailPayload {
    let strings = crate::i18n::strings();
    let system = snapshot.map(|s| &s.system);

    let cards = vec![
        stat_card(
            strings.label_cpu,
            format::metric(state.cpu_percent, MetricUnitPercent),
            "",
            threshold_status(state.cpu_percent, 80.0, 95.0),
        ),
        stat_card(
            strings.label_memory,
            format::metric(state.memory_percent, MetricUnitPercent),
            "",
            threshold_status(state.memory_percent, 85.0, 95.0),
        ),
        stat_card(
            strings.label_disk,
            format::metric(state.disk_percent, MetricUnitPercent),
            "",
            threshold_status(state.disk_percent, 85.0, 90.0),
        ),
        stat_card(
            strings.label_uptime,
            state.uptime_secs.map_or_else(
                || format::UNAVAILABLE.to_owned(),
                |s| format::duration_secs(s as i64),
            ),
            "",
            "",
        ),
    ];

    ServerDetailPayload {
        id: server.id.to_string(),
        name: server.name.clone(),
        host: format!("{}:{}", server.host, server.port),
        status: state.status.as_str().to_owned(),
        status_label: format::status_label(state.status).to_owned(),
        os: optional(system.and_then(|s| s.os_name.clone())),
        kernel: optional(system.and_then(|s| s.kernel.clone())),
        architecture: optional(system.and_then(|s| s.architecture.clone())),
        cpu_model: optional(system.and_then(|s| s.cpu_model.clone())),
        cores: system
            .and_then(|s| s.cpu_cores)
            .map_or_else(|| format::UNAVAILABLE.to_owned(), |c| c.to_string()),
        uptime: state.uptime_secs.map_or_else(
            || format::UNAVAILABLE.to_owned(),
            |s| format::duration_secs(s as i64),
        ),
        last_check: format::relative_time(state.last_check, now),
        last_error: state
            .last_error
            .as_deref()
            .map(|detail| describe_connection_error(state.last_error_kind, detail))
            .unwrap_or_default(),
        // The transport's own words, kept beneath the translation for diagnosis.
        last_error_detail: state.last_error.clone().unwrap_or_default(),
        cards,
        // `None` means the collector never ran or the host has no Docker; an empty vector
        // means Docker is there with nothing running. The panel is hidden only in the
        // first case.
        has_docker: snapshot.is_some_and(|s| s.containers.is_some()),
        has_systemd: snapshot.is_some_and(|s| s.services.is_some()),
    }
}

/// One website, as a card.
///
/// The preview travels as a *filename*; it is decoded on the UI thread, where `Image`
/// can exist. See [`crate::payload`].
pub fn website_card(
    website: &Website,
    state: &WebsiteRuntimeState,
    preview: &ScreenshotPresentation,
    thumbnail_file: Option<String>,
    visitors: MetricValue,
    uptime_percent: Option<f64>,
) -> WebsiteCardPayload {
    let strings = crate::i18n::strings();
    let (preview_message, capture_age) = preview_text(preview);

    WebsiteCardPayload {
        id: website.id.to_string(),
        name: website.name.clone(),
        url: website.url.clone(),
        status: state.status.as_str().to_owned(),
        status_label: format::status_label(state.status).to_owned(),
        response: fill(
            strings.card_response,
            &state
                .response_ms
                .map_or_else(|| format::UNAVAILABLE.to_owned(), |ms| format!("{ms} ms")),
        ),
        ssl: fill(
            strings.card_ssl,
            &format::ssl_expiry(state.ssl_days_remaining),
        ),
        uptime: fill(strings.card_uptime_24h, &format::uptime(uptime_percent)),
        visitors: match visitors.value() {
            Some(_) => fill(
                strings.card_visitors_today,
                &format::metric(visitors, vds_domain::metrics::MetricUnit::Count),
            ),
            // No analytics for this site: the line is omitted rather than showing a dash
            // that would imply a provider is connected but silent.
            None => String::new(),
        },
        thumbnail_file,
        preview_message,
        capture_age,
    }
}

/// What to show for a preview, and the age line that always accompanies an image.
pub fn preview_text(preview: &ScreenshotPresentation) -> (String, String) {
    let strings = crate::i18n::strings();
    match preview {
        ScreenshotPresentation::Cached { age, .. } => (
            String::new(),
            // Never shown without its age: presenting a four-hour-old picture as current
            // is the one way this feature could mislead.
            fill(
                strings.shot_captured,
                &vds_application::screenshots::describe_age(*age),
            ),
        ),
        ScreenshotPresentation::Capturing => (strings.shot_capturing.to_owned(), String::new()),
        ScreenshotPresentation::WebsiteOffline => (strings.shot_offline.to_owned(), String::new()),
        ScreenshotPresentation::Failed { reason } => {
            (fill(strings.shot_failed, reason), String::new())
        }
        ScreenshotPresentation::Unavailable => (strings.shot_unsupported.to_owned(), String::new()),
        ScreenshotPresentation::NotCaptured => (strings.shot_none_yet.to_owned(), String::new()),
    }
}

/// A website's detail page.
#[allow(clippy::too_many_arguments)]
pub fn website_detail(
    website: &Website,
    state: &WebsiteRuntimeState,
    preview: &ScreenshotPresentation,
    thumbnail_file: Option<String>,
    ssl_subject: Option<String>,
    ssl_issuer: Option<String>,
    uptime_percent: Option<f64>,
    has_analytics: bool,
    analytics_updated: Option<String>,
    visitors: MetricValue,
) -> WebsiteDetailPayload {
    let strings = crate::i18n::strings();
    let (preview_message, capture_age) = preview_text(preview);

    let mut cards = vec![
        stat_card(
            strings.label_status,
            format::status_label(state.status).to_owned(),
            "",
            state.status.as_str(),
        ),
        stat_card(
            strings.mk_response_time,
            state
                .response_ms
                .map_or_else(|| format::UNAVAILABLE.to_owned(), |ms| format!("{ms} ms")),
            "",
            "",
        ),
        stat_card("Uptime 24h", format::uptime(uptime_percent), "", ""),
        stat_card(
            strings.tab_ssl,
            format::ssl_expiry(state.ssl_days_remaining),
            "",
            ssl_status(state.ssl_days_remaining),
        ),
    ];

    if has_analytics {
        cards.push(stat_card(
            strings.tile_visitors,
            format::metric(visitors, vds_domain::metrics::MetricUnit::Count),
            "",
            "",
        ));
    }

    WebsiteDetailPayload {
        id: website.id.to_string(),
        name: website.name.clone(),
        url: website.url.clone(),
        status: state.status.as_str().to_owned(),
        status_label: format::status_label(state.status).to_owned(),
        http_status: format::http_status(state.http_status),
        response: state
            .response_ms
            .map_or_else(|| format::UNAVAILABLE.to_owned(), |ms| format!("{ms} ms")),
        uptime_24h: format::uptime(uptime_percent),
        ssl_issuer: optional(ssl_issuer),
        ssl_expiry: format::ssl_expiry(state.ssl_days_remaining),
        ssl_subject: optional(ssl_subject),
        thumbnail_file,
        preview_message,
        capture_age,
        cards,
        has_analytics,
        analytics_updated: analytics_updated
            .map(|when| fill(crate::i18n::strings().card_analytics_updated, &when))
            .unwrap_or_default(),
    }
}

/// One incident, as a row.
pub fn incident_row(incident: &Incident, subject_name: &str, now: DateTime<Utc>) -> AlertRow {
    AlertRow {
        id: incident.id.to_string().into(),
        severity: incident.severity.as_str().into(),
        severity_label: format::status_label(incident.severity).into(),
        title: incident.summary.clone().into(),
        detail: fill(
            crate::i18n::strings().incident_open_for,
            &format::duration_secs(incident.duration(now).num_seconds()),
        )
        .into(),
        subject: subject_name.into(),
        opened: format::relative_time(Some(incident.opened_at), now).into(),
        acknowledged: incident.acknowledged,
        resolved: !incident.is_open(),
    }
}

/// One alert rule, as a row.
///
/// Reuses [`AlertRow`] rather than adding a near-identical struct; `resolved` carries
/// "disabled" here, which the rules tab renders as an unchecked box.
pub fn rule_row(rule: &AlertRule) -> AlertRow {
    AlertRow {
        id: rule.id.to_string().into(),
        severity: rule.severity.as_str().into(),
        severity_label: format::status_label(rule.severity).into(),
        title: rule.name.clone().into(),
        detail: rule.condition.describe().into(),
        subject: String::new().into(),
        opened: String::new().into(),
        acknowledged: false,
        resolved: !rule.enabled,
    }
}

/// One event, as a row.
pub fn event_row(envelope: &EventEnvelope, now: DateTime<Utc>) -> EventRow {
    EventRow {
        severity: envelope.event.severity().as_str().into(),
        kind: envelope.event.kind().into(),
        message: describe_event(&envelope.event).into(),
        when: format::relative_time(Some(envelope.occurred_at), now).into(),
    }
}

/// Why a server is not answering, in the user's language.
///
/// The transport's own message is English and already formatted, so it cannot be
/// translated after the fact — the *kind* is carried beside it for exactly this. The
/// original text is not thrown away: it becomes the detail line, because "which key,
/// exactly" is what makes a failure diagnosable.
///
/// A state saved by an older build has no kind, and one saved by a newer build may have
/// a kind this version does not know. Both fall back to the detail alone, which is worse
/// than a translation and much better than an empty box.
pub fn describe_connection_error(
    kind: Option<vds_domain::ports::TransportErrorKind>,
    detail: &str,
) -> String {
    use vds_domain::ports::TransportErrorKind as K;

    let strings = crate::i18n::strings();
    let Some(kind) = kind else {
        return detail.to_owned();
    };

    match kind {
        K::Authentication => strings.conn_auth,
        K::HostKeyRejected => strings.conn_host_key,
        K::Connection => strings.conn_refused,
        K::Timeout => strings.conn_timeout,
        K::Execution => strings.conn_command,
        K::NotConnected => strings.conn_disconnected,
        K::MissingCredential => strings.conn_no_credential,
        K::Protocol => strings.conn_protocol,
    }
    .to_owned()
}

/// A rejected form, phrased for the person who filled it in.
///
/// The domain's own `Display` is written for a log: "server host must not be empty" is
/// accurate, English, and not what belongs in a dialog. Translating it is presentation
/// work, so it happens here rather than in the domain — which is also what keeps the
/// domain free of a language setting.
pub fn describe_provisioning_error(error: &ProvisioningError) -> String {
    use vds_domain::server::ServerValidationError as S;
    use vds_domain::website::WebsiteValidationError as W;

    let strings = crate::i18n::strings();
    match error {
        ProvisioningError::InvalidServer(inner) => match inner {
            S::EmptyName => strings.err_server_name_empty.to_owned(),
            S::EmptyHost => strings.err_server_host_empty.to_owned(),
            S::InvalidPort(_) => strings.err_port_invalid.to_owned(),
            S::InvalidPollInterval => strings.err_interval_invalid.to_owned(),
            S::InvalidFailureThreshold => strings.err_failures_invalid.to_owned(),
            S::InvalidTimeout => strings.err_timeout_invalid.to_owned(),
            S::TimeoutExceedsInterval => strings.err_timeout_too_long.to_owned(),
            S::IncoherentThreshold => strings.err_thresholds_inverted.to_owned(),
        },
        ProvisioningError::InvalidWebsite(inner) => match inner {
            W::EmptyName => strings.err_website_name_empty.to_owned(),
            W::MalformedUrl => strings.err_url_malformed.to_owned(),
            W::UnsupportedScheme(_) => strings.err_url_scheme.to_owned(),
            W::MissingHost => strings.err_url_no_host.to_owned(),
            W::InvalidPollInterval => strings.err_interval_invalid.to_owned(),
            W::InvalidTimeout => strings.err_timeout_invalid.to_owned(),
            W::InvalidFailureThreshold => strings.err_failures_invalid.to_owned(),
            W::InvalidExpectedStatus => strings.err_status_invalid.to_owned(),
        },
        ProvisioningError::MissingCredential => strings.err_credential_missing.to_owned(),
        ProvisioningError::EmptyCounter => strings.err_counter_empty.to_owned(),
        ProvisioningError::MalformedCounter => strings.err_counter_malformed.to_owned(),
        ProvisioningError::MissingAnalyticsToken => strings.err_no_analytics_token.to_owned(),
        // These two carry a cause from the operating system or the database. It is not
        // translated — it comes from outside — but the sentence around it is, so the line
        // still reads as one language with a quotation in it.
        ProvisioningError::Secrets(inner) => fill(strings.err_credential_store, &inner.to_string()),
        ProvisioningError::Repository(inner) => fill(strings.err_save_failed, &inner.to_string()),
    }
}

/// A one-line description of an event for the activity feed.
pub fn describe_event(event: &vds_domain::events::DomainEvent) -> String {
    use vds_domain::events::DomainEvent as E;
    let strings = crate::i18n::strings();
    match event {
        E::ServerStatusChanged { from, to, .. } => fill2(
            strings.ev_server_status,
            format::status_label(*from),
            format::status_label(*to),
        ),
        E::ServerCollectionFailed {
            consecutive_failures,
            error,
            ..
        } => fill2(
            strings.ev_collection_failed,
            &consecutive_failures.to_string(),
            error,
        ),
        E::WebsiteStatusChanged { from, to, .. } => fill2(
            strings.ev_website_status,
            format::status_label(*from),
            format::status_label(*to),
        ),
        E::MetricThresholdExceeded {
            metric,
            value,
            threshold,
            ..
        } => fill3(
            strings.ev_threshold,
            format::metric_kind_label(*metric),
            &format!("{value:.1}"),
            &format!("{threshold:.0}"),
        ),
        E::SslExpiringSoon { days_remaining, .. } => fill(
            strings.ev_certificate,
            &format::ssl_expiry(Some(*days_remaining)),
        ),
        E::TrafficAnomalyDetected {
            change_percent,
            metric,
            ..
        } => fill2(
            strings.ev_traffic_anomaly,
            format::analytics_metric_label(*metric),
            &format::change(Some(*change_percent)),
        ),
        E::AnalyticsUpdated { .. } => strings.ev_analytics_refreshed.to_owned(),
        E::AnalyticsRefreshFailed { error, .. } => fill(strings.ev_analytics_failed, error),
        E::ScreenshotUpdated { .. } => strings.ev_screenshot_updated.to_owned(),
        E::ScreenshotFailed { error, .. } => fill(strings.ev_screenshot_failed, error),
        // The summary was written by the alert rule, which the user named themselves.
        E::IncidentOpened { summary, .. } => summary.clone(),
        E::IncidentResolved { .. } => strings.ev_incident_resolved.to_owned(),
        E::ContainerStateChanged {
            container, state, ..
        } => fill2(strings.ev_container_state, container, state),
        E::ServiceStateChanged { service, state, .. } => {
            fill2(strings.ev_service_state, service, state)
        }
        E::WebsiteChecked { .. } => strings.ev_website_checked.to_owned(),
        E::ServerMetricsCollected { metric_count, .. } => {
            fill(strings.ev_metrics_collected, &metric_count.to_string())
        }
        E::FileChanged { path, action, .. } => fill(
            match action {
                vds_domain::events::FileAction::Written => strings.ev_file_written,
                vds_domain::events::FileAction::Deleted => strings.ev_file_deleted,
                vds_domain::events::FileAction::DirectoryCreated => strings.ev_file_dir_created,
            },
            path,
        ),
    }
}

/// A file operation's failure, in the user's language.
///
/// Keyed on [`FileError::kind`] rather than on the error's own text: the domain formats
/// its messages in English, and a sentence that already exists cannot be translated. The
/// one case that carries the server's own words through is `malformed`, where the useful
/// part is the quotation.
pub fn describe_file_error(error: &vds_domain::ports::FileError) -> String {
    let strings = crate::i18n::strings();
    match error.kind() {
        "not_found" => strings.err_file_not_found.to_owned(),
        "permission_denied" => strings.err_file_denied.to_owned(),
        "not_a_directory" => strings.err_file_not_a_directory.to_owned(),
        "not_a_file" => strings.err_file_not_a_file.to_owned(),
        "not_text" => strings.err_file_not_text.to_owned(),
        "too_large" => strings.err_file_too_large.to_owned(),
        // A broken connection is the transport's story to tell, and it is already
        // translated by the code that handles every other connection failure.
        "transport" => match error {
            vds_domain::ports::FileError::Transport(inner) => {
                describe_connection_error(Some(inner.kind()), &inner.to_string())
            }
            _ => error.to_string(),
        },
        _ => fill(strings.err_file_malformed, &error.to_string()),
    }
}

/// Substitutes one `{}` in a catalogue string.
///
/// The same reasoning as `format::fill`: the pattern comes from the catalogue, so it is
/// not known at compile time and `format!` cannot be used.
fn fill(pattern: &str, value: &str) -> String {
    match pattern.find("{}") {
        Some(at) => format!("{}{}{}", &pattern[..at], value, &pattern[at + 2..]),
        None => pattern.to_owned(),
    }
}

pub fn fill2(pattern: &str, first: &str, second: &str) -> String {
    fill(&fill(pattern, first), second)
}

fn fill3(pattern: &str, first: &str, second: &str, third: &str) -> String {
    fill(&fill2(pattern, first, second), third)
}

/// One directory entry, as a row.
///
/// Everything the view shows is formatted here: sizes in units, dates relative to now,
/// the symlink's target phrased as a sentence. The view lays out; it never decides what
/// an absent value looks like.
pub fn file_entry_row(entry: &vds_domain::ports::DirectoryEntry, now: DateTime<Utc>) -> FileEntry {
    use vds_domain::ports::EntryKind;

    let strings = crate::i18n::strings();
    let kind = match entry.kind {
        EntryKind::File => "file",
        EntryKind::Directory => "directory",
        EntryKind::Symlink => "symlink",
        EntryKind::Other => "other",
    };

    FileEntry {
        name: entry.name.clone().into(),
        kind: kind.into(),
        is_directory: entry.kind.is_directory(),
        can_open: entry.kind.is_readable(),
        // A directory's size is the size of its own inode, which means nothing to anyone.
        size: if entry.kind.is_directory() {
            SharedString::new()
        } else {
            format::bytes(entry.size_bytes as f64).into()
        },
        modified: format::relative_time(entry.modified, now).into(),
        mode: entry.mode.clone().into(),
        owner: entry.owner.clone().into(),
        detail: match &entry.target {
            Some(target) => fill(strings.files_link_to, target).into(),
            None => SharedString::new(),
        },
    }
}

/// One discovered site folder, as a row.
pub fn site_folder_row(root: &vds_application::files::SiteRoot) -> SiteFolder {
    SiteFolder {
        label: root.label().into(),
        path: root.path.clone().into(),
        source: root.source.clone().into(),
    }
}

/// One process, as a row.
pub fn process_row(process: &ProcessInfo) -> ProcessRow {
    ProcessRow {
        pid: process.pid.to_string().into(),
        user: process.user.clone().unwrap_or_default().into(),
        command: process.command.clone().into(),
        cpu: format!("{:.1}%", process.cpu_percent).into(),
        memory: process
            .rss_bytes
            .map_or_else(
                || format!("{:.1}%", process.memory_percent),
                |bytes| format::bytes(bytes as f64),
            )
            .into(),
    }
}

/// One container, as a row.
pub fn container_row(container: &ContainerInfo) -> ContainerRow {
    ContainerRow {
        name: container.name.clone().into(),
        image: container.image.clone().into(),
        status: container.status().as_str().into(),
        status_text: container.status_text.clone().into(),
        cpu: format::metric(container.cpu_percent, MetricUnitPercent).into(),
        memory: container
            .memory_used_bytes
            .map_or_else(
                || format::UNAVAILABLE.to_owned(),
                |b| format::bytes(b as f64),
            )
            .into(),
        restarts: container
            .restart_count
            .map_or_else(
                || format::UNAVAILABLE.to_owned(),
                |c| format!("{c} restarts"),
            )
            .into(),
    }
}

/// One systemd unit, as a row.
pub fn service_row(service: &ServiceInfo) -> ServiceRow {
    ServiceRow {
        name: service.name.clone().into(),
        state: service.state.status().as_str().into(),
        state_label: service.state.as_str().to_uppercase().into(),
        description: service.description.clone().unwrap_or_default().into(),
    }
}

/// One "top page" row.
pub fn top_page_row(page: &vds_domain::analytics::TopPage) -> TopPageRow {
    TopPageRow {
        url: page.url.clone().into(),
        views: format::count(page.page_views).into(),
        visitors: format::metric(page.visitors, vds_domain::metrics::MetricUnit::Count).into(),
    }
}

/// A headline figure.
pub fn stat_card(
    label: &str,
    value: impl Into<String>,
    detail: impl Into<String>,
    status: &str,
) -> StatCard {
    StatCard {
        label: label.into(),
        value: SharedString::from(value.into()),
        detail: SharedString::from(detail.into()),
        status: status.into(),
    }
}

/// Packages computed geometry for the trip to the UI thread.
pub fn chart_data(
    title: &str,
    geometry: &crate::chart::ChartGeometry,
) -> crate::payload::ChartPayload {
    crate::payload::ChartPayload::new(title, geometry.clone())
}

/// Title for a metric chart.
pub fn chart_title(kind: MetricKind, range_label: &str) -> String {
    format!("{} — {range_label}", format::metric_kind_label(kind))
}

/// Title for an analytics chart.
pub fn analytics_chart_title(metric: AnalyticsMetric, period_label: &str) -> String {
    format!(
        "{} — {period_label}",
        format::analytics_metric_label(metric)
    )
}

/// The status tint for a value against two thresholds.
///
/// Returns an empty string for a healthy or unmeasured value, so the tile stays the
/// default colour: tinting every number would make colour stop meaning anything.
fn threshold_status(value: MetricValue, warning: f64, critical: f64) -> &'static str {
    match value.value() {
        Some(v) if v >= critical => "critical",
        Some(v) if v >= warning => "warning",
        _ => "",
    }
}

/// The status tint for a certificate's remaining life.
fn ssl_status(days: Option<i64>) -> &'static str {
    match days {
        Some(d) if d < 0 => "critical",
        Some(d) if d <= 3 => "critical",
        Some(d) if d <= 14 => "warning",
        _ => "",
    }
}

/// An optional string, or the standard dash.
fn optional(value: Option<String>) -> String {
    value
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| format::UNAVAILABLE.to_owned())
}

/// Whether the dashboard has anything at all to show.
pub fn is_dashboard_empty(servers: usize, websites: usize) -> bool {
    servers == 0 && websites == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::Status;
    use vds_domain::ids::{CredentialRef, IncidentId, ServerId, WebsiteId};
    use vds_domain::server::{
        ConnectionSettings, ContainerHealth, ContainerState, ServiceState, SshAuthKind,
        SshSettings, SystemInfo,
    };

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn sample_server() -> Server {
        Server::new(
            "prod-01",
            "10.0.0.5",
            ConnectionSettings::Ssh(SshSettings {
                username: "root".into(),
                auth_kind: SshAuthKind::PrivateKey,
                credential_ref: CredentialRef::new(),
            }),
            at(0),
        )
    }

    fn healthy_state(id: ServerId) -> ServerRuntimeState {
        let mut state = ServerRuntimeState::unknown(id);
        state.status = Status::Healthy;
        state.cpu_percent = MetricValue::Available(32.0);
        state.memory_percent = MetricValue::Available(61.0);
        state.disk_percent = MetricValue::Available(72.0);
        state.uptime_secs = Some(143 * 86_400);
        state.last_check = Some(at(9_000));
        state
    }

    #[test]
    fn a_server_row_matches_the_layout_from_the_brief() {
        // "Production-01 · Online · CPU 32% · RAM 61% · Disk 72% · Uptime 143 days"
        let server = sample_server();
        let row = server_row(&server, &healthy_state(server.id), at(9_060));

        assert_eq!(row.name, "prod-01");
        assert_eq!(row.host, "10.0.0.5:22");
        assert_eq!(row.status_label, "Online");
        assert_eq!(row.cpu, "32%");
        assert_eq!(row.ram, "61%");
        assert_eq!(row.disk, "72%");
        assert_eq!(row.uptime, "143d 0h");
        assert_eq!(row.last_check, "1m ago");
    }

    #[test]
    fn an_unmeasured_metric_gets_an_empty_track_not_a_zero_bar() {
        // The visual equivalent of the NotAvailable rule: -1 draws no bar at all.
        assert_eq!(fraction(MetricValue::NotAvailable), -1.0);
        assert_eq!(fraction(MetricValue::Available(0.0)), 0.0);
        assert_eq!(fraction(MetricValue::Available(50.0)), 0.5);
    }

    #[test]
    fn a_meter_fraction_is_clamped_to_the_bar() {
        // A CPU reading of 101% across a sampling boundary must not overflow the track.
        assert_eq!(fraction(MetricValue::Available(150.0)), 1.0);
        assert_eq!(fraction(MetricValue::Available(-5.0)), 0.0);
    }

    #[test]
    fn an_offline_server_shows_dashes_rather_than_stale_numbers() {
        let server = sample_server();
        let mut state = ServerRuntimeState::unknown(server.id);
        state.status = Status::Offline;

        let row = server_row(&server, &state, at(9_060));
        assert_eq!(row.status_label, "Offline");
        assert_eq!(row.cpu, format::UNAVAILABLE);
        assert_eq!(row.uptime, format::UNAVAILABLE);
        assert_eq!(row.cpu_fraction, -1.0);
    }

    #[test]
    fn a_never_checked_server_says_never() {
        let server = sample_server();
        let state = ServerRuntimeState::unknown(server.id);
        assert_eq!(server_row(&server, &state, at(0)).last_check, "never");
    }

    #[test]
    fn only_a_breaching_value_is_tinted() {
        // Colouring every number would make colour meaningless.
        assert_eq!(
            threshold_status(MetricValue::Available(12.0), 80.0, 95.0),
            ""
        );
        assert_eq!(
            threshold_status(MetricValue::Available(85.0), 80.0, 95.0),
            "warning"
        );
        assert_eq!(
            threshold_status(MetricValue::Available(97.0), 80.0, 95.0),
            "critical"
        );
        assert_eq!(threshold_status(MetricValue::NotAvailable, 80.0, 95.0), "");
    }

    #[test]
    fn an_expired_certificate_is_tinted_critical() {
        assert_eq!(ssl_status(Some(-1)), "critical");
        assert_eq!(ssl_status(Some(2)), "critical");
        assert_eq!(ssl_status(Some(10)), "warning");
        assert_eq!(ssl_status(Some(60)), "");
        assert_eq!(ssl_status(None), "");
    }

    #[test]
    fn a_host_without_docker_hides_the_panel_but_an_empty_docker_shows_it() {
        // The distinction the whole `Option<Vec<_>>` shape exists to preserve.
        let server = sample_server();
        let state = healthy_state(server.id);

        let mut without = ServerSnapshot::new(server.id, at(0));
        without.containers = None;
        assert!(!server_detail(&server, &state, Some(&without), at(0)).has_docker);

        let mut empty = ServerSnapshot::new(server.id, at(0));
        empty.containers = Some(Vec::new());
        assert!(server_detail(&server, &state, Some(&empty), at(0)).has_docker);
    }

    #[test]
    fn system_facts_that_were_not_collected_show_a_dash() {
        let server = sample_server();
        let state = healthy_state(server.id);
        let snapshot = ServerSnapshot::new(server.id, at(0));

        let detail = server_detail(&server, &state, Some(&snapshot), at(0));
        assert_eq!(detail.os, format::UNAVAILABLE);
        assert_eq!(detail.kernel, format::UNAVAILABLE);
        assert_eq!(detail.cores, format::UNAVAILABLE);
    }

    #[test]
    fn system_facts_are_shown_when_they_were_collected() {
        let server = sample_server();
        let state = healthy_state(server.id);
        let mut snapshot = ServerSnapshot::new(server.id, at(0));
        snapshot.system = SystemInfo {
            hostname: Some("prod-01".into()),
            os_name: Some("Ubuntu 22.04.3 LTS".into()),
            kernel: Some("5.15.0-91-generic".into()),
            architecture: Some("x86_64".into()),
            cpu_model: Some("Xeon E5-2680".into()),
            cpu_cores: Some(8),
            ..Default::default()
        };

        let detail = server_detail(&server, &state, Some(&snapshot), at(0));
        assert_eq!(detail.os, "Ubuntu 22.04.3 LTS");
        assert_eq!(detail.cores, "8");
        assert_eq!(detail.cpu_model, "Xeon E5-2680");
    }

    #[test]
    fn a_cached_preview_always_carries_its_age() {
        // The rule from the brief, enforced where the string is built.
        let preview = ScreenshotPresentation::Cached {
            screenshot: vds_domain::screenshot::Screenshot {
                website_id: WebsiteId::new(),
                provider: vds_domain::ids::ProviderId::new("chromium_cli"),
                path: "a.png".into(),
                thumbnail_path: None,
                captured_at: at(0),
                status: vds_domain::screenshot::ScreenshotStatus::Captured,
                hash: "x".into(),
                width: 1,
                height: 1,
            },
            age: chrono::Duration::hours(4),
        };

        let (message, age) = preview_text(&preview);
        assert!(
            message.is_empty(),
            "a cached image needs no placeholder text"
        );
        assert_eq!(age, "Captured 4 hours ago");
    }

    #[test]
    fn an_offline_website_says_so_instead_of_showing_an_old_image() {
        let (message, age) = preview_text(&ScreenshotPresentation::WebsiteOffline);
        assert!(message.contains("offline"), "message was {message}");
        assert!(age.is_empty());
    }

    #[test]
    fn a_failed_capture_names_the_reason_so_a_retry_makes_sense() {
        let (message, _) = preview_text(&ScreenshotPresentation::Failed {
            reason: "navigation timed out".into(),
        });
        assert!(
            message.contains("navigation timed out"),
            "message was {message}"
        );
    }

    #[test]
    fn a_website_without_analytics_omits_the_visitor_line_entirely() {
        // A dash there would imply a provider is connected but returning nothing.
        let website = Website::new("Example", "https://example.com/", at(0));
        let state = WebsiteRuntimeState::unknown(website.id);

        let card = website_card(
            &website,
            &state,
            &ScreenshotPresentation::NotCaptured,
            None,
            MetricValue::NotAvailable,
            None,
        );
        assert_eq!(card.visitors, "");
        assert_eq!(card.thumbnail_file, None);
    }

    #[test]
    fn a_website_with_analytics_shows_its_visitor_count() {
        let website = Website::new("Example", "https://example.com/", at(0));
        let mut state = WebsiteRuntimeState::unknown(website.id);
        state.status = Status::Healthy;
        state.response_ms = Some(142);
        state.ssl_days_remaining = Some(48);

        let card = website_card(
            &website,
            &state,
            &ScreenshotPresentation::NotCaptured,
            None,
            MetricValue::Available(12_452.0),
            Some(99.98),
        );

        assert_eq!(card.visitors, "Visitors today: 12 452");
        assert_eq!(card.response, "Response: 142 ms");
        assert_eq!(card.ssl, "SSL: 48 days");
        assert_eq!(card.uptime, "Uptime 24h: 99.98%");
    }

    #[test]
    fn an_analytics_panel_is_hidden_when_no_provider_is_connected() {
        let website = Website::new("Example", "https://example.com/", at(0));
        let state = WebsiteRuntimeState::unknown(website.id);

        let detail = website_detail(
            &website,
            &state,
            &ScreenshotPresentation::NotCaptured,
            None,
            None,
            None,
            None,
            false,
            None,
            MetricValue::NotAvailable,
        );
        assert!(!detail.has_analytics);
        assert_eq!(detail.analytics_updated, "");
        // And the visitors tile is absent rather than showing a dash.
        assert_eq!(detail.cards.len(), 4);
    }

    #[test]
    fn an_incident_row_reports_how_long_it_has_been_open() {
        let rule = AlertRule::new(
            "Server offline",
            vds_domain::alerts::AlertCondition::ServerOffline,
            Status::Critical,
            at(0),
        );
        let incident = Incident::open(
            &rule,
            vds_domain::events::AlertSubject::Server(ServerId::new()),
            "prod-01 is unreachable",
            at(1_000),
        );

        let row = incident_row(&incident, "prod-01", at(1_000 + 3_600 * 5));
        assert_eq!(row.title, "prod-01 is unreachable");
        assert_eq!(row.detail, "Open for 5h 0m");
        assert!(!row.resolved);
        assert!(!row.acknowledged);
    }

    #[test]
    fn a_disabled_rule_is_marked_so_the_checkbox_reads_correctly() {
        let mut rule = AlertRule::new(
            "CPU high",
            vds_domain::alerts::AlertCondition::ServerOffline,
            Status::Warning,
            at(0),
        );
        assert!(
            !rule_row(&rule).resolved,
            "an enabled rule must show as checked"
        );

        rule.enabled = false;
        assert!(rule_row(&rule).resolved);
    }

    #[test]
    fn every_event_kind_produces_a_readable_line() {
        use vds_domain::events::DomainEvent as E;
        let server = ServerId::new();
        let website = WebsiteId::new();

        let events = vec![
            E::ServerStatusChanged {
                server_id: server,
                from: Status::Healthy,
                to: Status::Offline,
                reason: None,
            },
            E::MetricThresholdExceeded {
                server_id: server,
                metric: MetricKind::CpuUsage,
                value: 97.0,
                threshold: 90.0,
                status: Status::Critical,
            },
            E::SslExpiringSoon {
                website_id: website,
                days_remaining: 7,
            },
            E::TrafficAnomalyDetected {
                website_id: website,
                metric: AnalyticsMetric::Visitors,
                current: 6_500.0,
                baseline: 10_000.0,
                change_percent: -35.0,
            },
            E::ScreenshotFailed {
                website_id: website,
                error: "no browser".into(),
            },
            E::IncidentResolved {
                incident_id: IncidentId::new(),
                rule_id: vds_domain::ids::AlertRuleId::new(),
                subject: vds_domain::events::AlertSubject::Server(server),
            },
        ];

        for event in &events {
            let line = describe_event(event);
            assert!(!line.is_empty(), "{} produced no text", event.kind());
            assert!(
                !line.contains("None"),
                "{} leaked a Debug value: {line}",
                event.kind()
            );
        }

        assert_eq!(
            describe_event(&events[0]),
            "Server went from Online to Offline"
        );
        assert!(describe_event(&events[3]).contains("-35.0%"));
    }

    #[test]
    fn a_container_row_shows_docker_status_text_verbatim() {
        let container = ContainerInfo {
            id: "abc".into(),
            name: "web".into(),
            image: "nginx:1.25".into(),
            state: ContainerState::Running,
            health: ContainerHealth::Unhealthy,
            status_text: "Up 3 days (unhealthy)".into(),
            cpu_percent: MetricValue::Available(1.5),
            memory_used_bytes: Some(104_857_600),
            memory_limit_bytes: None,
            restart_count: Some(4),
            started_at: None,
        };

        let row = container_row(&container);
        assert_eq!(
            row.status, "critical",
            "an unhealthy container must read as critical"
        );
        assert_eq!(row.status_text, "Up 3 days (unhealthy)");
        assert_eq!(row.memory, "100 MiB");
        assert_eq!(row.restarts, "4 restarts");
    }

    #[test]
    fn a_failed_service_maps_to_a_critical_row() {
        let service = ServiceInfo {
            name: "redis.service".into(),
            state: ServiceState::Failed,
            sub_state: Some("failed".into()),
            description: Some("Advanced key-value store".into()),
            enabled: None,
        };

        let row = service_row(&service);
        assert_eq!(row.state, "critical");
        assert_eq!(row.state_label, "FAILED");
    }

    #[test]
    fn a_process_without_rss_falls_back_to_a_percentage() {
        let process = ProcessInfo {
            pid: 842,
            user: Some("www-data".into()),
            command: "nginx: worker".into(),
            cpu_percent: 12.5,
            memory_percent: 3.4,
            rss_bytes: None,
        };

        let row = process_row(&process);
        assert_eq!(row.cpu, "12.5%");
        assert_eq!(row.memory, "3.4%");
    }

    #[test]
    fn the_dashboard_is_empty_only_when_nothing_is_monitored() {
        assert!(is_dashboard_empty(0, 0));
        assert!(!is_dashboard_empty(1, 0));
        assert!(!is_dashboard_empty(0, 1));
    }

    #[test]
    fn a_website_with_analytics_gains_a_visitors_tile() {
        let website = Website::new("Example", "https://example.com/", at(0));
        let state = WebsiteRuntimeState::unknown(website.id);

        let detail = website_detail(
            &website,
            &state,
            &ScreenshotPresentation::NotCaptured,
            None,
            None,
            None,
            None,
            true,
            Some("8m ago".to_owned()),
            MetricValue::Available(24_821.0),
        );
        assert!(detail.has_analytics);
        assert_eq!(detail.cards.len(), 5);
        assert_eq!(detail.analytics_updated, "Analytics updated 8m ago");
    }

    #[test]
    fn a_chart_title_names_the_metric_and_the_range() {
        assert_eq!(
            chart_title(MetricKind::CpuUsage, "24 hours"),
            "CPU — 24 hours"
        );
        assert_eq!(
            analytics_chart_title(AnalyticsMetric::Visitors, "30 days"),
            "Visitors — 30 days"
        );
    }
}
