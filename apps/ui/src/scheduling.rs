//! Registering the recurring work.
//!
//! Every background job in the application goes through the one scheduler, so priorities,
//! concurrency limits, backoff and shutdown are decided in a single place. This module is
//! only the registration: it turns each server, website and integration into a
//! [`Task`](vds_application::scheduler::Task).

use crate::intents::Intent;
use async_trait::async_trait;
use chrono::Duration;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use vds_application::alerts::AlertObservation;
use vds_application::scheduler::{BackoffPolicy, JobKey, JobOutcome, Priority, Task};
use vds_composition::Application;
use vds_domain::ids::{IntegrationId, ServerId, WebsiteId};

/// How often the registration pass re-runs.
///
/// Servers and websites are added and removed while the application runs, so the job set
/// is rebuilt periodically rather than only at startup.
const REGISTRATION_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// How far apart the first runs of a large fleet are spread.
///
/// Adding two hundred servers at once must not open two hundred SSH connections in the
/// same instant.
const STAGGER: Duration = Duration::milliseconds(250);

/// Collects one server.
struct CollectServer {
    application: Arc<Application>,
    server: ServerId,
    intents: UnboundedSender<Intent>,
}

#[async_trait]
impl Task for CollectServer {
    fn key(&self) -> JobKey {
        JobKey::new(format!("server:{}", self.server))
    }

    fn priority(&self) -> Priority {
        Priority::ServerAvailability
    }

    async fn run(&self, cancel: CancellationToken) -> JobOutcome {
        if cancel.is_cancelled() {
            return JobOutcome::Skipped;
        }

        let outcome = tokio::select! {
            outcome = self.application.server_monitor.collect(self.server) => outcome,
            // Shutdown must not wait for a 20-second SSH timeout to expire.
            _ = cancel.cancelled() => return JobOutcome::Skipped,
        };

        // The window re-queries rather than being pushed a diff, so a completed
        // collection just asks it to refresh.
        let _ = self.intents.send(Intent::RefreshServers);
        let _ = self.intents.send(Intent::RefreshDashboard);
        outcome
    }
}

/// Checks one website.
struct CheckWebsite {
    application: Arc<Application>,
    website: WebsiteId,
    intents: UnboundedSender<Intent>,
}

#[async_trait]
impl Task for CheckWebsite {
    fn key(&self) -> JobKey {
        JobKey::new(format!("website:{}", self.website))
    }

    fn priority(&self) -> Priority {
        Priority::WebsiteAvailability
    }

    async fn run(&self, cancel: CancellationToken) -> JobOutcome {
        let outcome = tokio::select! {
            outcome = self.application.website_monitor.check(self.website) => outcome,
            _ = cancel.cancelled() => return JobOutcome::Skipped,
        };

        let _ = self.intents.send(Intent::RefreshWebsites);
        outcome
    }
}

/// Refreshes one analytics integration.
struct RefreshAnalytics {
    application: Arc<Application>,
    integration: IntegrationId,
    intents: UnboundedSender<Intent>,
}

#[async_trait]
impl Task for RefreshAnalytics {
    fn key(&self) -> JobKey {
        JobKey::new(format!("analytics:{}", self.integration))
    }

    fn priority(&self) -> Priority {
        Priority::Analytics
    }

    async fn run(&self, cancel: CancellationToken) -> JobOutcome {
        let outcome = tokio::select! {
            outcome = self.application.analytics.refresh(self.integration) => outcome,
            _ = cancel.cancelled() => return JobOutcome::Skipped,
        };

        let _ = self.intents.send(Intent::RefreshDashboard);
        outcome
    }
}

/// Captures one website's screenshot, if the policy says it is due.
struct CaptureScreenshot {
    application: Arc<Application>,
    website: WebsiteId,
    intents: UnboundedSender<Intent>,
}

#[async_trait]
impl Task for CaptureScreenshot {
    fn key(&self) -> JobKey {
        JobKey::new(format!("screenshot:{}", self.website))
    }

    fn priority(&self) -> Priority {
        Priority::Screenshots
    }

    async fn run(&self, cancel: CancellationToken) -> JobOutcome {
        let outcome = tokio::select! {
            outcome = self.application.screenshots.capture_if_due(self.website) => outcome,
            _ = cancel.cancelled() => return JobOutcome::Skipped,
        };

        if outcome == JobOutcome::Success {
            let _ = self.intents.send(Intent::RefreshWebsites);
        }
        outcome
    }
}

/// Evaluates every alert rule.
struct EvaluateAlerts {
    application: Arc<Application>,
    intents: UnboundedSender<Intent>,
}

#[async_trait]
impl Task for EvaluateAlerts {
    fn key(&self) -> JobKey {
        JobKey::new("alerts")
    }

    fn priority(&self) -> Priority {
        Priority::CriticalAlert
    }

    async fn run(&self, _cancel: CancellationToken) -> JobOutcome {
        let observations = gather_observations(&self.application).await;
        let report = self
            .application
            .alert_service
            .evaluate_all(&observations)
            .await;

        if report.opened > 0 || report.resolved > 0 {
            let _ = self.intents.send(Intent::RefreshAlerts);
            let _ = self.intents.send(Intent::RefreshDashboard);
        }
        JobOutcome::Success
    }
}

/// Builds rollups.
struct Aggregate {
    application: Arc<Application>,
}

#[async_trait]
impl Task for Aggregate {
    fn key(&self) -> JobKey {
        JobKey::new("aggregate")
    }

    fn priority(&self) -> Priority {
        Priority::Maintenance
    }

    async fn run(&self, _cancel: CancellationToken) -> JobOutcome {
        self.application.aggregation.run_as_job().await
    }
}

/// Applies retention.
struct Retention {
    application: Arc<Application>,
}

#[async_trait]
impl Task for Retention {
    fn key(&self) -> JobKey {
        JobKey::new("retention")
    }

    fn priority(&self) -> Priority {
        Priority::Maintenance
    }

    async fn run(&self, _cancel: CancellationToken) -> JobOutcome {
        self.application.retention.run_as_job().await
    }
}

/// Assembles what the alert engine needs to know about every subject.
///
/// Lives here rather than in the engine because gathering it touches four repositories,
/// and keeping the engine free of I/O is what makes its hold timers testable.
async fn gather_observations(application: &Arc<Application>) -> Vec<AlertObservation> {
    use vds_domain::events::AlertSubject;
    use vds_domain::metrics::MetricKind;

    let mut observations = Vec::new();

    let servers = application.servers.list().await.unwrap_or_default();
    let states = application.servers.list_states().await.unwrap_or_default();

    for server in &servers {
        let Some(state) = states.iter().find(|s| s.server_id == server.id) else {
            continue;
        };

        let mut observation =
            AlertObservation::new(AlertSubject::Server(server.id), &server.name, state.status)
                .with_tags(server.tags.clone());

        // Only measured values are offered. A metric that is absent must not be
        // presented as zero, or a "disk below 5%" rule would fire on every unreachable
        // server.
        for (kind, value) in [
            (MetricKind::CpuUsage, state.cpu_percent),
            (MetricKind::MemoryUsage, state.memory_percent),
            (MetricKind::DiskUsage, state.disk_percent),
        ] {
            if let Some(number) = value.value() {
                observation.metrics.insert(kind, number);
            }
        }

        observations.push(observation);
    }

    let websites = application.websites.list().await.unwrap_or_default();
    let website_states = application.websites.list_states().await.unwrap_or_default();

    for website in &websites {
        let Some(state) = website_states.iter().find(|s| s.website_id == website.id) else {
            continue;
        };

        let mut observation = AlertObservation::new(
            AlertSubject::Website(website.id),
            &website.name,
            state.status,
        )
        .with_tags(website.tags.clone());
        observation.ssl_days_remaining = state.ssl_days_remaining;
        observation.unexpected_response = state.status == vds_domain::Status::Critical;

        if let Some(ms) = state.response_ms {
            observation
                .metrics
                .insert(MetricKind::ResponseTimeMs, f64::from(ms));
        }

        observations.push(observation);
    }

    observations
}

/// Registers everything, then keeps the registration current.
///
/// Runs until the process ends. Rebuilding the job set on a timer is what makes a server
/// added in the UI start being polled without a restart.
pub async fn run(application: Arc<Application>, intents: UnboundedSender<Intent>) {
    register_fixed(&application, &intents);

    let scheduler = Arc::clone(&application.scheduler);
    let driver = {
        let scheduler = Arc::clone(&scheduler);
        tokio::spawn(async move {
            scheduler.run(std::time::Duration::from_secs(5)).await;
        })
    };

    loop {
        register_subjects(&application, &intents).await;
        tokio::time::sleep(REGISTRATION_INTERVAL).await;

        if application.scheduler.cancellation_token().is_cancelled() {
            break;
        }
    }

    driver.abort();
}

/// Registers the jobs that always exist.
fn register_fixed(application: &Arc<Application>, intents: &UnboundedSender<Intent>) {
    let scheduler = &application.scheduler;

    scheduler.register(
        Arc::new(EvaluateAlerts {
            application: Arc::clone(application),
            intents: intents.clone(),
        }),
        Duration::seconds(30),
        BackoffPolicy::default(),
    );

    scheduler.register(
        Arc::new(Aggregate {
            application: Arc::clone(application),
        }),
        Duration::minutes(5),
        BackoffPolicy::default(),
    );

    // Retention runs rarely: it is housekeeping, and running it often would spend disk
    // I/O deleting a handful of rows.
    scheduler.register(
        Arc::new(Retention {
            application: Arc::clone(application),
        }),
        Duration::hours(6),
        BackoffPolicy::default(),
    );
}

/// Reads a list for the scheduler, saying so when it cannot.
///
/// `unwrap_or_default` was here, and it is the wrong tool for this: a query that fails and
/// a table that is empty become the same empty vector, and the visible result is that
/// monitoring quietly stops for everything of that kind. The scheduler still carries on
/// with what it has — one unreadable table must not stop the others — but it no longer
/// does so silently.
fn or_report<T>(outcome: Result<Vec<T>, vds_domain::ports::RepositoryError>, what: &str) -> Vec<T> {
    match outcome {
        Ok(items) => items,
        Err(error) => {
            tracing::warn!(%error, what, "could not read the list; nothing of this kind will run");
            Vec::new()
        }
    }
}

/// Registers a job per server, website and integration.
///
/// Idempotent: re-registering an existing job updates its interval without disturbing an
/// in-flight run or forgiving a failure streak.
async fn register_subjects(application: &Arc<Application>, intents: &UnboundedSender<Intent>) {
    let scheduler = &application.scheduler;

    let servers = or_report(application.servers.list().await, "servers");
    for (index, server) in servers.iter().enumerate() {
        if !server.enabled {
            scheduler.unregister(&JobKey::new(format!("server:{}", server.id)));
            continue;
        }

        scheduler.register_delayed(
            Arc::new(CollectServer {
                application: Arc::clone(application),
                server: server.id,
                intents: intents.clone(),
            }),
            server.poll_interval(),
            BackoffPolicy::default(),
            STAGGER * i32::try_from(index).unwrap_or(0),
        );
    }

    let websites = or_report(application.websites.list().await, "websites");
    for (index, website) in websites.iter().enumerate() {
        if !website.enabled {
            scheduler.unregister(&JobKey::new(format!("website:{}", website.id)));
            scheduler.unregister(&JobKey::new(format!("screenshot:{}", website.id)));
            continue;
        }

        scheduler.register_delayed(
            Arc::new(CheckWebsite {
                application: Arc::clone(application),
                website: website.id,
                intents: intents.clone(),
            }),
            website.poll_interval(),
            BackoffPolicy::default(),
            STAGGER * i32::try_from(index).unwrap_or(0),
        );

        // The capture task itself asks the policy whether anything is due, so this
        // interval is just how often that question is posed.
        scheduler.register_delayed(
            Arc::new(CaptureScreenshot {
                application: Arc::clone(application),
                website: website.id,
                intents: intents.clone(),
            }),
            Duration::minutes(15),
            BackoffPolicy::default(),
            Duration::minutes(1) + STAGGER * i32::try_from(index).unwrap_or(0),
        );
    }

    // Read explicitly rather than with `unwrap_or_default`: a failing query and an empty
    // table look identical through that, and "analytics never refreshes" is exactly the
    // symptom either would produce.
    let mut registered_integrations = 0usize;
    let integrations = or_report(
        application.analytics_repository.list_integrations().await,
        "analytics integrations",
    );
    for integration in &integrations {
        if !integration.enabled {
            scheduler.unregister(&JobKey::new(format!("analytics:{}", integration.id)));
            continue;
        }

        registered_integrations += 1;
        scheduler.register(
            Arc::new(RefreshAnalytics {
                application: Arc::clone(application),
                integration: integration.id,
                intents: intents.clone(),
            }),
            integration.refresh_interval(),
            BackoffPolicy {
                // Analytics providers have quotas; a failure should back off hard rather
                // than burn the allowance retrying.
                initial_secs: 60,
                max_secs: 3_600,
                multiplier: 2,
                jitter_percent: 20,
            },
        );
    }

    // Said once per pass, at debug, so that "nothing is refreshing" can be answered from
    // the log rather than from the database.
    tracing::debug!(
        servers = servers.len(),
        websites = websites.len(),
        integrations = registered_integrations,
        "scheduler pass"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_application::testing::FakeSecretStore;
    use vds_composition::{AppPaths, SecretsSetup};
    use vds_domain::ports::NullEventPublisher;
    use vds_domain::server::{ConnectionSettings, Server, SshAuthKind, SshSettings};
    use vds_domain::website::Website;

    async fn application_in(dir: &tempfile::TempDir) -> Arc<Application> {
        Arc::new(
            Application::assemble(
                AppPaths::rooted(dir.path()),
                vds_application::config::Configuration::default(),
                Arc::new(NullEventPublisher),
                SecretsSetup::Provided(Arc::new(FakeSecretStore::new())),
            )
            .await
            .expect("assembles"),
        )
    }

    fn sample_server(name: &str) -> Server {
        Server::new(
            name,
            "10.0.0.1",
            ConnectionSettings::Ssh(SshSettings {
                username: "root".into(),
                auth_kind: SshAuthKind::PrivateKey,
                credential_ref: vds_domain::ids::CredentialRef::new(),
            }),
            chrono::Utc::now(),
        )
    }

    #[tokio::test]
    async fn every_server_and_website_gets_a_job() {
        let dir = tempfile::tempdir().expect("temp dir");
        let application = application_in(&dir).await;
        let (intents, _receiver) = tokio::sync::mpsc::unbounded_channel();

        application
            .servers
            .save(&sample_server("a"))
            .await
            .expect("saved");
        application
            .servers
            .save(&sample_server("b"))
            .await
            .expect("saved");
        application
            .websites
            .save(&Website::new(
                "site",
                "https://example.com/",
                chrono::Utc::now(),
            ))
            .await
            .expect("saved");

        register_subjects(&application, &intents).await;

        // Two servers, one website check, one screenshot task.
        assert_eq!(application.scheduler.registered_count(), 4);
    }

    #[tokio::test]
    async fn a_disabled_server_is_unregistered_rather_than_left_polling() {
        let dir = tempfile::tempdir().expect("temp dir");
        let application = application_in(&dir).await;
        let (intents, _receiver) = tokio::sync::mpsc::unbounded_channel();

        let mut server = sample_server("a");
        application.servers.save(&server).await.expect("saved");
        register_subjects(&application, &intents).await;
        assert_eq!(application.scheduler.registered_count(), 1);

        server.enabled = false;
        application.servers.save(&server).await.expect("saved");
        register_subjects(&application, &intents).await;
        assert_eq!(application.scheduler.registered_count(), 0);
    }

    #[tokio::test]
    async fn re_registering_does_not_duplicate_jobs() {
        // The registration pass runs every thirty seconds for the life of the process.
        let dir = tempfile::tempdir().expect("temp dir");
        let application = application_in(&dir).await;
        let (intents, _receiver) = tokio::sync::mpsc::unbounded_channel();

        application
            .servers
            .save(&sample_server("a"))
            .await
            .expect("saved");

        for _ in 0..5 {
            register_subjects(&application, &intents).await;
        }
        assert_eq!(application.scheduler.registered_count(), 1);
    }

    #[tokio::test]
    async fn the_fixed_maintenance_jobs_are_registered() {
        let dir = tempfile::tempdir().expect("temp dir");
        let application = application_in(&dir).await;
        let (intents, _receiver) = tokio::sync::mpsc::unbounded_channel();

        register_fixed(&application, &intents);

        let keys: Vec<String> = application
            .scheduler
            .snapshot()
            .iter()
            .map(|j| j.key.to_string())
            .collect();
        assert!(keys.contains(&"alerts".to_owned()));
        assert!(keys.contains(&"aggregate".to_owned()));
        assert!(keys.contains(&"retention".to_owned()));
    }

    #[tokio::test]
    async fn alert_evaluation_has_the_highest_priority() {
        // If the machine cannot keep up, the user still finds out something is wrong.
        let dir = tempfile::tempdir().expect("temp dir");
        let application = application_in(&dir).await;
        let (intents, _receiver) = tokio::sync::mpsc::unbounded_channel();

        register_fixed(&application, &intents);
        let snapshot = application.scheduler.snapshot();
        assert_eq!(
            snapshot.first().map(|j| j.priority),
            Some(Priority::CriticalAlert)
        );
    }

    #[tokio::test]
    async fn a_large_fleet_is_staggered_rather_than_started_all_at_once() {
        // Two hundred simultaneous SSH handshakes is a self-inflicted outage.
        let dir = tempfile::tempdir().expect("temp dir");
        let application = application_in(&dir).await;
        let (intents, _receiver) = tokio::sync::mpsc::unbounded_channel();

        for index in 0..20 {
            application
                .servers
                .save(&sample_server(&format!("server-{index}")))
                .await
                .expect("saved");
        }
        register_subjects(&application, &intents).await;

        let due: Vec<_> = application
            .scheduler
            .snapshot()
            .iter()
            .map(|j| j.due_at)
            .collect();
        let earliest = due.iter().min().copied().expect("jobs registered");
        let latest = due.iter().max().copied().expect("jobs registered");
        assert!(
            latest > earliest,
            "every job was scheduled for the same instant"
        );
    }

    #[tokio::test]
    async fn observations_omit_metrics_that_were_never_measured() {
        // Offering a zero would make a "disk below 5%" rule fire on every unreachable
        // server.
        let dir = tempfile::tempdir().expect("temp dir");
        let application = application_in(&dir).await;

        let server = sample_server("a");
        application.servers.save(&server).await.expect("saved");
        application
            .servers
            .save_state(&vds_domain::server::ServerRuntimeState::unknown(server.id))
            .await
            .expect("saved");

        let observations = gather_observations(&application).await;
        assert_eq!(observations.len(), 1);
        assert!(
            observations[0].metrics.is_empty(),
            "no metric should be offered"
        );
    }

    #[tokio::test]
    async fn observations_carry_measured_metrics_and_tags() {
        let dir = tempfile::tempdir().expect("temp dir");
        let application = application_in(&dir).await;

        let mut server = sample_server("a");
        server.tags = vec!["production".into()];
        application.servers.save(&server).await.expect("saved");

        let mut state = vds_domain::server::ServerRuntimeState::unknown(server.id);
        state.status = vds_domain::Status::Warning;
        state.cpu_percent = vds_domain::metrics::MetricValue::Available(95.0);
        application.servers.save_state(&state).await.expect("saved");

        let observations = gather_observations(&application).await;
        assert_eq!(
            observations[0]
                .metrics
                .get(&vds_domain::metrics::MetricKind::CpuUsage),
            Some(&95.0)
        );
        assert_eq!(observations[0].tags, vec!["production".to_owned()]);
        assert_eq!(observations[0].status, vds_domain::Status::Warning);
    }
}
