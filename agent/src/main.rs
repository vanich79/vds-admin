//! # `vds-agent`
//!
//! The optional daemon side of VDS Admin — "Mode B" in `docs/ARCHITECTURE.md`. It reads
//! the host it runs on and serves the result over HTTPS to the desktop or mobile app.
//!
//! ## Why an agent exists at all when SSH mode works
//!
//! SSH mode opens a connection, runs a batch of commands and tears it down, once per
//! poll, per server. That is fine for tens of servers and expensive for hundreds: the
//! handshake dominates, and a fleet of a thousand is a thousand key exchanges a minute.
//! The agent moves the cost to the machine being watched, where it is a few `/proc` reads
//! behind a short cache, and removes the need to hand the monitoring application SSH
//! credentials to every box.
//!
//! ## What it deliberately does not do
//!
//! It runs no commands on request, writes nothing, and has no endpoint that changes the
//! host. A stolen token is worth a reading of the machine — which is a real loss, and a
//! bounded one. Restarting services and containers is a feature the *app* is structured
//! for, and adding it here would change what that token is worth.
//!
//! ## Resource budget
//!
//! The release profile builds for size (`opt-level = "z"`, LTO, stripped). At rest the
//! process is an idle Tokio runtime and a cached report: a few megabytes of RSS and no
//! measurable CPU. Collection happens on request, not on a timer, so an agent nobody is
//! watching costs nothing.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod auth;
mod collect;
mod config;
mod report;
mod server;
mod tls;

use clap::Parser;
use config::AgentConfig;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// Default location of the configuration file.
const DEFAULT_CONFIG: &str = "/etc/vds-agent/agent.toml";

/// How long in-flight requests are given after a shutdown signal.
const SHUTDOWN_GRACE_SECS: u64 = 5;

#[derive(Parser)]
#[command(
    name = "vds-agent",
    version,
    about = "Serves host metrics to VDS Admin over HTTPS."
)]
struct Cli {
    /// Path to the configuration file.
    #[arg(short, long, env = "VDS_AGENT_CONFIG", default_value = DEFAULT_CONFIG)]
    config: PathBuf,

    /// Validate the configuration and TLS material, then exit.
    ///
    /// Used by the installer and by `ExecStartPre`, so a bad edit fails before the
    /// running agent is replaced.
    #[arg(long)]
    check: bool,

    /// Print the certificate fingerprint and exit.
    ///
    /// This is the value an operator compares against what the app pinned.
    #[arg(long)]
    fingerprint: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Reported rather than panicked: systemd shows the message, and a backtrace
            // through the TLS stack helps nobody diagnose a missing token.
            eprintln!("vds-agent: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Every way the agent can fail to start.
#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Tls(#[from] tls::TlsError),
    #[error("could not install the cryptography provider")]
    CryptoProvider,
    #[error("could not start the runtime: {0}")]
    Runtime(std::io::Error),
    #[error("could not listen on {address}: {source}")]
    Listen {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

fn run(cli: Cli) -> Result<(), StartupError> {
    let config = AgentConfig::load(&cli.config)?;
    init_logging(&config.log_level);

    // `ring` is installed explicitly because the process, not the library, owns this
    // choice; see the note on `rustls` in the workspace manifest.
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| StartupError::CryptoProvider)?;

    let hostname = hostname();
    let material = tls::load_or_generate(
        &config.certificate_path(),
        &config.private_key_path(),
        std::slice::from_ref(&hostname),
    )?;

    let fingerprint = material
        .certificates
        .first()
        .map(tls::fingerprint)
        .unwrap_or_default();

    if cli.fingerprint {
        println!("{fingerprint}");
        return Ok(());
    }

    if material.generated {
        tracing::info!(
            certificate = %config.certificate_path().display(),
            "generated a self-signed certificate"
        );
    }

    if cli.check {
        // Building the rustls config is the real test: it is where a mismatched
        // certificate and key are caught.
        tls::server_config(material)?;
        println!("configuration is valid");
        println!("certificate fingerprint: {fingerprint}");
        return Ok(());
    }

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        protocol = %vds_agent_protocol::PROTOCOL_VERSION,
        %hostname,
        %fingerprint,
        "starting"
    );
    // Logged so an operator can see what took effect. The token is not in this rendering;
    // see `AgentConfig`'s hand-written `Debug`.
    tracing::debug!(configuration = ?config, "effective configuration");

    let tls_config = tls::server_config(material)?;
    let address = SocketAddr::new(config.bind, config.port);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        // Two workers is enough for a daemon whose busiest moment is reading a dozen
        // files, and it keeps the footprint predictable on a single-core VPS.
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(StartupError::Runtime)?;

    let state = Arc::new(server::AgentState {
        collector: collect::Collector::new(&config),
        token: config.token().to_owned(),
        started_at: Instant::now(),
        hostname,
    });

    runtime.block_on(async move {
        let shutdown = CancellationToken::new();
        spawn_signal_handler(shutdown.clone());

        let result = server::serve(address, tls_config, server::router(state), shutdown)
            .await
            .map_err(|source| StartupError::Listen { address, source });

        // In-flight requests get a moment to finish; a metrics scrape that is cut off
        // mid-body looks to the app like a failed collection.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        result
    })?;

    tracing::info!("stopped");
    Ok(())
}

/// Cancels the token on SIGTERM or Ctrl-C.
///
/// SIGTERM is what systemd sends on `stop` and on `restart`, so handling it is the
/// difference between a clean stop and the unit being killed after its timeout.
fn spawn_signal_handler(shutdown: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};

            let mut terminate = match signal(SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(err) => {
                    tracing::warn!(error = %err, "could not listen for SIGTERM");
                    return;
                }
            };

            tokio::select! {
                _ = terminate.recv() => tracing::info!("received SIGTERM"),
                result = tokio::signal::ctrl_c() => match result {
                    Ok(()) => tracing::info!("received an interrupt"),
                    Err(err) => tracing::warn!(error = %err, "could not listen for Ctrl-C"),
                },
            }
        }
        #[cfg(not(unix))]
        {
            match tokio::signal::ctrl_c().await {
                Ok(()) => tracing::info!("received an interrupt"),
                Err(err) => tracing::warn!(error = %err, "could not listen for Ctrl-C"),
            }
        }

        shutdown.cancel();

        // A second signal, or an operator out of patience, should not have to wait.
        tokio::time::sleep(std::time::Duration::from_secs(SHUTDOWN_GRACE_SECS)).await;
    });
}

/// The machine's name.
///
/// Read from `/proc` rather than through a crate: it is one file, and the agent's
/// dependency list is part of how easily it cross-compiles to five architectures.
fn hostname() -> String {
    #[cfg(unix)]
    {
        for path in ["/proc/sys/kernel/hostname", "/etc/hostname"] {
            if let Ok(contents) = std::fs::read_to_string(path) {
                let name = contents.trim();
                if !name.is_empty() {
                    return name.to_owned();
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Ok(name) = std::env::var("COMPUTERNAME")
            && !name.trim().is_empty()
        {
            return name;
        }
    }

    // Better than an empty string in a log line, and honest about what happened.
    "unknown".to_owned()
}

/// Sets up structured logging on stderr, which is where systemd's journal collects it.
fn init_logging(level: &str) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("VDS_AGENT_LOG")
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // `try_init` rather than `init`: a second call in a test binary must not abort the
    // process.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        // The journal adds its own timestamps, so ours would be duplicated noise.
        .without_time()
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cli_accepts_the_documented_flags() {
        let cli = Cli::try_parse_from(["vds-agent", "--config", "/tmp/a.toml", "--check"])
            .expect("parses");
        assert_eq!(cli.config, PathBuf::from("/tmp/a.toml"));
        assert!(cli.check);
        assert!(!cli.fingerprint);
    }

    #[test]
    fn the_configuration_path_has_a_documented_default() {
        // The systemd unit relies on this, so it must not drift silently.
        let cli = Cli::try_parse_from(["vds-agent"]).expect("parses");
        assert_eq!(cli.config, PathBuf::from(DEFAULT_CONFIG));
    }

    #[test]
    fn an_unknown_flag_is_refused_rather_than_ignored() {
        assert!(Cli::try_parse_from(["vds-agent", "--danger"]).is_err());
    }

    #[test]
    fn a_missing_configuration_file_is_a_startup_error_not_a_panic() {
        let cli = Cli {
            config: PathBuf::from("/nowhere/agent.toml"),
            check: true,
            fingerprint: false,
        };

        let err = run(cli).expect_err("must fail");
        assert!(matches!(err, StartupError::Config(_)));
        assert!(err.to_string().contains("agent.toml"), "{err}");
    }

    #[test]
    fn the_default_port_agrees_with_the_one_the_app_offers() {
        // The domain carries its own copy, because it depends on nothing — including this
        // protocol crate. This test is the join between them: the two are only allowed to
        // be separate constants for as long as they are the same number.
        assert_eq!(
            vds_agent_protocol::DEFAULT_AGENT_PORT,
            vds_domain::server::DEFAULT_AGENT_PORT
        );
    }

    #[test]
    fn the_hostname_is_never_empty() {
        // It goes into a log line and into `/v1/info`; an empty string there reads as a
        // bug in the app.
        assert!(!hostname().trim().is_empty());
    }

    #[test]
    fn logging_can_be_initialised_more_than_once() {
        init_logging("debug");
        init_logging("info");
    }
}
