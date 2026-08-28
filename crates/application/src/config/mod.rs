//! Centralised configuration.
//!
//! One typed tree, one loader, one validator. Configuration is never read ad hoc from
//! scattered places; anything that needs a setting takes it from here.
//!
//! Every settings file carries a `version`, and [`migrate`] brings older files forward,
//! so an upgrade never silently discards a user's tuning or refuses to start.

mod migration;

pub use migration::{MigrationOutcome, migrate};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use vds_domain::screenshot::ScreenshotRefreshPolicy;
use vds_domain::server::MonitoringThresholds;

/// Current settings schema version.
pub const CONFIG_VERSION: u32 = 1;

/// The whole application configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    /// Schema version, used by [`migrate`].
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub application: ApplicationSettings,
    #[serde(default)]
    pub monitoring: MonitoringSettings,
    #[serde(default)]
    pub analytics: AnalyticsSettings,
    #[serde(default)]
    pub screenshots: ScreenshotSettings,
    #[serde(default)]
    pub notifications: NotificationSettings,
    #[serde(default)]
    pub storage: StorageSettings,
    #[serde(default)]
    pub logging: LoggingSettings,
}

fn default_version() -> u32 {
    CONFIG_VERSION
}

impl Default for Configuration {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            application: ApplicationSettings::default(),
            monitoring: MonitoringSettings::default(),
            analytics: AnalyticsSettings::default(),
            screenshots: ScreenshotSettings::default(),
            notifications: NotificationSettings::default(),
            storage: StorageSettings::default(),
            logging: LoggingSettings::default(),
        }
    }
}

impl Configuration {
    /// Parses TOML, applying migrations first.
    pub fn from_toml(text: &str) -> Result<(Configuration, MigrationOutcome), ConfigError> {
        let raw: toml::Value =
            toml::from_str(text).map_err(|e| ConfigError::Malformed(e.to_string()))?;
        let (migrated, outcome) = migrate(raw)?;
        let config: Configuration = migrated
            .try_into()
            .map_err(|e: toml::de::Error| ConfigError::Malformed(e.to_string()))?;
        config.validate()?;
        Ok((config, outcome))
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::Serialise(e.to_string()))
    }

    /// Checks every invariant the rest of the system relies on.
    ///
    /// Validation happens once, here, rather than being re-checked defensively at every
    /// use site.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version > CONFIG_VERSION {
            return Err(ConfigError::FromTheFuture {
                found: self.version,
                supported: CONFIG_VERSION,
            });
        }
        self.monitoring.validate()?;
        self.analytics.validate()?;
        self.screenshots.validate()?;
        self.storage.validate()?;
        self.logging.validate()?;
        Ok(())
    }
}

/// General application behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApplicationSettings {
    pub theme: Theme,
    /// Start minimised to the tray/background.
    pub start_minimised: bool,
    /// Show the debug panel in the UI.
    pub debug_mode: bool,
    /// Two-letter language code, or `"system"`.
    pub language: String,
}

impl Default for ApplicationSettings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            start_minimised: false,
            debug_mode: false,
            language: "system".to_owned(),
        }
    }
}

/// Colour scheme preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    Dark,
    /// Follow the operating system.
    System,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
            Theme::System => "system",
        }
    }

    pub fn parse(raw: &str) -> Option<Theme> {
        match raw {
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            "system" => Some(Theme::System),
            _ => None,
        }
    }
}

/// Monitoring cadence and limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MonitoringSettings {
    /// Default poll interval for new servers, in seconds.
    pub default_server_interval_secs: u32,
    /// Default poll interval for new websites, in seconds.
    pub default_website_interval_secs: u32,
    /// Consecutive failures before a subject is declared offline.
    pub offline_after_failures: u32,
    /// Per-check timeout, in seconds.
    pub timeout_secs: u32,
    /// Ceiling on simultaneous server collections.
    pub max_concurrent_servers: usize,
    /// Ceiling on simultaneous website checks.
    pub max_concurrent_websites: usize,
    /// Default thresholds applied to new servers.
    pub thresholds: MonitoringThresholds,
    /// Collect processes, Docker and systemd as well as core metrics.
    pub collect_extended: bool,
}

impl Default for MonitoringSettings {
    fn default() -> Self {
        Self {
            default_server_interval_secs: vds_domain::server::DEFAULT_POLL_INTERVAL_SECS,
            default_website_interval_secs: vds_domain::website::DEFAULT_WEBSITE_POLL_INTERVAL_SECS,
            offline_after_failures: vds_domain::server::DEFAULT_OFFLINE_AFTER_FAILURES,
            timeout_secs: vds_domain::server::DEFAULT_TIMEOUT_SECS,
            max_concurrent_servers: 16,
            max_concurrent_websites: 32,
            thresholds: MonitoringThresholds::default(),
            collect_extended: true,
        }
    }
}

impl MonitoringSettings {
    pub fn validate(&self) -> Result<(), ConfigError> {
        require(
            self.default_server_interval_secs > 0,
            "monitoring.default_server_interval_secs",
        )?;
        require(
            self.default_website_interval_secs > 0,
            "monitoring.default_website_interval_secs",
        )?;
        require(
            self.offline_after_failures > 0,
            "monitoring.offline_after_failures",
        )?;
        require(self.timeout_secs > 0, "monitoring.timeout_secs")?;
        require(
            self.max_concurrent_servers > 0,
            "monitoring.max_concurrent_servers",
        )?;
        require(
            self.max_concurrent_websites > 0,
            "monitoring.max_concurrent_websites",
        )?;

        for threshold in [
            self.thresholds.cpu,
            self.thresholds.memory,
            self.thresholds.disk,
            self.thresholds.swap,
            self.thresholds.load_per_core,
            self.thresholds.temperature,
        ] {
            if !threshold.is_coherent() {
                return Err(ConfigError::Invalid {
                    field: "monitoring.thresholds",
                    reason: "warning and critical values are inverted for the direction".to_owned(),
                });
            }
        }
        Ok(())
    }
}

/// Analytics refresh behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnalyticsSettings {
    /// Minutes between provider refreshes.
    pub refresh_interval_mins: u32,
    /// Ceiling on simultaneous provider calls.
    pub max_concurrent_requests: usize,
    /// Requests per minute allowed per provider.
    pub rate_limit_per_minute: u32,
    /// Percentage drop that counts as a traffic anomaly.
    pub anomaly_threshold_percent: u32,
    /// Enable the traffic anomaly detector.
    pub anomaly_detection: bool,
}

impl Default for AnalyticsSettings {
    fn default() -> Self {
        Self {
            refresh_interval_mins: vds_domain::analytics::DEFAULT_ANALYTICS_REFRESH_MINS,
            max_concurrent_requests: 4,
            rate_limit_per_minute: 30,
            anomaly_threshold_percent: 30,
            anomaly_detection: true,
        }
    }
}

impl AnalyticsSettings {
    pub fn validate(&self) -> Result<(), ConfigError> {
        require(
            self.refresh_interval_mins > 0,
            "analytics.refresh_interval_mins",
        )?;
        require(
            self.max_concurrent_requests > 0,
            "analytics.max_concurrent_requests",
        )?;
        require(
            self.rate_limit_per_minute > 0,
            "analytics.rate_limit_per_minute",
        )?;
        if self.anomaly_threshold_percent == 0 || self.anomaly_threshold_percent > 100 {
            return Err(ConfigError::Invalid {
                field: "analytics.anomaly_threshold_percent",
                reason: "must be between 1 and 100".to_owned(),
            });
        }
        Ok(())
    }
}

/// Screenshot capture behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScreenshotSettings {
    pub enabled: bool,
    pub refresh_policy: ScreenshotRefreshPolicy,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub thumbnail_max_edge: u32,
    pub timeout_secs: u32,
    /// Simultaneous captures. Deliberately tiny: a browser is heavy.
    pub max_concurrent: usize,
    /// Explicit path to a Chromium-family browser, when auto-detection fails.
    pub browser_path: Option<PathBuf>,
}

impl Default for ScreenshotSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            refresh_policy: ScreenshotRefreshPolicy::default(),
            viewport_width: vds_domain::screenshot::DEFAULT_VIEWPORT_WIDTH,
            viewport_height: vds_domain::screenshot::DEFAULT_VIEWPORT_HEIGHT,
            thumbnail_max_edge: vds_domain::screenshot::DEFAULT_THUMBNAIL_MAX_EDGE,
            timeout_secs: vds_domain::screenshot::DEFAULT_CAPTURE_TIMEOUT_SECS,
            max_concurrent: 2,
            browser_path: None,
        }
    }
}

impl ScreenshotSettings {
    pub fn validate(&self) -> Result<(), ConfigError> {
        require(self.viewport_width >= 320, "screenshots.viewport_width")?;
        require(self.viewport_height >= 240, "screenshots.viewport_height")?;
        require(
            self.thumbnail_max_edge >= 64,
            "screenshots.thumbnail_max_edge",
        )?;
        require(self.timeout_secs > 0, "screenshots.timeout_secs")?;
        require(self.max_concurrent > 0, "screenshots.max_concurrent")?;
        if self.thumbnail_max_edge > self.viewport_width {
            return Err(ConfigError::Invalid {
                field: "screenshots.thumbnail_max_edge",
                reason: "a thumbnail larger than the capture is not a thumbnail".to_owned(),
            });
        }
        Ok(())
    }
}

/// Notification delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotificationSettings {
    pub desktop_enabled: bool,
    pub sound_enabled: bool,
    /// Webhook endpoint; empty disables the webhook provider.
    pub webhook_url: Option<String>,
    /// Suppress notifications below this severity.
    pub min_severity: String,
    /// Seconds between repeat notifications for the same open incident.
    pub renotify_after_secs: u32,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            desktop_enabled: true,
            sound_enabled: false,
            webhook_url: None,
            min_severity: "warning".to_owned(),
            renotify_after_secs: vds_domain::alerts::DEFAULT_RENOTIFY_SECS,
        }
    }
}

/// Where data lives and how long it is kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageSettings {
    /// Database path. `None` means the platform default data directory.
    pub database_path: Option<PathBuf>,
    /// Screenshot cache directory.
    pub screenshot_dir: Option<PathBuf>,
    pub retention: RetentionSettings,
}

impl StorageSettings {
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.retention.validate()
    }
}

/// Retention windows per storage tier, in days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetentionSettings {
    pub raw_days: u32,
    pub five_minute_days: u32,
    pub hourly_days: u32,
    /// Daily rollups. Zero means keep forever.
    pub daily_days: u32,
    pub events_days: u32,
    pub incidents_days: u32,
    pub website_checks_days: u32,
    pub analytics_days: u32,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            raw_days: 7,
            five_minute_days: 30,
            hourly_days: 365,
            daily_days: 0,
            events_days: 90,
            incidents_days: 365,
            website_checks_days: 30,
            analytics_days: 400,
        }
    }
}

impl RetentionSettings {
    pub fn validate(&self) -> Result<(), ConfigError> {
        require(self.raw_days > 0, "storage.retention.raw_days")?;
        require(
            self.five_minute_days > 0,
            "storage.retention.five_minute_days",
        )?;
        require(self.hourly_days > 0, "storage.retention.hourly_days")?;
        require(self.events_days > 0, "storage.retention.events_days")?;

        // Each tier exists to answer queries the finer tier no longer can, so a coarser
        // tier retained for less time than a finer one leaves a hole in the history.
        if self.five_minute_days < self.raw_days {
            return Err(ConfigError::Invalid {
                field: "storage.retention.five_minute_days",
                reason: "must not be shorter than raw retention, or history gains a gap".to_owned(),
            });
        }
        if self.hourly_days < self.five_minute_days {
            return Err(ConfigError::Invalid {
                field: "storage.retention.hourly_days",
                reason: "must not be shorter than five-minute retention".to_owned(),
            });
        }
        if self.daily_days != 0 && self.daily_days < self.hourly_days {
            return Err(ConfigError::Invalid {
                field: "storage.retention.daily_days",
                reason: "must not be shorter than hourly retention (0 means keep forever)"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

/// Logging behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingSettings {
    /// `trace` | `debug` | `info` | `warn` | `error`.
    pub level: String,
    /// Write logs to a rotating file as well as stderr.
    pub file_enabled: bool,
    pub directory: Option<PathBuf>,
    /// Rotated files to keep.
    pub max_files: u32,
    /// Emit JSON rather than human-readable lines.
    pub json: bool,
    /// Redact secret-shaped values from log records.
    ///
    /// A second line of defence behind the type-level protections; disabling it is a
    /// deliberate, visible act.
    pub redact_secrets: bool,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            level: "info".to_owned(),
            file_enabled: true,
            directory: None,
            max_files: 7,
            json: false,
            redact_secrets: true,
        }
    }
}

impl LoggingSettings {
    pub const LEVELS: &'static [&'static str] = &["trace", "debug", "info", "warn", "error"];

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !Self::LEVELS.contains(&self.level.as_str()) {
            return Err(ConfigError::Invalid {
                field: "logging.level",
                reason: format!("expected one of {:?}, found {:?}", Self::LEVELS, self.level),
            });
        }
        require(self.max_files > 0, "logging.max_files")?;
        Ok(())
    }
}

/// Why configuration was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("configuration file is malformed: {0}")]
    Malformed(String),
    #[error("could not serialise configuration: {0}")]
    Serialise(String),
    #[error("{field} is invalid: {reason}")]
    Invalid { field: &'static str, reason: String },
    #[error(
        "configuration version {found} was written by a newer version of the application \
         (this build understands up to {supported})"
    )]
    FromTheFuture { found: u32, supported: u32 },
    #[error("cannot migrate configuration from version {0}")]
    UnknownVersion(u32),
}

fn require(condition: bool, field: &'static str) -> Result<(), ConfigError> {
    if condition {
        Ok(())
    } else {
        Err(ConfigError::Invalid {
            field,
            reason: "must be greater than zero".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_configuration_is_valid() {
        assert_eq!(Configuration::default().validate(), Ok(()));
    }

    #[test]
    fn defaults_round_trip_through_toml() {
        let original = Configuration::default();
        let text = original.to_toml().expect("serialises");
        let (parsed, _) = Configuration::from_toml(&text).expect("parses");
        assert_eq!(parsed, original);
    }

    #[test]
    fn an_empty_file_yields_the_defaults() {
        // A user who deletes their settings file must get a working application, not an
        // error.
        let (config, _) = Configuration::from_toml("").expect("parses");
        assert_eq!(config, Configuration::default());
    }

    #[test]
    fn a_partial_file_fills_in_the_rest() {
        let text = r#"
version = 1
[monitoring]
default_server_interval_secs = 15
"#;
        let (config, _) = Configuration::from_toml(text).expect("parses");
        assert_eq!(config.monitoring.default_server_interval_secs, 15);
        assert_eq!(config.monitoring.max_concurrent_servers, 16);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn a_typo_in_a_key_is_reported_rather_than_silently_ignored() {
        // Silently ignoring `intervall_secs` would leave the user convinced they had
        // changed something.
        let text = "[monitoring]\ndefault_server_intervall_secs = 15\n";
        let err = Configuration::from_toml(text).expect_err("must fail");
        assert!(matches!(err, ConfigError::Malformed(_)), "got {err:?}");
    }

    #[test]
    fn a_configuration_from_a_newer_version_is_refused_clearly() {
        let text = format!("version = {}\n", CONFIG_VERSION + 5);
        let err = Configuration::from_toml(&text).expect_err("must fail");
        assert!(
            matches!(err, ConfigError::FromTheFuture { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn zero_intervals_are_rejected() {
        let mut config = Configuration::default();
        config.monitoring.default_server_interval_secs = 0;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::Invalid {
                field: "monitoring.default_server_interval_secs",
                ..
            })
        ));
    }

    #[test]
    fn zero_concurrency_is_rejected_because_it_would_stall_monitoring_forever() {
        let mut config = Configuration::default();
        config.monitoring.max_concurrent_servers = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn retention_tiers_must_not_leave_a_hole_in_history() {
        // Keeping raw data for 30 days but five-minute rollups for 7 means that between
        // day 7 and day 30 there is nothing to draw.
        let mut config = Configuration::default();
        config.storage.retention.raw_days = 30;
        config.storage.retention.five_minute_days = 7;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::Invalid {
                field: "storage.retention.five_minute_days",
                ..
            })
        ));
    }

    #[test]
    fn daily_retention_of_zero_means_forever_and_is_allowed() {
        let mut config = Configuration::default();
        config.storage.retention.daily_days = 0;
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn an_unknown_log_level_is_rejected_with_the_valid_options() {
        let mut config = Configuration::default();
        config.logging.level = "verbose".to_owned();
        let err = config.validate().expect_err("must fail");
        assert!(err.to_string().contains("trace"), "message was: {err}");
    }

    #[test]
    fn a_thumbnail_larger_than_the_capture_is_rejected() {
        let mut config = Configuration::default();
        config.screenshots.thumbnail_max_edge = 4_000;
        assert!(config.validate().is_err());
    }

    #[test]
    fn inverted_thresholds_are_rejected() {
        let mut config = Configuration::default();
        config.monitoring.thresholds.cpu = vds_domain::Threshold::above(95.0, 80.0);
        assert!(matches!(
            config.validate(),
            Err(ConfigError::Invalid {
                field: "monitoring.thresholds",
                ..
            })
        ));
    }

    #[test]
    fn anomaly_thresholds_must_be_percentages() {
        let mut config = Configuration::default();
        config.analytics.anomaly_threshold_percent = 0;
        assert!(config.validate().is_err());

        config.analytics.anomaly_threshold_percent = 101;
        assert!(config.validate().is_err());

        config.analytics.anomaly_threshold_percent = 35;
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn themes_round_trip() {
        for theme in [Theme::Light, Theme::Dark, Theme::System] {
            assert_eq!(Theme::parse(theme.as_str()), Some(theme));
        }
        assert_eq!(Theme::parse("solarized"), None);
    }
}
