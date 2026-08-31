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
use vds_application::files;
use vds_application::files::Preview;
use vds_application::provisioning::{
    ConnectionEdit, NewConnection, NewServer, NewWebsite, ServerEdit,
};
use vds_composition::{AppPaths, Application, PersistentEventPublisher, SecretsSetup, logging};
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
    SaveAnalyticsToken(Secret),
    UpdateServer {
        id: ServerId,
        edit: Box<ServerEdit>,
    },
    UpdateWebsite {
        id: WebsiteId,
        edit: Box<NewWebsite>,
    },
    BeginEditServer(ServerId),
    BeginEditWebsite(WebsiteId),
    ConnectAnalytics {
        website: WebsiteId,
        counter: String,
    },
    DisconnectAnalytics(WebsiteId),

    // --- files ---
    // The one group of intents that changes something on a server. Each carries only a
    // name or a path; which server it applies to is the one the worker has open, so a
    // stale click cannot land on a different machine.
    OpenFiles,
    BrowseTo(String),
    OpenFileEntry {
        name: String,
        is_directory: bool,
    },
    SaveOpenFile(String),
    CloseOpenFile,
    DeleteFileEntry(String),
    CreateFileEntry {
        name: String,
        is_directory: bool,
    },
    RefreshFiles,
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

    // Built before the application because the producers inside it take the publisher as
    // a dependency; the repository it writes to only exists afterwards, which is why the
    // receiving half is handed over separately below.
    let (events, event_log) = PersistentEventPublisher::new();

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
            Arc::clone(&events) as Arc<dyn vds_domain::ports::EventPublisher>,
            secrets,
        )
        .await
    })?;

    let application = Arc::new(application);

    // Without this the events table stays empty however much happens: every producer
    // publishes, and nothing writes it down.
    tokio.spawn(vds_composition::write_events(
        event_log,
        Arc::clone(&application.events_repository),
        Arc::clone(&application.clock),
    ));

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

    wire_clipboard(&window);
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
    // Also publishes whether an analytics token is stored, which the connect form needs.
    let _ = intents.send(Intent::RefreshAnalytics);

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
/// Serves the clipboard shortcuts that the toolkit cannot.
///
/// Slint matches shortcuts on the character a key produces rather than on the physical
/// key, so on a Russian layout Ctrl+V arrives as "м" and paste silently does nothing —
/// in an application whose interface is in Russian and whose main use for paste is an
/// SSH key. `clipboard.slint` catches those and calls in here.
///
/// A failure is deliberately quiet: an empty string on read, and nothing written on
/// write. There is no clipboard on a headless CI runner, and a monitoring application
/// must not refuse to start over one.
fn wire_clipboard(window: &AppWindow) {
    use copypasta::ClipboardProvider;

    window.global::<Clipboard>().on_read(|| {
        copypasta::ClipboardContext::new()
            .and_then(|mut ctx| ctx.get_contents())
            .unwrap_or_else(|error| {
                tracing::debug!(%error, "could not read the clipboard");
                String::new()
            })
            .into()
    });

    window.global::<Clipboard>().on_write(|text| {
        if let Err(error) = copypasta::ClipboardContext::new()
            .and_then(|mut ctx| ctx.set_contents(text.to_string()))
        {
            tracing::debug!(%error, "could not write to the clipboard");
        }
    });
}

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
    window.on_begin_edit_website(move |id| match WebsiteId::parse(&id) {
        Ok(id) => send(&queue, Intent::BeginEditWebsite(id)),
        Err(error) => tracing::warn!(%error, "ignoring a malformed website id"),
    });

    let queue = intents.clone();
    window.on_update_website(move |id, name, url, interval, status, text| {
        let Ok(id) = WebsiteId::parse(&id) else {
            tracing::warn!("ignoring a malformed website id");
            return;
        };
        send(
            &queue,
            Intent::UpdateWebsite {
                id,
                edit: Box::new(NewWebsite {
                    name: name.to_string(),
                    url: url.to_string(),
                    poll_interval_secs: runtime::number_or(
                        &interval,
                        vds_domain::website::DEFAULT_WEBSITE_POLL_INTERVAL_SECS,
                    ),
                    expected_status: runtime::number_or(&status, 200),
                    // Empty means "do not check the body". An empty expectation would
                    // match any response, so the check would pass while testing nothing.
                    expected_text: Some(text.to_string()),
                    server_id: None,
                }),
            },
        );
    });

    let queue = intents.clone();
    window.on_begin_edit_server(move |id| match ServerId::parse(&id) {
        Ok(id) => send(&queue, Intent::BeginEditServer(id)),
        Err(error) => tracing::warn!(%error, "ignoring a malformed server id"),
    });

    let queue = intents.clone();
    #[allow(clippy::too_many_arguments)]
    window.on_update_server(
        move |id,
              name,
              host,
              port,
              mode,
              auth_kind,
              username,
              secret,
              passphrase,
              token,
              interval| {
            let Ok(id) = ServerId::parse(&id) else {
                tracing::warn!("ignoring a malformed server id");
                return;
            };
            // An empty field means "keep the stored credential", which is the whole
            // reason editing exists as its own operation.
            let optional = |value: SharedString| {
                let value = value.to_string();
                (!value.trim().is_empty()).then(|| Secret::from_string(value))
            };

            let connection = if runtime::is_agent_mode(mode) {
                ConnectionEdit::Agent {
                    port: runtime::number_or(&port, vds_domain::server::DEFAULT_AGENT_PORT),
                    token: optional(token),
                }
            } else {
                ConnectionEdit::Ssh {
                    username: username.to_string(),
                    auth_kind: runtime::auth_kind_at(auth_kind),
                    secret: optional(secret),
                    passphrase: optional(passphrase),
                }
            };

            send(
                &queue,
                Intent::UpdateServer {
                    id,
                    edit: Box::new(ServerEdit {
                        name: name.to_string(),
                        host: host.to_string(),
                        port: if runtime::is_agent_mode(mode) {
                            vds_domain::server::DEFAULT_SSH_PORT
                        } else {
                            runtime::number_or(&port, vds_domain::server::DEFAULT_SSH_PORT)
                        },
                        connection,
                        poll_interval_secs: runtime::number_or(
                            &interval,
                            vds_domain::server::DEFAULT_POLL_INTERVAL_SECS,
                        ),
                        enabled: true,
                        tags: Vec::new(),
                    }),
                },
            );
        },
    );

    let queue = intents.clone();
    window.on_save_analytics_token(move |token| {
        // Wrapped in `Secret` at the boundary, so it is redacted from every log line and
        // zeroed on drop from here on.
        send(
            &queue,
            Intent::SaveAnalyticsToken(Secret::from_string(token.to_string())),
        );
    });

    let queue = intents.clone();
    window.on_connect_analytics(move |website, counter| match WebsiteId::parse(&website) {
        Ok(website) => send(
            &queue,
            Intent::ConnectAnalytics {
                website,
                counter: counter.to_string(),
            },
        ),
        Err(error) => tracing::warn!(%error, "ignoring a malformed website id"),
    });

    let queue = intents.clone();
    window.on_disconnect_analytics(move |website| match WebsiteId::parse(&website) {
        Ok(website) => send(&queue, Intent::DisconnectAnalytics(website)),
        Err(error) => tracing::warn!(%error, "ignoring a malformed website id"),
    });

    let queue = intents.clone();
    window.on_save_notification_settings(move || send(&queue, Intent::RefreshAlerts));

    let queue = intents.clone();
    window.on_screenshot_policy_changed(move |_index| send(&queue, Intent::RefreshWebsites));

    // --- files ---
    // Each of these says what the user asked for and nothing else. Which server it lands
    // on is the worker's business, so a click that arrives after the user has moved on
    // cannot reach a machine they are no longer looking at.

    let queue = intents.clone();
    window.on_open_files(move || send(&queue, Intent::OpenFiles));

    let queue = intents.clone();
    window.on_browse_path(move |path| send(&queue, Intent::BrowseTo(path.to_string())));

    let queue = intents.clone();
    window.on_open_file_entry(move |name, is_directory| {
        send(
            &queue,
            Intent::OpenFileEntry {
                name: name.to_string(),
                is_directory,
            },
        );
    });

    let queue = intents.clone();
    window.on_save_open_file(move |text| send(&queue, Intent::SaveOpenFile(text.to_string())));

    let queue = intents.clone();
    window.on_close_open_file(move || send(&queue, Intent::CloseOpenFile));

    let queue = intents.clone();
    window.on_delete_file_entry(move |name| {
        send(&queue, Intent::DeleteFileEntry(name.to_string()));
    });

    let queue = intents.clone();
    window.on_create_file_entry(move |name, is_directory| {
        let name = name.trim().to_owned();
        // A blank name would build the folder's own path, and deleting or overwriting the
        // folder you are standing in is not what anyone meant by "new file".
        if name.is_empty() || name.contains('/') {
            return;
        }
        send(&queue, Intent::CreateFileEntry { name, is_directory });
    });

    let queue = intents.clone();
    window.on_refresh_files(move || send(&queue, Intent::RefreshFiles));
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
    // Where the file browser is, and what it has open. Held here rather than read back
    // from the window: the view is a projection, and a path that only exists on screen
    // could be edited by a click that arrives while a listing is still in flight.
    let mut browse_path = files::DEFAULT_START_PATH.to_owned();
    let mut open_file: Option<String> = None;
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

            Intent::BeginEditWebsite(id) => {
                let Ok(website) = application.websites.get(id).await else {
                    continue;
                };
                push(&window, move |window| {
                    window.set_form_website_name(website.name.clone().into());
                    window.set_form_url(website.url.clone().into());
                    window.set_form_website_interval(website.poll_interval_secs.to_string().into());
                    window.set_form_expected_status(website.expectation.status.to_string().into());
                    window.set_form_expected_text(
                        website
                            .expectation
                            .body_contains
                            .clone()
                            .unwrap_or_default()
                            .into(),
                    );

                    window.set_editing_website(true);
                    window.set_dialog_error(SharedString::new());
                    window.set_showing_add_website(true);
                });
            }

            Intent::BeginEditServer(id) => {
                // The form is filled from stored state rather than from what is on
                // screen, so an abandoned edit leaves nothing behind.
                let Ok(server) = application.servers.get(id).await else {
                    continue;
                };
                push(&window, move |window| {
                    window.set_form_server_name(server.name.clone().into());
                    window.set_form_host(server.host.clone().into());
                    window.set_form_port(server.port.to_string().into());
                    window.set_form_interval(server.poll_interval_secs.to_string().into());

                    match &server.connection {
                        vds_domain::server::ConnectionSettings::Ssh(ssh) => {
                            window.set_form_mode(0);
                            window.set_form_username(ssh.username.clone().into());
                            window.set_form_auth_kind(runtime::auth_kind_index(ssh.auth_kind));
                        }
                        vds_domain::server::ConnectionSettings::Agent(agent) => {
                            window.set_form_mode(1);
                            window.set_form_port(agent.port.to_string().into());
                        }
                    }

                    window.set_editing_server(true);
                    window.set_dialog_error(SharedString::new());
                    window.set_showing_add_server(true);
                });
            }

            Intent::UpdateServer { id, edit } => {
                match application.provisioning.update_server(id, *edit).await {
                    Ok(_) => {
                        push(&window, |window| {
                            window.set_showing_add_server(false);
                            window.set_editing_server(false);
                        });
                        let rows = server_rows(&application).await;
                        push(&window, move |window| {
                            window.set_servers(view_model::model(rows));
                        });
                        if let Some(detail) = server_detail(&application, id, range).await {
                            push(&window, move |window| detail.apply(&window));
                        }
                    }
                    Err(error) => {
                        let message = view_model::describe_provisioning_error(&error);
                        push(&window, move |window| {
                            window.set_dialog_error(message.into());
                        });
                    }
                }
            }

            Intent::UpdateWebsite { id, edit } => {
                match application.provisioning.update_website(id, *edit).await {
                    Ok(_) => {
                        push(&window, |window| {
                            window.set_showing_add_website(false);
                            window.set_editing_website(false);
                        });
                        let directory = runtime.screenshot_dir();
                        let cards = runtime.website_cards().await;
                        push(&window, move |window| {
                            window.set_websites(view_model::model(into_cards(cards, &directory)));
                        });
                    }
                    Err(error) => {
                        let message = view_model::describe_provisioning_error(&error);
                        push(&window, move |window| {
                            window.set_dialog_error(message.into());
                        });
                    }
                }
            }

            Intent::SaveAnalyticsToken(token) => {
                let outcome = application.provisioning.save_analytics_token(token).await;
                let saved = application.provisioning.has_analytics_token().await;
                let message = match outcome {
                    Ok(()) => String::new(),
                    Err(error) => view_model::describe_provisioning_error(&error),
                };
                push(&window, move |window| {
                    window.set_analytics_token_saved(saved);
                    window.set_analytics_error(message.into());
                    // Cleared once stored: the field has served its purpose and the token
                    // should not sit in a window property for the rest of the session.
                    window.set_metrica_token(SharedString::new());
                });
            }

            Intent::ConnectAnalytics { website, counter } => {
                // Asked for rather than named: the UI must not know which provider
                // crate exists. See `AnalyticsService::default_provider`.
                let Some(provider) = application.analytics.default_provider() else {
                    tracing::warn!("no analytics provider is registered");
                    continue;
                };
                let message = match application
                    .provisioning
                    .connect_analytics(website, provider, &counter)
                    .await
                {
                    Ok(_) => String::new(),
                    Err(error) => view_model::describe_provisioning_error(&error),
                };

                push(&window, {
                    let message = message.clone();
                    move |window| window.set_analytics_error(message.into())
                });

                if message.is_empty()
                    && let Some(detail) = website_detail(&runtime, website, analytics_period).await
                {
                    push(&window, move |window| detail.apply(&window));
                    let update = runtime.analytics(analytics_period, analytics_metric).await;
                    push(&window, move |window| update.apply(&window));
                }
            }

            Intent::DisconnectAnalytics(website) => {
                let Some(provider) = application.analytics.default_provider() else {
                    continue;
                };
                if let Err(error) = application
                    .provisioning
                    .disconnect_analytics(website, &provider)
                    .await
                {
                    tracing::warn!(%error, "could not disconnect the counter");
                }
                if let Some(detail) = website_detail(&runtime, website, analytics_period).await {
                    push(&window, move |window| detail.apply(&window));
                }
                let update = runtime.analytics(analytics_period, analytics_metric).await;
                push(&window, move |window| update.apply(&window));
            }

            Intent::RefreshAnalytics => {
                let update = runtime.analytics(analytics_period, analytics_metric).await;
                // Whether a token is stored decides what the connect form offers. Read
                // here rather than on the UI thread: it touches the OS keystore, which
                // can block for as long as the keyring feels like it.
                let token_saved = application.provisioning.has_analytics_token().await;
                push(&window, move |window| {
                    update.apply(&window);
                    window.set_analytics_token_saved(token_saved);
                });
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

            // --- files ---
            Intent::OpenFiles => {
                let Some(id) = open_server else { continue };
                let Ok(server) = application.servers.get(id).await else {
                    continue;
                };

                let name = server.name.clone();
                open_file = None;
                push(&window, move |window| {
                    window.set_files_server_name(name.into());
                    clear_preview(&window);
                    window.set_files_error(SharedString::new());
                    window.set_files_busy(true);
                    window.set_showing_files(true);
                });

                // Where the server says its sites live. This also decides where browsing
                // starts, so that opening the browser lands somewhere useful rather than
                // in whatever directory happens to exist.
                let roots = application.files.site_roots(id).await.unwrap_or_default();
                if let Some(first) = roots.first() {
                    browse_path = first.path.clone();
                }
                let rows: Vec<SiteFolder> = roots.iter().map(view_model::site_folder_row).collect();
                push(&window, move |window| {
                    window.set_site_folders(view_model::model(rows));
                });

                // `/var/www` is a good guess and not a certainty. A machine that does
                // not have it is not a machine you should be unable to browse.
                match show_listing(&application, &window, id, &browse_path).await {
                    Some(path) => browse_path = path,
                    None => {
                        if let Some(path) = show_listing(&application, &window, id, "/").await {
                            browse_path = path;
                        }
                    }
                }
            }

            Intent::BrowseTo(path) => {
                let Some(id) = open_server else { continue };
                open_file = None;
                push(&window, |window| {
                    clear_preview(&window);
                    window.set_files_busy(true);
                });
                // Only a listing that succeeded moves us: a failed navigation must leave
                // the browser where it was, not in a folder it could not read.
                if let Some(path) = show_listing(&application, &window, id, &path).await {
                    browse_path = path;
                }
            }

            Intent::RefreshFiles => {
                let Some(id) = open_server else { continue };
                push(&window, |window| window.set_files_busy(true));
                show_listing(&application, &window, id, &browse_path).await;
            }

            Intent::OpenFileEntry { name, is_directory } => {
                let Some(id) = open_server else { continue };
                let path = files::join(&browse_path, &name);

                if is_directory {
                    push(&window, |window| window.set_files_busy(true));
                    if let Some(path) = show_listing(&application, &window, id, &path).await {
                        browse_path = path;
                    }
                    continue;
                }

                push(&window, |window| {
                    window.set_files_busy(true);
                    window.set_files_error(SharedString::new());
                });

                // One fetch answers everything: picture, text, or neither. Which of the
                // three it is comes from the bytes, not from the file's name.
                match application.files.open(id, &path).await {
                    Ok(preview) => {
                        // Only text is editable, so only text is remembered as open. A
                        // save while a picture is on screen would have nothing to save.
                        open_file = matches!(preview, Preview::Text(_)).then(|| path.clone());
                        show_preview(&window, path, preview);
                    }
                    Err(error) => {
                        let message = view_model::describe_file_error(&error);
                        push(&window, move |window| {
                            window.set_files_error(message.into());
                            window.set_files_busy(false);
                        });
                    }
                }
            }

            Intent::SaveOpenFile(text) => {
                let Some(id) = open_server else { continue };
                let Some(path) = open_file.clone() else {
                    continue;
                };

                push(&window, |window| {
                    window.set_files_busy(true);
                    window.set_files_error(SharedString::new());
                });
                match application.files.write(id, &path, &text).await {
                    Ok(()) => push(&window, move |window| {
                        // Only now does the saved copy move. Until the server confirmed
                        // it, the editor was right to say there were unsaved changes.
                        window.set_open_file_saved_text(text.into());
                        window.set_files_busy(false);
                    }),
                    Err(error) => {
                        let message = view_model::describe_file_error(&error);
                        push(&window, move |window| {
                            window.set_files_error(message.into());
                            window.set_files_busy(false);
                        });
                    }
                }
            }

            Intent::CloseOpenFile => {
                let Some(id) = open_server else { continue };
                open_file = None;
                push(&window, |window| {
                    clear_preview(&window);
                    window.set_files_busy(true);
                });
                show_listing(&application, &window, id, &browse_path).await;
            }

            Intent::DeleteFileEntry(name) => {
                let Some(id) = open_server else { continue };
                let path = files::join(&browse_path, &name);

                push(&window, |window| {
                    window.set_files_busy(true);
                    window.set_files_error(SharedString::new());
                });
                if let Err(error) = application.files.delete(id, &path).await {
                    let message = view_model::describe_file_error(&error);
                    push(&window, move |window| {
                        window.set_files_error(message.into())
                    });
                }
                // Listed again either way: after a failure the row is still there, and
                // showing it gone would be a lie.
                show_listing(&application, &window, id, &browse_path).await;
            }

            Intent::CreateFileEntry { name, is_directory } => {
                let Some(id) = open_server else { continue };
                let path = files::join(&browse_path, name.trim());

                push(&window, |window| {
                    window.set_files_busy(true);
                    window.set_files_error(SharedString::new());
                });
                let outcome = if is_directory {
                    application.files.create_directory(id, &path).await
                } else {
                    // An empty file. Creating one with content would mean guessing what
                    // belongs in it; this makes it and lets the editor fill it.
                    application.files.write(id, &path, "").await
                };
                if let Err(error) = outcome {
                    let message = view_model::describe_file_error(&error);
                    push(&window, move |window| {
                        window.set_files_error(message.into())
                    });
                }
                show_listing(&application, &window, id, &browse_path).await;
            }
        }
    }
}

/// Puts the browser back on the listing, with nothing open.
fn clear_preview(window: &AppWindow) {
    window.set_open_file_path(SharedString::new());
    window.set_open_file_kind(SharedString::new());
    window.set_open_file_text(SharedString::new());
    window.set_open_file_saved_text(SharedString::new());
    window.set_open_file_message(SharedString::new());
    window.set_open_file_has_image(false);
    window.set_open_file_truncated(false);
}

/// Shows whatever was found at a path.
///
/// Everything here runs on the UI thread, because an image cannot be built anywhere else
/// (see `payload`). The worker hands over bytes; the pixels are made at the last moment.
fn show_preview(window: &Weak<AppWindow>, path: String, preview: Preview) {
    let strings = i18n::strings();
    let kind = preview.kind();
    let size = format::bytes(preview.size_bytes() as f64);

    match preview {
        Preview::Text(contents) => {
            let info = if contents.truncated {
                format!("{size} \u{b7} {}", strings.files_read_only)
            } else {
                size
            };
            push(window, move |window| {
                window.set_open_file_kind(kind.into());
                window.set_open_file_path(path.into());
                window.set_open_file_info(info.into());
                window.set_open_file_truncated(contents.truncated);
                window.set_open_file_text(contents.text.clone().into());
                // The saved copy is what "has this changed" is measured against, so it
                // is set from the same read.
                window.set_open_file_saved_text(contents.text.into());
                window.set_files_busy(false);
            });
        }

        Preview::Image(image) => {
            // Truncated bytes will not decode, and saying so is better than an image
            // widget that silently shows nothing.
            let too_large = image.truncated;
            let format_name = image.format;
            push(window, move |window| {
                window.set_open_file_kind(kind.into());
                window.set_open_file_path(path.into());
                window.set_open_file_truncated(false);
                window.set_open_file_text(SharedString::new());
                window.set_open_file_saved_text(SharedString::new());

                let decoded = (!too_large)
                    .then(|| payload::decode_preview(&image.bytes))
                    .flatten();

                match decoded {
                    Some((decoded, width, height)) => {
                        let dimensions = view_model::fill2(
                            strings.files_image_size,
                            &width.to_string(),
                            &height.to_string(),
                        );
                        window.set_open_file_has_image(true);
                        window.set_open_file_image(decoded);
                        window.set_open_file_info(
                            format!("{format_name} \u{b7} {dimensions} \u{b7} {size}").into(),
                        );
                        window.set_open_file_message(SharedString::new());
                    }
                    None => {
                        window.set_open_file_has_image(false);
                        window.set_open_file_info(size.into());
                        window.set_open_file_message(
                            if too_large {
                                strings.files_image_too_large
                            } else {
                                strings.files_image_broken
                            }
                            .into(),
                        );
                    }
                }
                window.set_files_busy(false);
            });
        }

        Preview::Binary { .. } => push(window, move |window| {
            window.set_open_file_kind(kind.into());
            window.set_open_file_path(path.into());
            window.set_open_file_info(size.clone().into());
            window.set_open_file_message(size.into());
            window.set_open_file_truncated(false);
            window.set_open_file_has_image(false);
            window.set_open_file_text(SharedString::new());
            window.set_open_file_saved_text(SharedString::new());
            window.set_files_busy(false);
        }),
    }
}

/// Lists one directory and shows it, returning the path that was actually read.
///
/// `None` on failure, which is what keeps a failed navigation from moving the browser
/// into a folder it could not read.
async fn show_listing(
    application: &Arc<Application>,
    window: &Weak<AppWindow>,
    server: ServerId,
    path: &str,
) -> Option<String> {
    let now = chrono::Utc::now();
    match application.files.list(server, path).await {
        Ok(listing) => {
            let path = listing.path.clone();
            push(window, move |window| {
                window.set_files_path(listing.path.into());
                // Rows are built here because a `SharedString` cannot cross a thread.
                window.set_file_entries(view_model::model(
                    listing
                        .entries
                        .iter()
                        .map(|entry| view_model::file_entry_row(entry, now))
                        .collect::<Vec<_>>(),
                ));
                window.set_files_error(SharedString::new());
                window.set_files_busy(false);
            });
            Some(path)
        }
        Err(error) => {
            let message = view_model::describe_file_error(&error);
            let attempted = path.to_owned();
            push(window, move |window| {
                // The path that failed is still shown. "Where am I" is the question every
                // mistake with a file browser starts with, and a blank box does not
                // answer it.
                window.set_files_path(attempted.into());
                window.set_file_entries(view_model::model(Vec::<FileEntry>::new()));
                window.set_files_error(message.into());
                window.set_files_busy(false);
            });
            None
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
    /// The counter this website is connected to, empty when it is not. Shown next to the
    /// figures so a wrong number is visible rather than inferred from odd traffic.
    counter: String,
    /// Whether the shared token exists, which decides whether the connect form is usable.
    token_saved: bool,
}

impl WebsiteDetailUpdate {
    fn apply(self, window: &AppWindow) {
        window.set_website_detail(self.detail.into_view(&self.directory));
        window.set_website_charts(view_model::model(into_charts(self.charts)));
        window.set_top_pages(view_model::model(self.top_pages));
        window.set_website_counter(self.counter.into());
        window.set_analytics_token_saved(self.token_saved);
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
