//! The other half of the application: everything that is not the UI thread.
//!
//! The worker owns the runtime's side of the boundary. It receives intents, does the
//! work, and hands finished results back through `push`, which is the only sanctioned way
//! to touch a window property from another thread.
//!
//! Note where the conversions live. Slint's `Image` and `ModelRc` are not `Send`, so
//! every function here that builds one is called *inside* a `push` closure, on the UI
//! thread, at the last possible moment. See `payload`.

use crate::intents::Intent;
use crate::payload::ChartPayload;
use crate::queries::{alert_rows, server_detail, server_rows, website_detail};
use crate::{
    AppWindow, ChartData, FileEntry, SiteFolder, WebsiteCard, format, i18n, payload, runtime,
    view_model,
};
use slint::{SharedString, Weak};
use std::sync::Arc;
use tokio::sync::mpsc;
use vds_application::files;
use vds_application::files::Preview;
use vds_composition::Application;
use vds_domain::analytics::AnalyticsPeriod;
use vds_domain::ids::{ServerId, WebsiteId};

/// Handles intents, off the UI thread.
pub(crate) async fn worker(
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
                // Why the last scheduled refresh failed, if it did. A refresh runs with
                // nobody watching, so without this an expired token is indistinguishable
                // from a site that simply has no traffic.
                let failure = application
                    .analytics
                    .last_failure()
                    .map(view_model::describe_provider_error);
                push(&window, move |window| {
                    update.apply(&window);
                    window.set_analytics_token_saved(token_saved);
                    window.set_analytics_error(failure.unwrap_or_default().into());
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
pub(crate) fn clear_preview(window: &AppWindow) {
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
pub(crate) fn show_preview(window: &Weak<AppWindow>, path: String, preview: Preview) {
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
pub(crate) async fn show_listing(
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
pub(crate) fn push<F>(window: &Weak<AppWindow>, update: F)
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
pub(crate) fn into_cards(
    cards: Vec<payload::WebsiteCardPayload>,
    directory: &std::path::Path,
) -> Vec<WebsiteCard> {
    cards
        .into_iter()
        .map(|card| card.into_view(directory))
        .collect()
}

/// Turns chart geometry into view objects. Must run on the UI thread.
pub(crate) fn into_charts(charts: Vec<ChartPayload>) -> Vec<ChartData> {
    charts.into_iter().map(ChartPayload::into_view).collect()
}
