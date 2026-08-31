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
mod intents;
mod payload;
mod queries;
mod runtime;
mod scheduling;
mod view_model;
mod window;
mod worker;

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
use intents::Intent;
use slint::ComponentHandle;
use std::sync::Arc;
use tokio::sync::mpsc;
use vds_application::config::Configuration;
use vds_composition::{AppPaths, Application, PersistentEventPublisher, SecretsSetup, logging};

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

    window::wire_clipboard(&window);
    window::configure_static_properties(&window, &application);

    let (intents, receiver) = mpsc::unbounded_channel::<Intent>();
    window::wire_callbacks(&window, &intents);

    // The worker owns every service; the window owns nothing but its own state.
    tokio.spawn(worker::worker(
        Arc::clone(&application),
        window.as_weak(),
        receiver,
    ));

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
