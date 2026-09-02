//! The server monitoring use case: one collection cycle, start to finish.
//!
//! Probe → evaluate → persist → publish. Every dependency is a port, so the whole cycle
//! runs in a unit test against in-memory fakes.

use super::offline::{OfflineDetector, Transition};
use super::rates::RateTracker;
use crate::metrics::samples::samples_from_snapshot;
use crate::scheduler::JobOutcome;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use vds_domain::Status;
use vds_domain::events::DomainEvent;
use vds_domain::ids::ServerId;
use vds_domain::metrics::MetricValue;
use vds_domain::ports::{
    Clock, EventPublisher, MetricsRepository, RepositoryError, ServerProbe, ServerRepository,
    TransportError,
};
use vds_domain::server::{Server, ServerSnapshot, evaluate_snapshot};

/// Runs collection cycles for servers.
pub struct ServerMonitor {
    probe: Arc<dyn ServerProbe>,
    servers: Arc<dyn ServerRepository>,
    metrics: Arc<dyn MetricsRepository>,
    events: Arc<dyn EventPublisher>,
    clock: Arc<dyn Clock>,
    rates: Arc<Mutex<RateTracker>>,
    snapshots: Arc<Mutex<HashMap<ServerId, Arc<ServerSnapshot>>>>,
}

impl ServerMonitor {
    pub fn new(
        probe: Arc<dyn ServerProbe>,
        servers: Arc<dyn ServerRepository>,
        metrics: Arc<dyn MetricsRepository>,
        events: Arc<dyn EventPublisher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            probe,
            servers,
            metrics,
            events,
            clock,
            rates: Arc::new(Mutex::new(RateTracker::new())),
            snapshots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Collects one server and records everything that follows from it.
    pub async fn collect(&self, server_id: ServerId) -> JobOutcome {
        let server = match self.servers.get(server_id).await {
            Ok(server) => server,
            Err(RepositoryError::NotFound { .. }) => {
                // Deleted while the job was queued. Not a failure.
                return JobOutcome::Skipped;
            }
            Err(err) => return JobOutcome::Retry(format!("could not load server: {err}")),
        };

        if !server.enabled {
            return JobOutcome::Skipped;
        }

        let now = self.clock.now();
        match self.probe.probe(&server, now).await {
            Ok(snapshot) => self.on_success(&server, snapshot).await,
            Err(err) => self.on_failure(&server, err).await,
        }
    }

    /// The most recent successful collection for a server, if there has been one.
    ///
    /// Processes, containers and services are point-in-time facts with no historical
    /// value, so they are not written to the database — a fleet of fifty servers would
    /// add tens of thousands of rows an hour that nobody would ever query. The detail
    /// page reads the last collection from here instead, and shows nothing at all until
    /// one has happened rather than inventing an empty list.
    pub fn last_snapshot(&self, server_id: ServerId) -> Option<Arc<ServerSnapshot>> {
        self.snapshots.lock().get(&server_id).cloned()
    }

    async fn on_success(&self, server: &Server, snapshot: ServerSnapshot) -> JobOutcome {
        let now = self.clock.now();
        let detector = OfflineDetector::new(server.offline_after_failures);

        let results = evaluate_snapshot(&snapshot, &server.thresholds);
        let health = Status::worst_of(results.iter().map(|r| r.status));

        // Collector-level problems degrade the server too — a machine whose disk
        // collector keeps failing is not fully healthy — but a *missing capability*
        // never does, which `affects_server_health` already encoded as `Unknown`.
        let collector_health = Status::worst_of(
            snapshot
                .outcomes
                .iter()
                .map(|o| o.status)
                .filter(|s| *s != Status::Unknown),
        );
        let health = Status::worst_of([health, collector_health]);

        let rates = self
            .rates
            .lock()
            .observe(server.id, &snapshot.interfaces, now);
        let samples = samples_from_snapshot(&snapshot, rates);
        let sample_count = samples.len();

        if !samples.is_empty()
            && let Err(err) = self.metrics.record_samples(&samples).await
        {
            // Losing a batch of samples is worth retrying, but the status update below
            // is more important than the history, so it still happens.
            tracing::warn!(server = %server.id, error = %err, "could not store metric samples");
        }

        let mut state = self
            .servers
            .load_state(server.id)
            .await
            .unwrap_or_else(|_| vds_domain::server::ServerRuntimeState::unknown(server.id));

        let transition = detector.record_server_success(&mut state, health, now);

        state.uptime_secs = snapshot.uptime_secs;
        state.cpu_percent = snapshot.cpu.total_percent;
        state.memory_percent = snapshot.memory.used_percent();
        state.disk_percent = snapshot.worst_filesystem_percent();

        // Recorded every cycle, including the cycles that clear it. A stale reason beside
        // a healthy server would be worse than none: it would name a problem that has
        // already gone away.
        state.status_cause =
            vds_domain::server::worst_cause(&results, &server.thresholds, &snapshot.outcomes);

        if let Err(err) = self.servers.save_state(&state).await {
            return JobOutcome::Retry(format!("could not save server state: {err}"));
        }

        // Kept for the detail page, which needs the process/container/service lists that
        // the metric samples above deliberately do not carry.
        self.snapshots.lock().insert(server.id, Arc::new(snapshot));

        self.publish_transition(server.id, &transition);
        self.events.publish(DomainEvent::ServerMetricsCollected {
            server_id: server.id,
            metric_count: sample_count,
        });

        // Threshold breaches are published individually so the alert engine does not
        // have to re-derive them from the raw snapshot.
        for result in results.iter().filter(|r| r.status.is_problem()) {
            if let (Some(value), Some(threshold)) =
                (result.value.value(), threshold_for(server, result.kind))
            {
                self.events.publish(DomainEvent::MetricThresholdExceeded {
                    server_id: server.id,
                    metric: result.kind,
                    value,
                    threshold,
                    status: result.status,
                });
            }
        }

        JobOutcome::Success
    }

    async fn on_failure(&self, server: &Server, err: TransportError) -> JobOutcome {
        let now = self.clock.now();
        let detector = OfflineDetector::new(server.offline_after_failures);

        let mut state = self
            .servers
            .load_state(server.id)
            .await
            .unwrap_or_else(|_| vds_domain::server::ServerRuntimeState::unknown(server.id));

        let transition =
            detector.record_server_failure(&mut state, err.to_string(), Some(err.kind()), now);
        let failures = state.consecutive_failures;

        if let Err(save_err) = self.servers.save_state(&state).await {
            tracing::warn!(server = %server.id, error = %save_err, "could not save server state");
        }

        self.publish_transition(server.id, &transition);
        self.events.publish(DomainEvent::ServerCollectionFailed {
            server_id: server.id,
            consecutive_failures: failures,
            error: err.to_string(),
        });

        // A rejected password will be rejected again in thirty seconds. Backing off
        // exponentially would only delay recovery once the user fixes it, and retrying
        // hard risks tripping account lockouts.
        if err.is_retryable() {
            JobOutcome::Retry(err.to_string())
        } else {
            JobOutcome::Permanent(err.to_string())
        }
    }

    fn publish_transition(&self, server_id: ServerId, transition: &Transition) {
        if transition.changed() {
            self.events.publish(DomainEvent::ServerStatusChanged {
                server_id,
                from: transition.from,
                to: transition.to,
                reason: transition.reason.clone(),
            });
        }
    }

    /// Forgets everything cached in memory for a server.
    ///
    /// Called when a server is deleted, so neither its rate baseline nor its last
    /// snapshot outlives it — and so a recycled address cannot inherit them.
    pub fn forget(&self, server_id: ServerId) {
        self.rates.lock().forget(server_id);
        self.snapshots.lock().remove(&server_id);
    }
}

/// The configured threshold value a metric breached, for the event payload.
fn threshold_for(server: &Server, kind: vds_domain::metrics::MetricKind) -> Option<f64> {
    use vds_domain::metrics::MetricKind;
    let thresholds = &server.thresholds;
    let threshold = match kind {
        MetricKind::CpuUsage => thresholds.cpu,
        MetricKind::MemoryUsage => thresholds.memory,
        MetricKind::DiskUsage => thresholds.disk,
        MetricKind::SwapUsage => thresholds.swap,
        MetricKind::LoadAverage1 => thresholds.load_per_core,
        MetricKind::TemperatureCelsius => thresholds.temperature,
        _ => return None,
    };
    Some(threshold.warning)
}

/// Convenience: the current status of every server, for the dashboard.
pub async fn statuses(
    servers: &dyn ServerRepository,
) -> Result<Vec<(ServerId, Status, MetricValue)>, RepositoryError> {
    Ok(servers
        .list_states()
        .await?
        .into_iter()
        .map(|state| (state.server_id, state.status, state.cpu_percent))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMetricsRepository, FakeServerRepository, ScriptedProbe};
    use chrono::DateTime;
    use vds_domain::ids::CredentialRef;
    use vds_domain::metrics::MetricKind;
    use vds_domain::ports::{FixedClock, RecordingEventPublisher};
    use vds_domain::server::{
        ConnectionSettings, FilesystemUsage, MemoryUsage, SshAuthKind, SshSettings,
    };

    fn at(secs: i64) -> DateTime<chrono::Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn test_server() -> Server {
        let mut server = Server::new(
            "prod-01",
            "10.0.0.1",
            ConnectionSettings::Ssh(SshSettings {
                username: "root".into(),
                auth_kind: SshAuthKind::PrivateKey,
                credential_ref: CredentialRef::new(),
            }),
            at(0),
        );
        server.offline_after_failures = 3;
        server
    }

    fn healthy_snapshot(server_id: ServerId, at_time: DateTime<chrono::Utc>) -> ServerSnapshot {
        let mut snapshot = ServerSnapshot::new(server_id, at_time);
        snapshot.cpu.total_percent = MetricValue::Available(12.0);
        snapshot.memory = MemoryUsage {
            total_bytes: Some(100),
            used_bytes: Some(40),
            ..Default::default()
        };
        snapshot.filesystems = vec![FilesystemUsage {
            mount_point: "/".into(),
            device: None,
            filesystem: None,
            total_bytes: 100,
            used_bytes: 55,
            available_bytes: 45,
        }];
        snapshot.uptime_secs = Some(86_400);
        snapshot
    }

    struct Harness {
        monitor: ServerMonitor,
        servers: Arc<FakeServerRepository>,
        metrics: Arc<FakeMetricsRepository>,
        events: Arc<RecordingEventPublisher>,
        probe: Arc<ScriptedProbe>,
        clock: FixedClock,
        server: Server,
    }

    fn harness() -> Harness {
        let server = test_server();
        let servers = Arc::new(FakeServerRepository::new());
        servers.insert(server.clone());
        let metrics = Arc::new(FakeMetricsRepository::new());
        let events = Arc::new(RecordingEventPublisher::new());
        let probe = Arc::new(ScriptedProbe::new());
        let clock = FixedClock::new(at(1_000));

        let monitor = ServerMonitor::new(
            Arc::clone(&probe) as Arc<dyn ServerProbe>,
            Arc::clone(&servers) as Arc<dyn ServerRepository>,
            Arc::clone(&metrics) as Arc<dyn MetricsRepository>,
            Arc::clone(&events) as Arc<dyn EventPublisher>,
            Arc::new(clock.clone()),
        );

        Harness {
            monitor,
            servers,
            metrics,
            events,
            probe,
            clock,
            server,
        }
    }

    #[tokio::test]
    async fn a_successful_cycle_stores_metrics_and_marks_the_server_healthy() {
        let h = harness();
        h.probe
            .respond(Ok(healthy_snapshot(h.server.id, at(1_000))));

        assert_eq!(h.monitor.collect(h.server.id).await, JobOutcome::Success);

        let state = h.servers.state(h.server.id).expect("state saved");
        assert_eq!(state.status, Status::Healthy);
        assert_eq!(state.cpu_percent, MetricValue::Available(12.0));
        assert_eq!(state.uptime_secs, Some(86_400));

        // Percentages are computed in floating point, so compare with a tolerance
        // rather than for exact equality: 55/100*100 is not bit-identical to 55.0.
        let stored = h.metrics.samples();
        let sample = |kind: MetricKind| stored.iter().find(|s| s.kind == kind).map(|s| s.value);
        assert_eq!(sample(MetricKind::CpuUsage), Some(12.0));
        let disk = sample(MetricKind::DiskUsage).expect("disk usage stored");
        assert!((disk - 55.0).abs() < 1e-9, "disk usage was {disk}");
    }

    #[tokio::test]
    async fn a_status_change_is_published_but_a_steady_state_is_not() {
        let h = harness();
        h.probe
            .respond(Ok(healthy_snapshot(h.server.id, at(1_000))));
        h.monitor.collect(h.server.id).await;
        assert!(h.events.contains(|e| e.kind() == "server_status_changed"));

        h.events.clear();
        h.probe
            .respond(Ok(healthy_snapshot(h.server.id, at(1_030))));
        h.monitor.collect(h.server.id).await;
        assert!(
            !h.events.contains(|e| e.kind() == "server_status_changed"),
            "an unchanged status must not be republished every cycle"
        );
    }

    #[tokio::test]
    async fn a_breached_threshold_makes_the_server_warning_and_publishes_the_breach() {
        let h = harness();
        let mut snapshot = healthy_snapshot(h.server.id, at(1_000));
        snapshot.cpu.total_percent = MetricValue::Available(97.0);
        h.probe.respond(Ok(snapshot));

        h.monitor.collect(h.server.id).await;

        let state = h.servers.state(h.server.id).expect("state saved");
        assert_eq!(state.status, Status::Critical);
        assert!(h.events.contains(|e| matches!(
            e,
            DomainEvent::MetricThresholdExceeded {
                metric: MetricKind::CpuUsage,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn three_consecutive_failures_are_needed_before_offline() {
        let h = harness();
        h.probe
            .respond(Err(TransportError::Timeout { seconds: 20 }));

        for expected in [Status::Unknown, Status::Unknown, Status::Offline] {
            let outcome = h.monitor.collect(h.server.id).await;
            assert!(matches!(outcome, JobOutcome::Retry(_)));
            assert_eq!(
                h.servers.state(h.server.id).expect("state").status,
                expected
            );
        }
    }

    #[tokio::test]
    async fn a_bad_credential_is_a_permanent_failure_not_a_retry() {
        // Backing off exponentially on a rejected password only delays recovery once the
        // user fixes it, and retrying hard can lock the account out.
        let h = harness();
        h.probe.respond(Err(TransportError::Authentication(
            "permission denied".into(),
        )));

        let outcome = h.monitor.collect(h.server.id).await;
        assert!(
            matches!(outcome, JobOutcome::Permanent(_)),
            "got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn recovery_is_published_and_clears_the_streak() {
        let h = harness();
        h.probe
            .respond(Err(TransportError::Timeout { seconds: 20 }));
        for _ in 0..3 {
            h.monitor.collect(h.server.id).await;
        }
        assert_eq!(
            h.servers.state(h.server.id).expect("state").status,
            Status::Offline
        );

        h.events.clear();
        h.probe
            .respond(Ok(healthy_snapshot(h.server.id, at(2_000))));
        h.monitor.collect(h.server.id).await;

        let state = h.servers.state(h.server.id).expect("state");
        assert_eq!(state.status, Status::Healthy);
        assert_eq!(state.consecutive_failures, 0);
        assert!(h.events.contains(|e| matches!(
            e,
            DomainEvent::ServerStatusChanged {
                from: Status::Offline,
                to: Status::Healthy,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn a_disabled_server_is_skipped_without_touching_anything() {
        let h = harness();
        let mut server = h.server.clone();
        server.enabled = false;
        h.servers.insert(server);

        assert_eq!(h.monitor.collect(h.server.id).await, JobOutcome::Skipped);
        assert!(h.metrics.samples().is_empty());
        assert!(h.events.is_empty());
    }

    #[tokio::test]
    async fn a_server_deleted_mid_flight_is_skipped_rather_than_failing() {
        let h = harness();
        h.servers.clear();
        assert_eq!(h.monitor.collect(h.server.id).await, JobOutcome::Skipped);
    }

    #[tokio::test]
    async fn network_rates_appear_from_the_second_cycle_onwards() {
        let h = harness();
        let iface = |rx: u64, tx: u64| vds_domain::server::NetworkInterface {
            name: "eth0".into(),
            rx_bytes: rx,
            tx_bytes: tx,
            rx_errors: 0,
            tx_errors: 0,
        };

        let mut first = healthy_snapshot(h.server.id, at(1_000));
        first.interfaces = vec![iface(0, 0)];
        h.probe.respond(Ok(first));
        h.monitor.collect(h.server.id).await;
        assert!(
            !h.metrics
                .samples()
                .iter()
                .any(|s| s.kind == MetricKind::NetworkRxBytesPerSec)
        );

        h.clock.set(at(1_010));
        let mut second = healthy_snapshot(h.server.id, at(1_010));
        second.interfaces = vec![iface(10_000, 2_000)];
        h.probe.respond(Ok(second));
        h.monitor.collect(h.server.id).await;

        let rx = h
            .metrics
            .samples()
            .into_iter()
            .find(|s| s.kind == MetricKind::NetworkRxBytesPerSec)
            .expect("rx rate stored");
        assert_eq!(rx.value, 1_000.0);
    }

    #[tokio::test]
    async fn a_failing_collector_degrades_the_server_but_a_missing_capability_does_not() {
        let h = harness();
        let mut snapshot = healthy_snapshot(h.server.id, at(1_000));
        snapshot.outcomes = vec![
            vds_domain::metrics::CollectorOutcome {
                collector: vds_domain::ids::CollectorId::new("docker"),
                status: Status::Unknown,
                message: Some("docker is not available on this host".into()),
            },
            vds_domain::metrics::CollectorOutcome {
                collector: vds_domain::ids::CollectorId::new("cpu"),
                status: Status::Healthy,
                message: None,
            },
        ];
        h.probe.respond(Ok(snapshot.clone()));
        h.monitor.collect(h.server.id).await;
        assert_eq!(
            h.servers.state(h.server.id).expect("state").status,
            Status::Healthy,
            "a host without Docker must not look degraded"
        );

        snapshot.outcomes[0].status = Status::Warning;
        h.probe.respond(Ok(snapshot));
        h.monitor.collect(h.server.id).await;
        assert_eq!(
            h.servers.state(h.server.id).expect("state").status,
            Status::Warning
        );
    }

    #[tokio::test]
    async fn the_reason_for_the_status_is_recorded_and_later_cleared() {
        // The complaint: a server marked Critical while every figure on the overview
        // looked fine, because the measurement that lost was not one of the four shown.
        let h = harness();
        let mut snapshot = healthy_snapshot(h.server.id, at(1_000));
        snapshot.memory.swap_total_bytes = Some(1_000_000_000);
        snapshot.memory.swap_used_bytes = Some(970_000_000);
        h.probe.respond(Ok(snapshot));
        h.monitor.collect(h.server.id).await;

        let state = h.servers.state(h.server.id).expect("state");
        assert_eq!(state.status, Status::Critical);
        assert_eq!(
            state.status_cause.as_ref().and_then(|c| c.metric()),
            Some(vds_domain::metrics::MetricKind::SwapUsage),
            "the status said critical and would not say why"
        );

        // And it goes away with the problem. A reason that outlived its cause would name
        // something already fixed.
        h.probe
            .respond(Ok(healthy_snapshot(h.server.id, at(2_000))));
        h.monitor.collect(h.server.id).await;

        let state = h.servers.state(h.server.id).expect("state");
        assert_eq!(state.status, Status::Healthy);
        assert_eq!(state.status_cause, None, "a stale reason survived");
    }

    #[tokio::test]
    async fn a_metrics_write_failure_does_not_lose_the_status_update() {
        // History is valuable; knowing the server is on fire is more valuable.
        let h = harness();
        h.metrics.fail_writes(true);
        let mut snapshot = healthy_snapshot(h.server.id, at(1_000));
        snapshot.cpu.total_percent = MetricValue::Available(99.0);
        h.probe.respond(Ok(snapshot));

        assert_eq!(h.monitor.collect(h.server.id).await, JobOutcome::Success);
        assert_eq!(
            h.servers.state(h.server.id).expect("state").status,
            Status::Critical
        );
    }

    #[tokio::test]
    async fn the_last_snapshot_is_kept_for_the_detail_page() {
        // Processes, containers and services are never written to the database, so this
        // cache is the only place the detail tabs can read them from.
        let h = harness();
        let mut snapshot = healthy_snapshot(h.server.id, at(1_000));
        snapshot.processes = vec![vds_domain::server::ProcessInfo {
            pid: 1,
            user: Some("root".into()),
            command: "nginx: master process".into(),
            cpu_percent: 1.5,
            memory_percent: 0.4,
            rss_bytes: Some(4_096),
        }];
        h.probe.respond(Ok(snapshot));

        assert_eq!(h.monitor.collect(h.server.id).await, JobOutcome::Success);

        let kept = h.monitor.last_snapshot(h.server.id).expect("a snapshot");
        assert_eq!(kept.processes.len(), 1);
        assert_eq!(kept.processes[0].command, "nginx: master process");
    }

    #[tokio::test]
    async fn a_server_that_has_never_been_collected_has_no_snapshot() {
        // Absent, so the UI can say "not collected yet" instead of showing an empty
        // process list that reads as "this machine is running nothing".
        let h = harness();
        assert!(h.monitor.last_snapshot(h.server.id).is_none());
    }

    #[tokio::test]
    async fn a_failed_collection_leaves_the_previous_snapshot_in_place() {
        // The tabs keep showing the last known state rather than blanking, which is what
        // a user needs while they work out why the server went quiet.
        let h = harness();
        h.probe
            .respond(Ok(healthy_snapshot(h.server.id, at(1_000))));
        h.monitor.collect(h.server.id).await;

        h.probe
            .respond(Err(TransportError::Timeout { seconds: 10 }));
        h.monitor.collect(h.server.id).await;

        assert!(h.monitor.last_snapshot(h.server.id).is_some());
    }

    #[tokio::test]
    async fn forgetting_a_server_drops_its_snapshot_too() {
        let h = harness();
        h.probe
            .respond(Ok(healthy_snapshot(h.server.id, at(1_000))));
        h.monitor.collect(h.server.id).await;
        assert!(h.monitor.last_snapshot(h.server.id).is_some());

        h.monitor.forget(h.server.id);
        assert!(h.monitor.last_snapshot(h.server.id).is_none());
    }

    #[tokio::test]
    async fn a_second_collection_replaces_the_snapshot_rather_than_accumulating() {
        let h = harness();
        h.probe
            .respond(Ok(healthy_snapshot(h.server.id, at(1_000))));
        h.monitor.collect(h.server.id).await;

        let mut later = healthy_snapshot(h.server.id, at(2_000));
        later.uptime_secs = Some(90_000);
        h.probe.respond(Ok(later));
        h.monitor.collect(h.server.id).await;

        let kept = h.monitor.last_snapshot(h.server.id).expect("a snapshot");
        assert_eq!(kept.uptime_secs, Some(90_000));
        assert_eq!(kept.collected_at, at(2_000));
    }
}
