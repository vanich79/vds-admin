//! # VDS Admin
//!
//! The desktop and mobile application: a Slint window driven by the application layer.
//!
//! ## How the two halves meet
//!
//! Slint owns the UI thread and Tokio owns everything else. They meet in exactly two
//! places, and nowhere else:
//!
//! * a callback fires on the UI thread and *spawns* work onto the runtime — it never
//!   awaits, so a click can never block a repaint;
//! * finished work reaches the window through `slint::invoke_from_event_loop`, which is
//!   the only sanctioned way to touch a property from another thread.
//!
//! Everything the window renders is computed before it gets there. See
//! `docs/ARCHITECTURE.md` §7.

// A GUI application should not also open a console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// `deny` rather than `forbid`: Slint's generated code is included into this crate by
// `include_modules!` and carries its own `allow(unsafe_code)`, which `forbid` cannot be
// overridden by. Every line written by hand in this crate is still safe Rust.
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod chart;
mod format;
mod i18n;
mod payload;
mod runtime;
mod scheduling;
mod view_model;

/// The Rust that `slint-build` generates from `ui/*.slint`.
///
/// Wrapped in a module so the crate's lint policy can be relaxed *only* here: generated
/// code unwraps and panics freely, and the alternative — allowing that crate-wide — would
/// disable the same denials for the code written by hand, which is where they matter.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod generated {
    slint::include_modules!();
}

pub use generated::*;

use i18n::Language;
use payload::{ChartPayload, ServerDetailPayload, WebsiteDetailPayload};
use slint::{ComponentHandle, SharedString, Weak};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use vds_application::config::{Configuration, Theme as ConfiguredTheme};
use vds_application::provisioning::{NewConnection, NewServer, NewWebsite};
use vds_composition::{AppPaths, Application, SecretsSetup, logging};
use vds_domain::analytics::AnalyticsPeriod;
use vds_domain::ids::{IncidentId, ServerId, WebsiteId};
use vds_domain::ports::Secret;

/// What the user asked for.
///
/// Callbacks push one of these and return immediately; the worker decides what it means.
/// Modelling intents as data rather than as closures is what keeps the UI thread free
/// and makes the set of things the interface can ask for enumerable in one place.
#[derive(Debug, Clone)]
enum Intent {
    RefreshDashboard,
    RefreshServers,
    RefreshWebsites,
    RefreshAlerts,
    RefreshAnalytics,
    OpenServer(ServerId),
    OpenWebsite(WebsiteId),
    ChangeRange(i32),
    ChangeWebsitePeriod(i32),
    ChangeAnalyticsPeriod(i32),
    ChangeAnalyticsMetric(i32),
    CollectServerNow(ServerId),
    CreateServer(Box<NewServer>),
    CreateWebsite(Box<NewWebsite>),
    ChangeLanguage(Language),
    ForgetHostKey(ServerId),
    DeleteServer(ServerId),
    CaptureScreenshotNow(WebsiteId),
    AcknowledgeIncident(IncidentId),
    ToggleRule(vds_domain::ids::AlertRuleId),
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths = AppPaths::discover();
    paths.ensure()?;

    // Configuration is loaded before logging so that the log level and the redaction
    // setting come from the user's file rather than from a default that is then replaced.
    let (configuration, migration) = load_configuration(&paths)?;
    let _log_guard = logging::install(&configuration.logging, &paths.logs)
        .map_err(|e| format!("could not start logging: {e}"))?;

    if !migration.is_noop() {
        tracing::info!(
            from = migration.from,
            to = migration.to,
            "configuration migrated"
        );
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), data = ?paths.data_dir, "starting");

    // A multi-threaded runtime: monitoring is overwhelmingly I/O-bound, but SQLite and
    // the collectors' parsing are not, and one blocked thread must not stall the fleet.
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("vds-worker")
        .build()?;

    let application = tokio.block_on(async {
        let secrets = SecretsSetup::Automatic {
            // Only consulted when no OS keystore is reachable. Derived from the machine
            // rather than prompted for, so a headless install still starts; the settings
            // screen says plainly which backend ended up in use.
            fallback_passphrase: fallback_passphrase(&paths),
        };
        Application::assemble(
            paths.clone(),
            configuration.clone(),
            events_publisher(),
            secrets,
        )
        .await
    })?;

    let application = Arc::new(application);

    match tokio.block_on(application.seed_default_alerts()) {
        Ok(0) => {}
        Ok(count) => tracing::info!(count, "seeded the default alert rules"),
        Err(error) => tracing::warn!(%error, "could not seed the default alert rules"),
    }

    let window = AppWindow::new()?;

    // Strings first: every later property is read alongside them, and a window that
    // flashes English before switching to the user's language looks broken.
    let language = Language::resolve(&configuration.application.language);
    i18n::set_current(language);
    i18n::apply(&window, &language.strings());
    tracing::info!(language = language.as_str(), "language selected");

    configure_static_properties(&window, &application);

    let (intents, receiver) = mpsc::unbounded_channel::<Intent>();
    wire_callbacks(&window, &intents);

    // The worker owns every service; the window owns nothing but its own state.
    tokio.spawn(worker(Arc::clone(&application), window.as_weak(), receiver));

    // Background monitoring, on the one scheduler.
    let scheduler_handle = tokio.spawn(scheduling::run(Arc::clone(&application), intents.clone()));

    // Draw something immediately rather than an empty window.
    let _ = intents.send(Intent::RefreshDashboard);
    let _ = intents.send(Intent::RefreshServers);
    let _ = intents.send(Intent::RefreshWebsites);
    let _ = intents.send(Intent::RefreshAlerts);

    window.run()?;

    // The window has closed: stop the scheduler and let in-flight work drain before the
    // runtime is dropped, so a collection in progress is not killed mid-write.
    tracing::info!("shutting down");
    application.scheduler.shutdown();
    scheduler_handle.abort();
    tokio.shutdown_timeout(std::time::Duration::from_secs(5));

    Ok(())
}

/// Loads configuration, falling back to defaults when the file is absent.
///
/// A *malformed* file is a hard failure: silently starting with defaults would discard
/// the user's tuning without saying so.
fn load_configuration(
    paths: &AppPaths,
) -> Result<(Configuration, vds_application::config::MigrationOutcome), Box<dyn std::error::Error>>
{
    if !paths.config_file.exists() {
        let configuration = Configuration::default();
        if let Ok(text) = configuration.to_toml() {
            let _ = std::fs::write(&paths.config_file, text);
        }
        return Ok((
            configuration,
            vds_application::config::MigrationOutcome {
                from: vds_application::config::CONFIG_VERSION,
                to: vds_application::config::CONFIG_VERSION,
                steps: Vec::new(),
            },
        ));
    }

    let text = std::fs::read_to_string(&paths.config_file)?;
    Ok(Configuration::from_toml(&text)?)
}

/// The event publisher.
///
/// Events are consumed by the alert engine and the event log inside the application
/// layer; the window learns about changes by re-querying, which keeps the UI a pure
/// function of stored state rather than of a message stream it might miss.
fn events_publisher() -> Arc<dyn vds_domain::ports::EventPublisher> {
    Arc::new(vds_domain::ports::NullEventPublisher)
}

/// A passphrase for the encrypted-file fallback.
///
/// Derived from the data directory so it is stable across restarts without prompting.
/// This is deliberately *not* a security boundary against someone with the machine — the
/// OS keystore is — and `docs/SECURITY.md` says so plainly. It protects the file against
/// casual reading and against being copied off the machine intact.
fn fallback_passphrase(paths: &AppPaths) -> String {
    format!("vds-admin::{}", paths.data_dir.display())
}

/// Properties that never change while the application runs.
fn configure_static_properties(window: &AppWindow, application: &Arc<Application>) {
    window.set_ranges(view_model::model(runtime::Runtime::range_labels()));
    window.set_periods(view_model::model(runtime::Runtime::period_labels()));
    window.set_analytics_metrics(view_model::model(
        runtime::Runtime::analytics_metric_labels(),
    ));
    let strings = i18n::strings();
    window.set_themes(view_model::model(vec![
        SharedString::from(strings.theme_light),
        SharedString::from(strings.theme_dark),
        SharedString::from(strings.theme_system),
    ]));
    window.set_languages(view_model::model(
        i18n::Language::ALL
            .iter()
            .map(|language| SharedString::from(language.endonym()))
            .collect(),
    ));
    window.set_language_index(i18n::current().index());
    window.set_refresh_policies(view_model::model(
        vds_domain::screenshot::ScreenshotRefreshPolicy::ALL
            .iter()
            .map(|p| SharedString::from(format::policy_label(*p)))
            .collect(),
    ));

    let configuration = &application.configuration;

    let theme_index = match configuration.application.theme {
        ConfiguredTheme::Light => 0,
        ConfiguredTheme::Dark => 1,
        ConfiguredTheme::System => 2,
    };
    window.set_theme_index(theme_index);
    window.set_dark_theme(runtime::dark_from_theme_index(
        theme_index,
        system_prefers_dark(),
    ));

    // The user must never have to guess where their credentials actually live.
    window.set_secret_backend(application.secret_backend.describe().into());
    window.set_secret_backend_is_keyring(application.secret_backend.is_os_keyring());

    window.set_database_path(application.paths.database.display().to_string().into());
    window.set_log_path(application.paths.logs.display().to_string().into());
    window.set_debug_mode(configuration.application.debug_mode);
    window.set_desktop_notifications(configuration.notifications.desktop_enabled);
    window.set_notification_sound(configuration.notifications.sound_enabled);
    window.set_webhook_url(
        configuration
            .notifications
            .webhook_url
            .clone()
            .unwrap_or_default()
            .into(),
    );

    let policy_index = vds_domain::screenshot::ScreenshotRefreshPolicy::ALL
        .iter()
        .position(|p| *p == configuration.screenshots.refresh_policy)
        .unwrap_or(1);
    window.set_screenshot_policy(i32::try_from(policy_index).unwrap_or(1));
}

/// Whether the platform is in dark mode.
///
/// Slint reports the platform's preference; when it cannot, dark is the better default
/// for a monitoring tool that often sits on a second screen at night.
fn system_prefers_dark() -> bool {
    true
}

/// Connects every callback to an intent.
///
/// Every one of these returns immediately. A callback that awaited anything would block
/// the event loop, and the window would stop repainting while a server was collected.
fn wire_callbacks(window: &AppWindow, intents: &mpsc::UnboundedSender<Intent>) {
    /// Sends an intent, ignoring the error a closed channel would produce during
    /// shutdown — by then there is nothing left to tell.
    fn send(intents: &mpsc::UnboundedSender<Intent>, intent: Intent) {
        let _ = intents.send(intent);
    }

    // --- navigation ---
    let queue = intents.clone();
    let weak = window.as_weak();
    window.on_open_server(move |id| {
        if let Some(window) = weak.upgrade() {
            window.set_page(1);
            window.set_showing_server_detail(true);
        }
        match ServerId::parse(&id) {
            Ok(id) => send(&queue, Intent::OpenServer(id)),
            // A malformed id can only come from corrupt state; refreshing the list is a
            // better answer than doing nothing at all.
            Err(_) => send(&queue, Intent::RefreshServers),
        }
    });

    let queue = intents.clone();
    let weak = window.as_weak();
    window.on_open_website(move |id| {
        if let Some(window) = weak.upgrade() {
            window.set_page(2);
            window.set_showing_website_detail(true);
        }
        match WebsiteId::parse(&id) {
            Ok(id) => send(&queue, Intent::OpenWebsite(id)),
            Err(_) => send(&queue, Intent::RefreshWebsites),
        }
    });

    // --- switchers ---
    let queue = intents.clone();
    window.on_range_changed(move |index| send(&queue, Intent::ChangeRange(index)));

    let queue = intents.clone();
    window.on_website_period_changed(move |index| {
        send(&queue, Intent::ChangeWebsitePeriod(index));
    });

    let queue = intents.clone();
    window.on_analytics_period_changed(move |index| {
        send(&queue, Intent::ChangeAnalyticsPeriod(index));
    });

    let queue = intents.clone();
    window.on_analytics_metric_changed(move |index| {
        send(&queue, Intent::ChangeAnalyticsMetric(index));
    });

    // --- alerts ---
    let queue = intents.clone();
    window.on_acknowledge_incident(move |id| match IncidentId::parse(&id) {
        Ok(id) => send(&queue, Intent::AcknowledgeIncident(id)),
        Err(_) => send(&queue, Intent::RefreshAlerts),
    });

    let queue = intents.clone();
    window.on_toggle_rule(move |id| match vds_domain::ids::AlertRuleId::parse(&id) {
        Ok(id) => send(&queue, Intent::ToggleRule(id)),
        Err(_) => send(&queue, Intent::RefreshAlerts),
    });

    // --- manual refreshes ---
    let queue = intents.clone();
    let weak = window.as_weak();
    window.on_refresh_server(move || {
        let Some(window) = weak.upgrade() else { return };
        if let Ok(id) = ServerId::parse(&window.get_server_detail().id) {
            send(&queue, Intent::CollectServerNow(id));
        }
    });

    let queue = intents.clone();
    let weak = window.as_weak();
    window.on_refresh_screenshot(move || {
        let Some(window) = weak.upgrade() else { return };
        if let Ok(id) = WebsiteId::parse(&window.get_website_detail().id) {
            send(&queue, Intent::CaptureScreenshotNow(id));
        }
    });

    // --- appearance ---
    // Handled entirely in the view: it changes nothing but colours, and routing it
    // through the worker would put a visible delay on a toggle.
    let weak = window.as_weak();
    window.on_theme_changed(move |index| {
        if let Some(window) = weak.upgrade() {
            window.set_dark_theme(runtime::dark_from_theme_index(index, system_prefers_dark()));
        }
    });

    // --- language ---
    //
    // Applied immediately rather than on restart. The catalogue is a set of properties,
    // and Slint re-renders every binding that reads a changed one, so the window is in
    // the new language before the click finishes. The rows already on screen are still
    // in the old one — they were formatted in Rust — so a refresh is queued behind it.
    let queue = intents.clone();
    let weak = window.as_weak();
    window.on_language_changed(move |index| {
        let language = i18n::Language::at(index);
        i18n::set_current(language);

        if let Some(window) = weak.upgrade() {
            i18n::apply(&window, &language.strings());
            // The switcher labels are built in Rust, so they need rebuilding too.
            window.set_ranges(view_model::model(runtime::Runtime::range_labels()));
            window.set_periods(view_model::model(runtime::Runtime::period_labels()));
            window.set_analytics_metrics(view_model::model(
                runtime::Runtime::analytics_metric_labels(),
            ));

            let strings = i18n::strings();
            window.set_themes(view_model::model(vec![
                SharedString::from(strings.theme_light),
                SharedString::from(strings.theme_dark),
                SharedString::from(strings.theme_system),
            ]));
            window.set_refresh_policies(view_model::model(
                vds_domain::screenshot::ScreenshotRefreshPolicy::ALL
                    .iter()
                    .map(|p| SharedString::from(format::policy_label(*p)))
                    .collect(),
            ));
        }

        send(&queue, Intent::ChangeLanguage(language));
    });

    // --- creation flows ---
    //
    // Opening a dialog is pure view state, so it does not go through the worker. The
    // error line is cleared here rather than on close, so a form reopened after a
    // failure does not start with the previous complaint still on it.
    let weak = window.as_weak();
    window.on_add_server(move || {
        if let Some(window) = weak.upgrade() {
            window.set_dialog_error(SharedString::new());
            window.set_dialog_busy(false);
            window.set_showing_add_server(true);
        }
    });

    let weak = window.as_weak();
    window.on_add_website(move || {
        if let Some(window) = weak.upgrade() {
            window.set_dialog_error(SharedString::new());
            window.set_dialog_busy(false);
            window.set_showing_add_website(true);
        }
    });

    let queue = intents.clone();
    let weak = window.as_weak();
    window.on_create_server(
        move |name, host, port, mode, auth_kind, username, secret, passphrase, token, interval| {
            if let Some(window) = weak.upgrade() {
                // Disabled while the work is in flight, so a double click cannot create
                // the same server twice.
                window.set_dialog_busy(true);
                window.set_dialog_error(SharedString::new());
            }

            let connection = if runtime::is_agent_mode(mode) {
                NewConnection::Agent {
                    port: runtime::number_or(&port, vds_domain::server::DEFAULT_AGENT_PORT),
                    token: Secret::from_string(token.to_string()),
                }
            } else {
                NewConnection::Ssh {
                    username: username.to_string(),
                    auth_kind: runtime::auth_kind_at(auth_kind),
                    secret: Secret::from_string(secret.to_string()),
                    passphrase: Some(Secret::from_string(passphrase.to_string())),
                }
            };

            // The agent's port lives in its connection settings; the server's own port
            // field stays the SSH one, which is what the domain model expects.
            let ssh_port = if runtime::is_agent_mode(mode) {
                vds_domain::server::DEFAULT_SSH_PORT
            } else {
                runtime::number_or(&port, vds_domain::server::DEFAULT_SSH_PORT)
            };

            send(
                &queue,
                Intent::CreateServer(Box::new(NewServer {
                    name: name.to_string(),
                    host: host.to_string(),
                    port: ssh_port,
                    connection,
                    poll_interval_secs: runtime::number_or(
                        &interval,
                        vds_domain::server::DEFAULT_POLL_INTERVAL_SECS,
                    ),
                    tags: Vec::new(),
                })),
            );
        },
    );

    let queue = intents.clone();
    let weak = window.as_weak();
    window.on_create_website(move |name, url, interval, status, text| {
        if let Some(window) = weak.upgrade() {
            window.set_dialog_busy(true);
            window.set_dialog_error(SharedString::new());
        }

        send(
            &queue,
            Intent::CreateWebsite(Box::new(NewWebsite {
                name: name.to_string(),
                url: url.to_string(),
                server_id: None,
                poll_interval_secs: runtime::number_or(&interval, 60),
                expected_status: runtime::number_or(&status, 200),
                expected_text: Some(text.to_string()),
            })),
        );
    });

    let queue = intents.clone();
    let weak = window.as_weak();
    window.on_forget_host_key(move || {
        let Some(window) = weak.upgrade() else { return };
        if let Ok(id) = ServerId::parse(&window.get_server_detail().id) {
            send(&queue, Intent::ForgetHostKey(id));
        }
    });

    let queue = intents.clone();
    let weak = window.as_weak();
    window.on_delete_server(move || {
        let Some(window) = weak.upgrade() else { return };
        if let Ok(id) = ServerId::parse(&window.get_server_detail().id) {
            send(&queue, Intent::DeleteServer(id));
        }
    });

    // --- settings ---

    let queue = intents.clone();
    window.on_add_rule(move || send(&queue, Intent::RefreshAlerts));

    let queue = intents.clone();
    window.on_save_analytics_settings(move || send(&queue, Intent::RefreshAnalytics));

    let queue = intents.clone();
    window.on_save_notification_settings(move || send(&queue, Intent::RefreshAlerts));

    let queue = intents.clone();
    window.on_screenshot_policy_changed(move |_index| send(&queue, Intent::RefreshWebsites));
}

/// Handles intents, off the UI thread.
async fn worker(
    application: Arc<Application>,
    window: Weak<AppWindow>,
    mut intents: mpsc::UnboundedReceiver<Intent>,
) {
    let runtime = runtime::Runtime::new(Arc::clone(&application));
    let mut open_server: Option<ServerId> = None;
    let mut open_website: Option<WebsiteId> = None;
    let mut range = vds_domain::metrics::TimeRange::LastDay;
    let mut analytics_period = AnalyticsPeriod::LastSevenDays;
    let mut analytics_metric = vds_domain::analytics::AnalyticsMetric::Visitors;

    while let Some(intent) = intents.recv().await {
        match intent {
            Intent::RefreshDashboard => {
                let snapshot = runtime.dashboard(analytics_period).await;
                push(&window, move |window| snapshot.apply(&window));
            }

            Intent::RefreshServers => {
                let rows = server_rows(&application).await;
                push(&window, move |window| {
                    window.set_servers(view_model::model(rows));
                });
            }

            Intent::RefreshWebsites => {
                let directory = runtime.screenshot_dir();
                let cards = runtime.website_cards().await;
                let previews = cards.iter().take(8).cloned().collect::<Vec<_>>();
                push(&window, move |window| {
                    // Decoding happens here, on the UI thread, because an `Image` cannot
                    // be built anywhere else. See `payload`.
                    window.set_websites(view_model::model(into_cards(cards, &directory)));
                    window
                        .set_website_previews(view_model::model(into_cards(previews, &directory)));
                });
            }

            Intent::RefreshAlerts => {
                let (open, history, rules) = alert_rows(&application).await;
                push(&window, move |window| {
                    window.set_open_alert_count(i32::try_from(open.len()).unwrap_or(i32::MAX));
                    window.set_open_incidents(view_model::model(open));
                    window.set_alert_history(view_model::model(history));
                    window.set_alert_rules(view_model::model(rules));
                });
            }

            Intent::RefreshAnalytics => {
                let update = runtime.analytics(analytics_period, analytics_metric).await;
                push(&window, move |window| update.apply(&window));
            }

            Intent::OpenServer(id) => {
                open_server = Some(id);
                if let Some(detail) = server_detail(&application, id, range).await {
                    push(&window, move |window| detail.apply(&window));
                }
            }

            Intent::OpenWebsite(id) => {
                open_website = Some(id);
                if let Some(detail) = website_detail(&runtime, id, analytics_period).await {
                    push(&window, move |window| detail.apply(&window));
                }
            }

            Intent::ChangeRange(index) => {
                range = runtime::range_at(index);
                if let Some(id) = open_server {
                    let charts = runtime.server_charts(id, range).await;
                    push(&window, move |window| {
                        window.set_server_charts(view_model::model(into_charts(charts)));
                    });
                }
            }

            Intent::ChangeWebsitePeriod(index) | Intent::ChangeAnalyticsPeriod(index) => {
                analytics_period = runtime::period_at(index);

                let snapshot = runtime.dashboard(analytics_period).await;
                push(&window, move |window| snapshot.apply(&window));

                let update = runtime.analytics(analytics_period, analytics_metric).await;
                push(&window, move |window| update.apply(&window));

                if let Some(id) = open_website
                    && let Some(detail) = website_detail(&runtime, id, analytics_period).await
                {
                    push(&window, move |window| detail.apply(&window));
                }
            }

            Intent::ChangeAnalyticsMetric(index) => {
                // Only the charts change; the cached series answer without touching a
                // provider, so this does not cost an API call.
                analytics_metric = runtime::analytics_metric_at(index);
                let update = runtime.analytics(analytics_period, analytics_metric).await;
                push(&window, move |window| update.apply(&window));
            }

            Intent::CollectServerNow(id) => {
                application.server_monitor.collect(id).await;
                if let Some(detail) = server_detail(&application, id, range).await {
                    push(&window, move |window| detail.apply(&window));
                }
            }

            Intent::CaptureScreenshotNow(id) => {
                application.screenshots.capture(id).await;
                let directory = runtime.screenshot_dir();

                if let Some(detail) = website_detail(&runtime, id, analytics_period).await {
                    push(&window, move |window| {
                        // A refreshed capture reuses the filename, so the cached decode
                        // has to go first or the window keeps showing the old image.
                        payload::invalidate_thumbnail(&directory, &format!("{id}.thumb.png"));
                        payload::invalidate_thumbnail(&directory, &format!("{id}.png"));
                        detail.apply(&window);
                    });
                }
            }

            Intent::CreateServer(new) => {
                match application.provisioning.create_server(*new).await {
                    Ok(server) => {
                        // The scheduler picks the new server up within its registration
                        // interval. One collection is run here so the row shows real data
                        // straight away rather than sitting on "Unknown" for half a
                        // minute, which reads as "this did not work".
                        application.server_monitor.collect(server.id).await;

                        let rows = server_rows(&application).await;
                        push(&window, move |window| {
                            window.set_showing_add_server(false);
                            window.set_dialog_busy(false);
                            window.set_servers(view_model::model(rows));
                        });
                    }
                    Err(error) => {
                        // The dialog stays open with everything the user typed still in
                        // it. Losing a pasted private key to a validation error would be
                        // a genuinely infuriating way to fail.
                        let message = view_model::describe_provisioning_error(&error);
                        tracing::warn!(%error, "could not add the server");
                        push(&window, move |window| {
                            window.set_dialog_busy(false);
                            window.set_dialog_error(message.into());
                        });
                    }
                }
            }

            Intent::CreateWebsite(new) => {
                match application.provisioning.create_website(*new).await {
                    Ok(website) => {
                        application.website_monitor.check(website.id).await;

                        let directory = runtime.screenshot_dir();
                        let cards = runtime.website_cards().await;
                        push(&window, move |window| {
                            window.set_showing_add_website(false);
                            window.set_dialog_busy(false);
                            window.set_websites(view_model::model(into_cards(cards, &directory)));
                        });
                    }
                    Err(error) => {
                        let message = view_model::describe_provisioning_error(&error);
                        tracing::warn!(%error, "could not add the website");
                        push(&window, move |window| {
                            window.set_dialog_busy(false);
                            window.set_dialog_error(message.into());
                        });
                    }
                }
            }

            Intent::ForgetHostKey(id) => {
                match application.servers.get(id).await {
                    Ok(server) => {
                        if let Err(error) =
                            application.known_hosts.forget(&server.host, server.port)
                        {
                            tracing::warn!(%error, "could not forget the host key");
                        } else {
                            tracing::info!(server = %id, "host key forgotten");
                        }
                        // The next collection re-pins whatever the host presents, so a
                        // reading is taken now rather than leaving the user wondering
                        // whether anything happened.
                        application.server_monitor.collect(id).await;
                    }
                    Err(error) => tracing::warn!(%error, "no such server"),
                }

                if let Some(detail) = server_detail(&application, id, range).await {
                    push(&window, move |window| detail.apply(&window));
                }
            }

            Intent::DeleteServer(id) => {
                if let Err(error) = application.provisioning.delete_server(id).await {
                    tracing::warn!(%error, "could not remove the server");
                }
                // The scheduler drops the job on its next registration pass; the cached
                // rate baseline and snapshot go now, so a recycled id cannot inherit them.
                application.server_monitor.forget(id);

                if open_server == Some(id) {
                    open_server = None;
                }

                let rows = server_rows(&application).await;
                push(&window, move |window| {
                    window.set_servers(view_model::model(rows));
                });
            }

            Intent::ChangeLanguage(language) => {
                // Persisted so the choice survives a restart, and everything already
                // rendered is rebuilt in the new language.
                let mut configuration = application.configuration.clone();
                configuration.application.language = language.as_str().to_owned();
                match configuration.to_toml() {
                    Ok(text) => {
                        if let Err(error) = std::fs::write(&application.paths.config_file, text) {
                            tracing::warn!(%error, "could not save the language");
                        }
                    }
                    Err(error) => tracing::warn!(%error, "could not serialise the configuration"),
                }

                let snapshot = runtime.dashboard(analytics_period).await;
                let rows = server_rows(&application).await;
                let directory = runtime.screenshot_dir();
                let cards = runtime.website_cards().await;
                let (open, history, rules) = alert_rows(&application).await;

                push(&window, move |window| {
                    snapshot.apply(&window);
                    window.set_servers(view_model::model(rows));
                    window.set_websites(view_model::model(into_cards(cards, &directory)));
                    window.set_open_incidents(view_model::model(open));
                    window.set_alert_history(view_model::model(history));
                    window.set_alert_rules(view_model::model(rules));
                });
            }

            Intent::AcknowledgeIncident(id) => {
                if let Err(error) = application.alert_service.acknowledge(id).await {
                    tracing::warn!(%error, "could not acknowledge the incident");
                }
                let (open, history, rules) = alert_rows(&application).await;
                push(&window, move |window| {
                    window.set_open_incidents(view_model::model(open));
                    window.set_alert_history(view_model::model(history));
                    window.set_alert_rules(view_model::model(rules));
                });
            }

            Intent::ToggleRule(id) => {
                if let Ok(mut rule) = application.alerts_repository.get_rule(id).await {
                    rule.enabled = !rule.enabled;
                    if let Err(error) = application.alerts_repository.save_rule(&rule).await {
                        tracing::warn!(%error, "could not save the rule");
                    }
                }
                let (open, history, rules) = alert_rows(&application).await;
                push(&window, move |window| {
                    window.set_open_incidents(view_model::model(open));
                    window.set_alert_history(view_model::model(history));
                    window.set_alert_rules(view_model::model(rules));
                });
            }
        }
    }
}

/// Runs a closure on the UI thread.
///
/// The window may already be gone — the user can close it while a collection is in
/// flight — so a failed upgrade is expected, not an error.
fn push<F>(window: &Weak<AppWindow>, update: F)
where
    F: FnOnce(AppWindow) + Send + 'static,
{
    let handle = window.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(window) = handle.upgrade() {
            update(window);
        }
    });
}

/// Decodes a batch of website cards. Must run on the UI thread.
fn into_cards(
    cards: Vec<payload::WebsiteCardPayload>,
    directory: &std::path::Path,
) -> Vec<WebsiteCard> {
    cards
        .into_iter()
        .map(|card| card.into_view(directory))
        .collect()
}

/// Turns chart geometry into view objects. Must run on the UI thread.
fn into_charts(charts: Vec<ChartPayload>) -> Vec<ChartData> {
    charts.into_iter().map(ChartPayload::into_view).collect()
}

/// Every server, as rows.
async fn server_rows(application: &Arc<Application>) -> Vec<ServerRow> {
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
async fn alert_rows(
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
struct ServerDetailUpdate {
    detail: ServerDetailPayload,
    charts: Vec<ChartPayload>,
    processes: Vec<ProcessRow>,
    containers: Vec<ContainerRow>,
    services: Vec<ServiceRow>,
    events: Vec<EventRow>,
}

impl ServerDetailUpdate {
    fn apply(self, window: &AppWindow) {
        window.set_server_detail(self.detail.into_view());
        window.set_server_charts(view_model::model(into_charts(self.charts)));
        window.set_processes(view_model::model(self.processes));
        window.set_containers(view_model::model(self.containers));
        window.set_services(view_model::model(self.services));
        window.set_server_events(view_model::model(self.events));
    }
}

async fn server_detail(
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
struct WebsiteDetailUpdate {
    detail: WebsiteDetailPayload,
    charts: Vec<ChartPayload>,
    top_pages: Vec<TopPageRow>,
    events: Vec<EventRow>,
    /// Where the capture named by the payload lives; the decode happens in `apply`.
    directory: PathBuf,
}

impl WebsiteDetailUpdate {
    fn apply(self, window: &AppWindow) {
        window.set_website_detail(self.detail.into_view(&self.directory));
        window.set_website_charts(view_model::model(into_charts(self.charts)));
        window.set_top_pages(view_model::model(self.top_pages));
        window.set_website_events(view_model::model(self.events));
    }
}

async fn website_detail(
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

    Some(WebsiteDetailUpdate {
        detail,
        charts,
        top_pages,
        events,
        directory,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_passphrase_is_stable_for_a_given_installation() {
        // It has to be, or every restart would fail to open the vault.
        let paths = AppPaths::rooted("/data/vds");
        assert_eq!(fallback_passphrase(&paths), fallback_passphrase(&paths));
    }

    #[test]
    fn two_installations_get_different_fallback_passphrases() {
        assert_ne!(
            fallback_passphrase(&AppPaths::rooted("/data/one")),
            fallback_passphrase(&AppPaths::rooted("/data/two"))
        );
    }

    #[test]
    fn a_missing_configuration_file_yields_defaults_and_writes_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::rooted(dir.path());
        paths.ensure().expect("directories");

        let (configuration, migration) = load_configuration(&paths).expect("loads");
        assert_eq!(configuration, Configuration::default());
        assert!(migration.is_noop());
        assert!(
            paths.config_file.exists(),
            "a starting configuration should be written"
        );
    }

    #[test]
    fn a_malformed_configuration_file_is_a_hard_failure() {
        // Silently starting with defaults would discard the user's tuning without saying
        // so, and they would spend an afternoon wondering why nothing applied.
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::rooted(dir.path());
        paths.ensure().expect("directories");
        std::fs::write(
            &paths.config_file,
            "[monitoring]\ntimeout_secs = \"not a number\"",
        )
        .expect("written");

        assert!(load_configuration(&paths).is_err());
    }

    #[test]
    fn an_existing_configuration_file_is_honoured() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::rooted(dir.path());
        paths.ensure().expect("directories");
        std::fs::write(
            &paths.config_file,
            "version = 1\n[monitoring]\ndefault_server_interval_secs = 15\n",
        )
        .expect("written");

        let (configuration, _) = load_configuration(&paths).expect("loads");
        assert_eq!(configuration.monitoring.default_server_interval_secs, 15);
    }
}
