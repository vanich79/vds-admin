//! Assembling the application from its parts.

use crate::paths::AppPaths;
use std::sync::Arc;
use vds_application::alerts::{AlertService, NotificationDispatcher};
use vds_application::analytics::{
    AnalyticsService, AnomalyConfig, ProviderRegistry, TrafficAnomalyDetector,
};
use vds_application::config::Configuration;
use vds_application::dashboard::DashboardQueryService;
use vds_application::files::FileService;
use vds_application::metrics::{MetricsAggregationService, RetentionService};
use vds_application::monitoring::{ServerMonitor, WebsiteMonitor};
use vds_application::provisioning::ProvisioningService;
use vds_application::scheduler::{ConcurrencyLimits, RateLimitManager, Scheduler};
use vds_application::screenshots::{ScreenshotService, ScreenshotStore};
use vds_domain::Status;
use vds_domain::ports::{
    AlertRepository, AnalyticsProvider, AnalyticsRepository, Clock, EventPublisher,
    EventRepository, FileBrowser, MetricsRepository, NotificationProvider, ScreenshotProvider,
    ScreenshotRepository, SecretStore, ServerProbe, ServerRepository, SystemClock,
    WebsiteRepository,
};
use vds_infra_analytics::YandexMetricaProvider;
use vds_infra_collectors::CollectorRegistry;
use vds_infra_db::{
    Database, SqliteAlertRepository, SqliteAnalyticsRepository, SqliteEventRepository,
    SqliteMetricsRepository, SqliteScreenshotRepository, SqliteServerRepository,
    SqliteWebsiteRepository,
};
use vds_infra_notify::{DesktopNotificationProvider, WebhookNotificationProvider};
use vds_infra_screenshot::{ChromiumScreenshotProvider, FilesystemScreenshotStore};
use vds_infra_secrets::{
    EncryptedFileStore, FileSecretStore, OsKeyringStore, ResolvedSecretStore, SecretBackend,
};
use vds_infra_ssh::{KnownHosts, SshServerProbe};
use vds_infra_web::HttpWebsiteChecker;

/// Why the application could not be assembled.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    #[error("could not prepare {0}")]
    Filesystem(String),
    #[error("configuration: {0}")]
    Configuration(#[from] vds_application::config::ConfigError),
    #[error("storage: {0}")]
    Storage(#[from] vds_domain::ports::RepositoryError),
    #[error("credential storage: {0}")]
    Secrets(String),
    #[error("website checker: {0}")]
    WebsiteChecker(String),
    #[error("analytics: {0}")]
    Analytics(String),
    #[error("notifications: {0}")]
    Notifications(String),
    #[error("SSH: {0}")]
    Ssh(String),
}

/// How credential storage should be set up.
///
/// The passphrase is only consulted if the OS keystore turns out to be unusable, so the
/// common case never asks the user for one.
pub enum SecretsSetup {
    /// Use the OS keystore, falling back to an encrypted file with this passphrase.
    Automatic { fallback_passphrase: String },
    /// Force the encrypted file, whatever the platform offers.
    EncryptedFile { passphrase: String },
    /// Use a store the caller already built. For tests.
    Provided(Arc<dyn SecretStore>),
}

/// Everything wired together.
///
/// Public fields on purpose: this is a bag of dependencies for the presentation layer to
/// reach into, not an abstraction of its own. Adding one more indirection here would buy
/// nothing.
pub struct Application {
    pub paths: AppPaths,
    pub configuration: Configuration,
    pub clock: Arc<dyn Clock>,

    pub servers: Arc<dyn ServerRepository>,
    pub websites: Arc<dyn WebsiteRepository>,
    pub metrics: Arc<dyn MetricsRepository>,
    pub analytics_repository: Arc<dyn AnalyticsRepository>,
    pub alerts_repository: Arc<dyn AlertRepository>,
    pub events_repository: Arc<dyn EventRepository>,
    pub screenshots_repository: Arc<dyn ScreenshotRepository>,
    pub secrets: Arc<dyn SecretStore>,
    pub secret_backend: SecretBackend,

    pub server_monitor: Arc<ServerMonitor>,
    pub website_monitor: Arc<WebsiteMonitor>,
    pub analytics: Arc<AnalyticsService>,
    pub screenshots: Arc<ScreenshotService>,
    pub alert_service: Arc<AlertService>,
    pub aggregation: Arc<MetricsAggregationService>,
    pub retention: Arc<RetentionService>,
    pub dashboard: Arc<DashboardQueryService>,
    pub provisioning: Arc<ProvisioningService>,
    pub files: Arc<FileService>,
    pub scheduler: Arc<Scheduler>,

    pub known_hosts: Arc<KnownHosts>,
    pub database: Database,
}

impl Application {
    /// Builds the whole application.
    pub async fn assemble(
        paths: AppPaths,
        configuration: Configuration,
        events: Arc<dyn EventPublisher>,
        secrets_setup: SecretsSetup,
    ) -> Result<Self, ApplicationError> {
        paths
            .ensure()
            .map_err(|e| ApplicationError::Filesystem(e.to_string()))?;

        let clock: Arc<dyn Clock> = Arc::new(SystemClock);

        // --- storage ---
        let database_path = configuration
            .storage
            .database_path
            .clone()
            .unwrap_or_else(|| paths.database.clone());
        let database = Database::open(&database_path).await?;

        let servers: Arc<dyn ServerRepository> =
            Arc::new(SqliteServerRepository::new(database.clone()));
        let websites: Arc<dyn WebsiteRepository> =
            Arc::new(SqliteWebsiteRepository::new(database.clone()));
        let metrics: Arc<dyn MetricsRepository> =
            Arc::new(SqliteMetricsRepository::new(database.clone()));
        let analytics_repository: Arc<dyn AnalyticsRepository> =
            Arc::new(SqliteAnalyticsRepository::new(database.clone()));
        let alerts_repository: Arc<dyn AlertRepository> =
            Arc::new(SqliteAlertRepository::new(database.clone()));
        let events_repository: Arc<dyn EventRepository> =
            Arc::new(SqliteEventRepository::new(database.clone()));
        let screenshots_repository: Arc<dyn ScreenshotRepository> =
            Arc::new(SqliteScreenshotRepository::new(database.clone()));

        // --- credentials ---
        let (secrets, secret_backend) = resolve_secrets(&paths, secrets_setup).await?;

        // --- monitoring ---
        let known_hosts = Arc::new(
            KnownHosts::load(&paths.known_hosts)
                .map_err(|e| ApplicationError::Ssh(e.to_string()))?,
        );

        let registry = if configuration.monitoring.collect_extended {
            CollectorRegistry::linux()
        } else {
            CollectorRegistry::essential()
        };

        // One object, two roles. The probe owns the pool of SSH sessions, and the file
        // browser reuses it rather than opening a second connection per server: doubling
        // the handshakes is how a monitoring tool gets its own address banned by fail2ban.
        let ssh = Arc::new(SshServerProbe::new(
            registry,
            Arc::clone(&secrets),
            Arc::clone(&known_hosts),
        ));
        let probe: Arc<dyn ServerProbe> = Arc::clone(&ssh) as Arc<dyn ServerProbe>;
        let file_browser: Arc<dyn FileBrowser> = Arc::clone(&ssh) as Arc<dyn FileBrowser>;

        let files = Arc::new(FileService::new(
            file_browser,
            Arc::clone(&servers),
            Arc::clone(&events),
        ));

        let server_monitor = Arc::new(ServerMonitor::new(
            probe,
            Arc::clone(&servers),
            Arc::clone(&metrics),
            Arc::clone(&events),
            Arc::clone(&clock),
        ));

        let checker = Arc::new(
            HttpWebsiteChecker::new(user_agent())
                .map_err(|e| ApplicationError::WebsiteChecker(e.to_string()))?,
        );
        let website_monitor = Arc::new(WebsiteMonitor::new(
            checker,
            Arc::clone(&websites),
            Arc::clone(&metrics),
            Arc::clone(&events),
            Arc::clone(&clock),
        ));

        // --- analytics ---
        let mut providers = ProviderRegistry::new();
        // Adding Google Analytics is one more line here. That is the whole point.
        match YandexMetricaProvider::new(Arc::clone(&secrets)) {
            Ok(provider) => providers.register(Arc::new(provider) as Arc<dyn AnalyticsProvider>),
            Err(err) => {
                // A provider that cannot be built must not stop the application: every
                // other feature still works, and the UI shows the provider as absent.
                tracing::warn!(error = %err, "the Yandex.Metrica provider is unavailable");
            }
        }

        #[cfg(feature = "demo-providers")]
        {
            // Registered, not selected. The registry is keyed by provider id, so an
            // integration has to name `demo` explicitly before a fabricated number can
            // reach a screen — being compiled in is not enough.
            providers.register(
                Arc::new(vds_infra_analytics::demo::DemoAnalyticsProvider::new())
                    as Arc<dyn AnalyticsProvider>,
            );
            tracing::warn!(
                "this build includes the demo analytics provider; its data is fabricated"
            );
        }

        let rate_limits = Arc::new(RateLimitManager::new());
        rate_limits.configure(
            vds_infra_analytics::YANDEX_METRICA_PROVIDER_ID,
            configuration.analytics.rate_limit_per_minute,
            clock.now(),
        );

        let detector = TrafficAnomalyDetector::new(AnomalyConfig {
            threshold_percent: f64::from(configuration.analytics.anomaly_threshold_percent),
            ..AnomalyConfig::default()
        });

        let analytics = Arc::new(AnalyticsService::new(
            Arc::new(providers),
            Arc::clone(&analytics_repository),
            Arc::clone(&events),
            Arc::clone(&clock),
            Arc::clone(&rate_limits),
            detector,
        ));

        // --- screenshots ---
        let screenshot_provider: Arc<dyn ScreenshotProvider> =
            Arc::new(match &configuration.screenshots.browser_path {
                Some(path) => ChromiumScreenshotProvider::with_executable(path),
                None => ChromiumScreenshotProvider::discover(),
            });

        // A real browser still wins when there is one: a development build should
        // exercise the real capture path wherever it can, and fall back only so that the
        // grid, the thumbnail cache and the staleness wording can be worked on from a
        // machine with no Chromium installed.
        #[cfg(feature = "demo-providers")]
        let screenshot_provider: Arc<dyn ScreenshotProvider> = if screenshot_provider
            .is_available()
            .await
        {
            screenshot_provider
        } else {
            tracing::warn!(
                "no browser found; using the demo screenshot provider, whose images are generated"
            );
            Arc::new(vds_infra_screenshot::demo::DemoScreenshotProvider::new())
        };

        let screenshot_directory = configuration
            .storage
            .screenshot_dir
            .clone()
            .unwrap_or_else(|| paths.screenshots.clone());
        let screenshot_store: Arc<dyn ScreenshotStore> =
            Arc::new(FilesystemScreenshotStore::new(screenshot_directory));

        let screenshots = Arc::new(ScreenshotService::new(
            screenshot_provider,
            screenshot_store,
            Arc::clone(&screenshots_repository),
            Arc::clone(&websites),
            Arc::clone(&events),
            Arc::clone(&clock),
            configuration.screenshots.clone(),
        ));

        // --- alerting ---
        let mut notifiers: Vec<Arc<dyn NotificationProvider>> = Vec::new();
        if configuration.notifications.desktop_enabled {
            notifiers.push(Arc::new(DesktopNotificationProvider::new(
                "VDS Admin",
                configuration.notifications.sound_enabled,
            )));
        }
        match WebhookNotificationProvider::new(configuration.notifications.webhook_url.clone()) {
            Ok(provider) => notifiers.push(Arc::new(provider)),
            Err(err) => tracing::warn!(error = %err, "the webhook channel is unavailable"),
        }

        let dispatcher = Arc::new(NotificationDispatcher::new(
            notifiers,
            Status::from_str_lenient(&configuration.notifications.min_severity),
        ));

        let alert_service = Arc::new(AlertService::new(
            Arc::clone(&alerts_repository),
            dispatcher,
            Arc::clone(&events),
            Arc::clone(&clock),
        ));

        // --- maintenance ---
        let aggregation = Arc::new(MetricsAggregationService::new(
            Arc::clone(&metrics),
            Arc::clone(&clock),
        ));
        let retention = Arc::new(RetentionService::new(
            Arc::clone(&metrics),
            Arc::clone(&events_repository),
            Arc::clone(&alerts_repository),
            Arc::clone(&websites),
            Arc::clone(&analytics_repository),
            Arc::clone(&clock),
            configuration.storage.retention,
        ));

        let dashboard = Arc::new(DashboardQueryService::new(
            Arc::clone(&servers),
            Arc::clone(&websites),
            Arc::clone(&analytics_repository),
            Arc::clone(&alerts_repository),
            Arc::clone(&events_repository),
            Arc::clone(&clock),
        ));

        // Adding a server is where a secret enters the system, so it goes through a
        // use case that owns the ordering and the rollback rather than through a dialog
        // handler.
        let provisioning = Arc::new(ProvisioningService::new(
            Arc::clone(&servers),
            Arc::clone(&websites),
            Arc::clone(&analytics_repository) as Arc<dyn AnalyticsRepository>,
            Arc::clone(&secrets),
            Arc::clone(&clock),
        ));

        let scheduler = Arc::new(Scheduler::new(
            Arc::clone(&clock),
            limits_from(&configuration),
        ));

        Ok(Self {
            paths,
            configuration,
            clock,
            servers,
            websites,
            metrics,
            analytics_repository,
            alerts_repository,
            events_repository,
            screenshots_repository,
            secrets,
            secret_backend,
            server_monitor,
            website_monitor,
            analytics,
            screenshots,
            alert_service,
            aggregation,
            retention,
            dashboard,
            provisioning,
            files,
            scheduler,
            known_hosts,
            database,
        })
    }

    /// Seeds the default alert rules on a fresh installation.
    ///
    /// Only when there are none: it must never resurrect rules a user deleted.
    pub async fn seed_default_alerts(&self) -> Result<usize, ApplicationError> {
        if !self.alerts_repository.list_rules().await?.is_empty() {
            return Ok(0);
        }

        let rules = vds_domain::alerts::AlertRule::defaults(self.clock.now());
        let count = rules.len();
        for rule in rules {
            self.alerts_repository.save_rule(&rule).await?;
        }
        Ok(count)
    }
}

/// Concurrency limits derived from configuration.
fn limits_from(configuration: &Configuration) -> ConcurrencyLimits {
    use vds_application::scheduler::Priority;

    let mut limits = ConcurrencyLimits::default();
    limits.per_priority.insert(
        Priority::ServerAvailability,
        configuration.monitoring.max_concurrent_servers,
    );
    limits.per_priority.insert(
        Priority::CoreMetrics,
        configuration.monitoring.max_concurrent_servers,
    );
    limits.per_priority.insert(
        Priority::WebsiteAvailability,
        configuration.monitoring.max_concurrent_websites,
    );
    limits.per_priority.insert(
        Priority::Analytics,
        configuration.analytics.max_concurrent_requests,
    );
    limits.per_priority.insert(
        Priority::Screenshots,
        configuration.screenshots.max_concurrent,
    );

    // The global ceiling must be at least as large as the largest per-kind ceiling, or
    // that setting would silently do nothing.
    limits.global = limits
        .per_priority
        .values()
        .copied()
        .max()
        .unwrap_or(16)
        .max(configuration.monitoring.max_concurrent_servers)
        .max(48);

    limits
}

/// The user agent websites will see.
fn user_agent() -> String {
    format!(
        "vds-admin/{} (+https://github.com/vds-admin/vds-admin)",
        env!("CARGO_PKG_VERSION")
    )
}

/// Chooses a credential backend.
async fn resolve_secrets(
    paths: &AppPaths,
    setup: SecretsSetup,
) -> Result<(Arc<dyn SecretStore>, SecretBackend), ApplicationError> {
    match setup {
        SecretsSetup::Provided(store) => {
            let description = store.backend_description();
            Ok((store, SecretBackend::OsKeyring(description)))
        }

        SecretsSetup::EncryptedFile { passphrase } => {
            let store = FileSecretStore::open(&paths.secrets_vault, &passphrase)
                .map_err(|e| ApplicationError::Secrets(e.to_string()))?;
            let backend = SecretBackend::EncryptedFile {
                path: paths.secrets_vault.display().to_string(),
                reason: "chosen explicitly".to_owned(),
            };
            Ok((Arc::new(store), backend))
        }

        SecretsSetup::Automatic {
            fallback_passphrase,
        } => {
            let keyring = OsKeyringStore::new();

            // Probe rather than assume: on a headless Linux box the keystore is usually
            // absent, and discovering that during the first SSH connection would be far
            // too late.
            match keyring.probe().await {
                Ok(()) => {
                    let backend =
                        SecretBackend::OsKeyring(OsKeyringStore::platform_name().to_owned());
                    Ok((Arc::new(keyring), backend))
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "the platform keystore is unusable; falling back to an encrypted file"
                    );
                    let store =
                        EncryptedFileStore::open(&paths.secrets_vault, &fallback_passphrase)
                            .map_err(|e| ApplicationError::Secrets(e.to_string()))?;
                    let backend = SecretBackend::EncryptedFile {
                        path: paths.secrets_vault.display().to_string(),
                        reason: err.to_string(),
                    };
                    let resolved = ResolvedSecretStore::new(
                        Arc::new(FileSecretStoreAdapter(store)),
                        backend.clone(),
                    );
                    Ok((Arc::new(resolved), backend))
                }
            }
        }
    }
}

/// Adapts the concrete store to the port.
struct FileSecretStoreAdapter(EncryptedFileStore);

#[async_trait::async_trait]
impl SecretStore for FileSecretStoreAdapter {
    async fn store(
        &self,
        reference: vds_domain::ids::CredentialRef,
        kind: vds_domain::ports::SecretKind,
        secret: vds_domain::ports::Secret,
    ) -> Result<(), vds_domain::ports::SecretStoreError> {
        self.0.put(reference, kind, &secret)
    }

    async fn retrieve(
        &self,
        reference: vds_domain::ids::CredentialRef,
        kind: vds_domain::ports::SecretKind,
    ) -> Result<vds_domain::ports::Secret, vds_domain::ports::SecretStoreError> {
        self.0.get(reference, kind)
    }

    async fn delete(
        &self,
        reference: vds_domain::ids::CredentialRef,
        kind: vds_domain::ports::SecretKind,
    ) -> Result<(), vds_domain::ports::SecretStoreError> {
        self.0.remove(reference, kind)
    }

    async fn contains(
        &self,
        reference: vds_domain::ids::CredentialRef,
        kind: vds_domain::ports::SecretKind,
    ) -> Result<bool, vds_domain::ports::SecretStoreError> {
        Ok(self.0.contains(reference, kind))
    }

    async fn delete_all(
        &self,
        reference: vds_domain::ids::CredentialRef,
    ) -> Result<(), vds_domain::ports::SecretStoreError> {
        self.0.remove_all(reference)
    }

    fn backend_description(&self) -> String {
        format!("encrypted file at {}", self.0.path().display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_application::testing::FakeSecretStore;
    use vds_domain::ports::NullEventPublisher;

    async fn assemble_in(dir: &tempfile::TempDir) -> Application {
        Application::assemble(
            AppPaths::rooted(dir.path()),
            Configuration::default(),
            Arc::new(NullEventPublisher),
            SecretsSetup::Provided(Arc::new(FakeSecretStore::new())),
        )
        .await
        .expect("assembles")
    }

    #[tokio::test]
    async fn the_application_assembles_from_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let application = assemble_in(&dir).await;

        // Every dependency is present and every directory exists.
        assert!(application.paths.data_dir.is_dir());
        assert!(application.paths.screenshots.is_dir());
        assert!(application.paths.database.exists());
        assert_eq!(application.scheduler.registered_count(), 0);
    }

    #[tokio::test]
    async fn assembling_twice_over_the_same_data_directory_works() {
        // This is what happens on every restart.
        let dir = tempfile::tempdir().expect("temp dir");
        let _first = assemble_in(&dir).await;
        let _second = assemble_in(&dir).await;
    }

    #[tokio::test]
    async fn the_default_alert_rules_are_seeded_once_and_only_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let application = assemble_in(&dir).await;

        let seeded = application.seed_default_alerts().await.expect("seeded");
        assert_eq!(seeded, 7);

        // A second call must not duplicate them — nor resurrect rules a user deleted.
        assert_eq!(application.seed_default_alerts().await.expect("seeded"), 0);
        assert_eq!(
            application
                .alerts_repository
                .list_rules()
                .await
                .expect("listed")
                .len(),
            7
        );
    }

    #[tokio::test]
    async fn the_repositories_are_all_backed_by_the_same_database() {
        let dir = tempfile::tempdir().expect("temp dir");
        let application = assemble_in(&dir).await;

        let server = vds_domain::server::Server::new(
            "prod-01",
            "10.0.0.1",
            vds_domain::server::ConnectionSettings::Ssh(vds_domain::server::SshSettings {
                username: "root".into(),
                auth_kind: vds_domain::server::SshAuthKind::PrivateKey,
                credential_ref: vds_domain::ids::CredentialRef::new(),
            }),
            application.clock.now(),
        );
        application.servers.save(&server).await.expect("saved");

        // Read back through the dashboard, which is a different object entirely.
        let summary = application.dashboard.infrastructure().await;
        assert_eq!(summary.servers.total, 0, "no state has been recorded yet");
        assert_eq!(application.servers.list().await.expect("listed").len(), 1);
    }

    #[tokio::test]
    async fn a_custom_database_path_is_honoured() {
        let dir = tempfile::tempdir().expect("temp dir");
        let elsewhere = dir.path().join("custom").join("metrics.db");

        let mut configuration = Configuration::default();
        configuration.storage.database_path = Some(elsewhere.clone());

        let application = Application::assemble(
            AppPaths::rooted(dir.path()),
            configuration,
            Arc::new(NullEventPublisher),
            SecretsSetup::Provided(Arc::new(FakeSecretStore::new())),
        )
        .await
        .expect("assembles");

        assert!(elsewhere.exists());
        assert!(
            !application.paths.database.exists(),
            "the default path must be unused"
        );
    }

    #[tokio::test]
    async fn the_encrypted_file_backend_can_be_chosen_explicitly() {
        let dir = tempfile::tempdir().expect("temp dir");
        let application = Application::assemble(
            AppPaths::rooted(dir.path()),
            Configuration::default(),
            Arc::new(NullEventPublisher),
            SecretsSetup::EncryptedFile {
                passphrase: "a test passphrase".into(),
            },
        )
        .await
        .expect("assembles");

        assert!(!application.secret_backend.is_os_keyring());
        assert!(
            application
                .secret_backend
                .describe()
                .contains("encrypted file")
        );

        // And the store actually works.
        let reference = vds_domain::ids::CredentialRef::new();
        application
            .secrets
            .store(
                reference,
                vds_domain::ports::SecretKind::SshPassword,
                vds_domain::ports::Secret::from_string("hunter2".into()),
            )
            .await
            .expect("stored");
        assert_eq!(
            application
                .secrets
                .retrieve(reference, vds_domain::ports::SecretKind::SshPassword)
                .await
                .expect("read")
                .expose(),
            b"hunter2"
        );
    }

    #[test]
    fn the_global_concurrency_ceiling_is_never_below_a_per_kind_one() {
        // Otherwise raising `max_concurrent_websites` would silently do nothing.
        let mut configuration = Configuration::default();
        configuration.monitoring.max_concurrent_websites = 500;
        configuration.monitoring.max_concurrent_servers = 200;

        let limits = limits_from(&configuration);
        assert!(limits.global >= 500);
        assert_eq!(
            limits.limit_for(vds_application::scheduler::Priority::WebsiteAvailability),
            500
        );
        assert_eq!(
            limits.limit_for(vds_application::scheduler::Priority::ServerAvailability),
            200
        );
    }

    #[test]
    fn screenshot_concurrency_stays_small_by_default() {
        // A headless browser is expensive; the default must not be raised by accident.
        let limits = limits_from(&Configuration::default());
        assert!(limits.limit_for(vds_application::scheduler::Priority::Screenshots) <= 4);
    }

    #[test]
    fn the_user_agent_identifies_the_application_and_its_version() {
        let agent = user_agent();
        assert!(agent.starts_with("vds-admin/"));
        assert!(agent.contains(env!("CARGO_PKG_VERSION")));
        // Site owners should be able to find out what is polling them.
        assert!(agent.contains("https://"));
    }
}
