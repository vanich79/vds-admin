//! The agent's configuration file.
//!
//! One TOML file, conventionally `/etc/vds-agent/agent.toml`, written by the installer.
//! Every field has a default so that a minimal file — just a token — starts a working
//! agent, and so that adding a field in a later version cannot break an existing install.
//!
//! ## The token
//!
//! The bearer token is the only thing standing between the network and a full reading of
//! the host, so it gets special handling:
//!
//! * it is read from a separate file (`token_file`) by preference, so the main config can
//!   be world-readable while the token is `0600`;
//! * [`AgentConfig`] has a hand-written [`std::fmt::Debug`] that never prints it;
//! * a token shorter than [`MIN_TOKEN_LEN`] is refused at startup rather than accepted
//!   and quietly brute-forced later.

use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use vds_agent_protocol::DEFAULT_AGENT_PORT;

/// Shortest token the agent will start with.
///
/// 32 characters of the installer's base64 alphabet is ~192 bits. The limit exists to
/// stop `token = "test"` reaching production, which is the failure mode that actually
/// happens.
pub const MIN_TOKEN_LEN: usize = 32;

/// Default seconds a collected report is served before the host is read again.
pub const DEFAULT_CACHE_TTL_SECS: u64 = 5;

/// Why the agent will not start.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid TOML: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("no bearer token is configured; set `token_file` or `token` in the configuration")]
    MissingToken,
    #[error(
        "the bearer token is too short: {len} characters, minimum {MIN_TOKEN_LEN}. \
         Generate one with `head -c 32 /dev/urandom | base64`"
    )]
    WeakToken { len: usize },
    #[error("`{field}` must not be {value}")]
    InvalidValue { field: &'static str, value: String },
    #[error("`tls_certificate` and `tls_private_key` must be set together")]
    IncompleteTls,
}

/// The parsed configuration.
///
/// Deliberately not `Clone`: one owner, so the token is not copied around.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Address to bind. Defaults to every interface; set to `127.0.0.1` when the app
    /// reaches the agent through a tunnel.
    #[serde(default = "default_bind")]
    pub bind: IpAddr,

    #[serde(default = "default_port")]
    pub port: u16,

    /// Bearer token, inline. Prefer `token_file`.
    ///
    /// Read it through [`AgentConfig::token`] rather than directly. What actually keeps
    /// it out of the logs is the hand-written [`fmt::Debug`] below, not this field's
    /// visibility.
    #[serde(default)]
    pub(crate) token: Option<String>,

    /// File containing the bearer token. Takes precedence over `token`.
    #[serde(default)]
    pub token_file: Option<PathBuf>,

    /// PEM certificate chain. Generated on first start when absent.
    #[serde(default)]
    pub tls_certificate: Option<PathBuf>,

    /// PEM private key matching `tls_certificate`.
    #[serde(default)]
    pub tls_private_key: Option<PathBuf>,

    /// Where a generated certificate and key are written.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,

    /// How long a collected report is reused before the host is read again.
    ///
    /// The app polls on its own schedule and several app instances may watch one host;
    /// without this the agent would re-read `/proc` once per client per poll.
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_secs: u64,

    /// Per-command timeout for the collector layer.
    #[serde(default = "default_collect_timeout")]
    pub collect_timeout_secs: u64,

    /// Log level: `trace`, `debug`, `info`, `warn` or `error`.
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Collect Docker containers when Docker is present.
    #[serde(default = "yes")]
    pub collect_docker: bool,

    /// Collect systemd units when systemd is present.
    #[serde(default = "yes")]
    pub collect_services: bool,

    /// Collect the process table.
    #[serde(default = "yes")]
    pub collect_processes: bool,
}

fn default_bind() -> IpAddr {
    IpAddr::from([0, 0, 0, 0])
}

fn default_port() -> u16 {
    DEFAULT_AGENT_PORT
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("/var/lib/vds-agent")
}

fn default_cache_ttl() -> u64 {
    DEFAULT_CACHE_TTL_SECS
}

fn default_collect_timeout() -> u64 {
    10
}

fn default_log_level() -> String {
    "info".to_owned()
}

fn yes() -> bool {
    true
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            port: default_port(),
            token: None,
            token_file: None,
            tls_certificate: None,
            tls_private_key: None,
            state_dir: default_state_dir(),
            cache_ttl_secs: default_cache_ttl(),
            collect_timeout_secs: default_collect_timeout(),
            log_level: default_log_level(),
            collect_docker: true,
            collect_services: true,
            collect_processes: true,
        }
    }
}

/// Prints everything except the token.
///
/// Hand-written rather than `#[derive]` precisely so that a future field cannot be added
/// to the struct and start appearing in logs by accident: adding one here is a conscious
/// act.
impl fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentConfig")
            .field("bind", &self.bind)
            .field("port", &self.port)
            .field("token", &"<redacted>")
            .field("token_file", &self.token_file)
            .field("tls_certificate", &self.tls_certificate)
            .field("tls_private_key", &self.tls_private_key)
            .field("state_dir", &self.state_dir)
            .field("cache_ttl_secs", &self.cache_ttl_secs)
            .field("collect_timeout_secs", &self.collect_timeout_secs)
            .field("log_level", &self.log_level)
            .field("collect_docker", &self.collect_docker)
            .field("collect_services", &self.collect_services)
            .field("collect_processes", &self.collect_processes)
            .finish()
    }
}

impl AgentConfig {
    /// Reads a configuration file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;

        let mut config: AgentConfig =
            toml::from_str(&raw).map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            })?;

        config.resolve_token()?;
        config.validate()?;
        Ok(config)
    }

    /// Reads the token file, if one is configured.
    ///
    /// The file wins over the inline value: an operator who moved the token out of the
    /// main file expects the copy they left behind to stop mattering.
    fn resolve_token(&mut self) -> Result<(), ConfigError> {
        let Some(path) = self.token_file.clone() else {
            return Ok(());
        };

        let raw = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        self.token = Some(raw.trim().to_owned());
        Ok(())
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let token = self.token.as_deref().unwrap_or_default();
        if token.is_empty() {
            return Err(ConfigError::MissingToken);
        }
        if token.chars().count() < MIN_TOKEN_LEN {
            return Err(ConfigError::WeakToken {
                len: token.chars().count(),
            });
        }

        if self.port == 0 {
            return Err(ConfigError::InvalidValue {
                field: "port",
                value: "0".to_owned(),
            });
        }
        if self.collect_timeout_secs == 0 {
            return Err(ConfigError::InvalidValue {
                field: "collect_timeout_secs",
                value: "0".to_owned(),
            });
        }
        if self.tls_certificate.is_some() != self.tls_private_key.is_some() {
            return Err(ConfigError::IncompleteTls);
        }
        Ok(())
    }

    /// The bearer token.
    pub fn token(&self) -> &str {
        self.token.as_deref().unwrap_or_default()
    }

    /// Where the generated certificate lives when none is configured.
    pub fn certificate_path(&self) -> PathBuf {
        self.tls_certificate
            .clone()
            .unwrap_or_else(|| self.state_dir.join("agent.crt"))
    }

    /// Where the generated key lives when none is configured.
    pub fn private_key_path(&self) -> PathBuf {
        self.tls_private_key
            .clone()
            .unwrap_or_else(|| self.state_dir.join("agent.key"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token long enough to pass validation.
    const GOOD_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn write(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, contents).unwrap_or_default();
        path
    }

    #[test]
    fn a_minimal_file_yields_working_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(&dir, "agent.toml", &format!("token = \"{GOOD_TOKEN}\""));

        let config = AgentConfig::load(&path).expect("loads");
        assert_eq!(config.port, DEFAULT_AGENT_PORT);
        assert_eq!(config.bind, IpAddr::from([0, 0, 0, 0]));
        assert_eq!(config.cache_ttl_secs, DEFAULT_CACHE_TTL_SECS);
        assert!(config.collect_docker);
    }

    #[test]
    fn a_token_file_takes_precedence_over_an_inline_token() {
        // An operator who moved the token out expects the leftover copy to stop counting.
        let dir = tempfile::tempdir().expect("temp dir");
        let token_path = write(&dir, "token", "  from-the-file-0123456789abcdef01  \n");
        let path = write(
            &dir,
            "agent.toml",
            &format!(
                "token = \"{GOOD_TOKEN}\"\ntoken_file = \"{}\"",
                token_path.display().to_string().replace('\\', "\\\\")
            ),
        );

        let config = AgentConfig::load(&path).expect("loads");
        assert_eq!(config.token(), "from-the-file-0123456789abcdef01");
    }

    #[test]
    fn a_missing_token_refuses_to_start() {
        // Starting without one would serve a full reading of the host to anyone who asks.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(&dir, "agent.toml", "port = 9443");

        assert!(matches!(
            AgentConfig::load(&path),
            Err(ConfigError::MissingToken)
        ));
    }

    #[test]
    fn a_short_token_refuses_to_start_and_says_how_to_make_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(&dir, "agent.toml", "token = \"test\"");

        let err = AgentConfig::load(&path).expect_err("must refuse");
        assert!(matches!(err, ConfigError::WeakToken { len: 4 }));
        assert!(err.to_string().contains("/dev/urandom"), "{err}");
    }

    #[test]
    fn a_missing_token_file_is_reported_with_its_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(&dir, "agent.toml", "token_file = \"/nowhere/at/all\"");

        let err = AgentConfig::load(&path).expect_err("must refuse");
        assert!(err.to_string().contains("/nowhere/at/all"), "{err}");
    }

    #[test]
    fn an_unknown_key_is_refused_rather_than_silently_ignored() {
        // A typo in a security-relevant setting must not look like it took effect.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(
            &dir,
            "agent.toml",
            &format!("token = \"{GOOD_TOKEN}\"\ncollect_dokcer = false"),
        );

        assert!(matches!(
            AgentConfig::load(&path),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn half_configured_tls_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(
            &dir,
            "agent.toml",
            &format!("token = \"{GOOD_TOKEN}\"\ntls_certificate = \"/etc/ssl/a.crt\""),
        );

        assert!(matches!(
            AgentConfig::load(&path),
            Err(ConfigError::IncompleteTls)
        ));
    }

    #[test]
    fn a_zero_port_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(
            &dir,
            "agent.toml",
            &format!("token = \"{GOOD_TOKEN}\"\nport = 0"),
        );

        assert!(matches!(
            AgentConfig::load(&path),
            Err(ConfigError::InvalidValue { field: "port", .. })
        ));
    }

    #[test]
    fn the_debug_rendering_never_contains_the_token() {
        // The config is logged at startup; this is what stops the token going with it.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(&dir, "agent.toml", &format!("token = \"{GOOD_TOKEN}\""));

        let config = AgentConfig::load(&path).expect("loads");
        let rendered = format!("{config:?}");

        assert!(!rendered.contains(GOOD_TOKEN), "Debug leaked the token");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("port"), "the rest should still be useful");
    }

    #[test]
    fn certificate_paths_fall_back_to_the_state_directory() {
        let config = AgentConfig::default();
        assert!(config.certificate_path().ends_with("agent.crt"));
        assert!(config.private_key_path().ends_with("agent.key"));
        assert!(config.certificate_path().starts_with("/var/lib/vds-agent"));
    }

    #[test]
    fn configured_certificate_paths_are_used_verbatim() {
        let config = AgentConfig {
            tls_certificate: Some(PathBuf::from("/etc/ssl/agent.pem")),
            tls_private_key: Some(PathBuf::from("/etc/ssl/agent.key")),
            ..Default::default()
        };
        assert_eq!(
            config.certificate_path(),
            PathBuf::from("/etc/ssl/agent.pem")
        );
        assert_eq!(
            config.private_key_path(),
            PathBuf::from("/etc/ssl/agent.key")
        );
    }

    #[test]
    fn a_malformed_file_names_itself_in_the_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write(&dir, "agent.toml", "token = ");

        let err = AgentConfig::load(&path).expect_err("must refuse");
        assert!(err.to_string().contains("agent.toml"), "{err}");
    }
}
