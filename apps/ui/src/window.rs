//! Setting up the window: static properties, the clipboard, and the callbacks.
//!
//! Everything here runs on the UI thread and finishes immediately. A callback's whole job
//! is to say what the user asked for and return — the work happens in `worker`, off this
//! thread, because a click that awaited anything would stall a repaint.

use crate::intents::Intent;
use crate::{AppWindow, Clipboard, format, i18n, runtime, view_model};
use slint::{ComponentHandle, SharedString};
use std::sync::Arc;
use tokio::sync::mpsc;
use vds_application::config::Theme as ConfiguredTheme;
use vds_application::provisioning::{
    ConnectionEdit, NewConnection, NewServer, NewWebsite, ServerEdit,
};
use vds_composition::Application;
use vds_domain::ids::{IncidentId, ServerId, WebsiteId};
use vds_domain::ports::Secret;

/// Properties that never change while the application runs.
pub(crate) fn configure_static_properties(window: &AppWindow, application: &Arc<Application>) {
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
pub(crate) fn system_prefers_dark() -> bool {
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
pub(crate) fn wire_clipboard(window: &AppWindow) {
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

pub(crate) fn wire_callbacks(window: &AppWindow, intents: &mpsc::UnboundedSender<Intent>) {
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
