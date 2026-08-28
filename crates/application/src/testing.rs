//! In-memory implementations of every port, for tests.
//!
//! This is the test infrastructure the brief asks for: emulating an online server, an
//! offline server, a slow server, a failing repository or an expired certificate needs
//! no container, no network and no real machine.
//!
//! Available to other crates via the `testing` feature.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use vds_domain::alerts::{AlertRule, AlertRuleState, Incident};
use vds_domain::analytics::{
    AnalyticsIntegration, AnalyticsInterval, AnalyticsMetric, AnalyticsSnapshot,
    AnalyticsTimeSeries, DateRange,
};
use vds_domain::events::{AlertSubject, EventEnvelope};
use vds_domain::ids::{
    AlertRuleId, CredentialRef, IncidentId, IntegrationId, ProviderId, ServerId, WebsiteId,
};
use vds_domain::metrics::{
    MetricKind, MetricRollup, MetricSample, MetricSeries, Resolution, SeriesPoint, TimeWindow,
};
use vds_domain::ports::*;
use vds_domain::screenshot::Screenshot;
use vds_domain::server::{Server, ServerRuntimeState, ServerSnapshot};
use vds_domain::website::{UptimeSummary, Website, WebsiteCheck, WebsiteRuntimeState};

/// A probe that returns whatever it was told to.
#[derive(Default)]
pub struct ScriptedProbe {
    response: Mutex<Option<Result<ServerSnapshot, TransportError>>>,
    /// Responses consumed in order; falls back to `response` when empty.
    queue: Mutex<Vec<Result<ServerSnapshot, TransportError>>>,
    probes: Mutex<u32>,
    /// Artificial delay, for exercising timeouts.
    delay: Mutex<Option<std::time::Duration>>,
}

impl ScriptedProbe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the response returned by every subsequent probe.
    pub fn respond(&self, response: Result<ServerSnapshot, TransportError>) {
        *self.response.lock() = Some(response);
    }

    /// Queues responses consumed one per probe, oldest first.
    pub fn respond_in_sequence(&self, responses: Vec<Result<ServerSnapshot, TransportError>>) {
        let mut queue = self.queue.lock();
        *queue = responses;
        queue.reverse();
    }

    /// Makes each probe take this long, so timeout handling can be exercised.
    pub fn set_delay(&self, delay: std::time::Duration) {
        *self.delay.lock() = Some(delay);
    }

    pub fn probe_count(&self) -> u32 {
        *self.probes.lock()
    }
}

#[async_trait]
impl ServerProbe for ScriptedProbe {
    async fn probe(
        &self,
        server: &Server,
        at: DateTime<Utc>,
    ) -> Result<ServerSnapshot, TransportError> {
        let delay = *self.delay.lock();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        *self.probes.lock() += 1;

        if let Some(queued) = self.queue.lock().pop() {
            return queued;
        }
        match self.response.lock().clone() {
            Some(response) => response,
            None => Ok(ServerSnapshot::new(server.id, at)),
        }
    }

    async fn ping(&self, _server: &Server) -> Result<(), TransportError> {
        match self.response.lock().as_ref() {
            Some(Err(err)) => Err(err.clone()),
            _ => Ok(()),
        }
    }

    async fn disconnect(&self, _server_id: ServerId) {}
}

/// In-memory server storage.
#[derive(Default)]
pub struct FakeServerRepository {
    servers: Mutex<HashMap<ServerId, Server>>,
    states: Mutex<HashMap<ServerId, ServerRuntimeState>>,
    fail: Mutex<bool>,
    /// One-shot failure, for exercising a rollback path without breaking the reads that
    /// have to keep working around it.
    fail_save_once: Mutex<bool>,
}

impl FakeServerRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, server: Server) {
        self.servers.lock().insert(server.id, server);
    }

    pub fn state(&self, id: ServerId) -> Option<ServerRuntimeState> {
        self.states.lock().get(&id).cloned()
    }

    pub fn clear(&self) {
        self.servers.lock().clear();
    }

    pub fn count(&self) -> usize {
        self.servers.lock().len()
    }

    /// Makes every operation fail, for exercising error paths.
    pub fn fail_all(&self, fail: bool) {
        *self.fail.lock() = fail;
    }

    /// Makes the *next* `save` fail, and only that one.
    pub fn fail_next_save(&self) {
        *self.fail_save_once.lock() = true;
    }

    fn check(&self) -> Result<(), RepositoryError> {
        if *self.fail.lock() {
            Err(RepositoryError::Backend("scripted failure".into()))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl ServerRepository for FakeServerRepository {
    async fn list(&self) -> Result<Vec<Server>, RepositoryError> {
        self.check()?;
        Ok(self.servers.lock().values().cloned().collect())
    }

    async fn get(&self, id: ServerId) -> Result<Server, RepositoryError> {
        self.check()?;
        self.servers
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| RepositoryError::not_found("server", id))
    }

    async fn save(&self, server: &Server) -> Result<(), RepositoryError> {
        self.check()?;
        if std::mem::take(&mut *self.fail_save_once.lock()) {
            return Err(RepositoryError::Backend("scripted save failure".into()));
        }
        self.servers.lock().insert(server.id, server.clone());
        Ok(())
    }

    async fn delete(&self, id: ServerId) -> Result<(), RepositoryError> {
        self.check()?;
        self.servers.lock().remove(&id);
        self.states.lock().remove(&id);
        Ok(())
    }

    async fn load_state(&self, id: ServerId) -> Result<ServerRuntimeState, RepositoryError> {
        self.check()?;
        Ok(self
            .states
            .lock()
            .get(&id)
            .cloned()
            .unwrap_or_else(|| ServerRuntimeState::unknown(id)))
    }

    async fn save_state(&self, state: &ServerRuntimeState) -> Result<(), RepositoryError> {
        self.check()?;
        self.states.lock().insert(state.server_id, state.clone());
        Ok(())
    }

    async fn list_states(&self) -> Result<Vec<ServerRuntimeState>, RepositoryError> {
        self.check()?;
        Ok(self.states.lock().values().cloned().collect())
    }
}

/// In-memory website storage.
#[derive(Default)]
pub struct FakeWebsiteRepository {
    websites: Mutex<HashMap<WebsiteId, Website>>,
    states: Mutex<HashMap<WebsiteId, WebsiteRuntimeState>>,
    checks: Mutex<Vec<WebsiteCheck>>,
}

impl FakeWebsiteRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, website: Website) {
        self.websites.lock().insert(website.id, website);
    }

    pub fn state(&self, id: WebsiteId) -> Option<WebsiteRuntimeState> {
        self.states.lock().get(&id).cloned()
    }

    pub fn checks(&self) -> Vec<WebsiteCheck> {
        self.checks.lock().clone()
    }

    pub fn clear(&self) {
        self.websites.lock().clear();
    }

    pub fn count(&self) -> usize {
        self.websites.lock().len()
    }
}

#[async_trait]
impl WebsiteRepository for FakeWebsiteRepository {
    async fn list(&self) -> Result<Vec<Website>, RepositoryError> {
        Ok(self.websites.lock().values().cloned().collect())
    }

    async fn list_for_server(&self, server: ServerId) -> Result<Vec<Website>, RepositoryError> {
        Ok(self
            .websites
            .lock()
            .values()
            .filter(|w| w.server_id == Some(server))
            .cloned()
            .collect())
    }

    async fn get(&self, id: WebsiteId) -> Result<Website, RepositoryError> {
        self.websites
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| RepositoryError::not_found("website", id))
    }

    async fn save(&self, website: &Website) -> Result<(), RepositoryError> {
        self.websites.lock().insert(website.id, website.clone());
        Ok(())
    }

    async fn delete(&self, id: WebsiteId) -> Result<(), RepositoryError> {
        self.websites.lock().remove(&id);
        self.states.lock().remove(&id);
        Ok(())
    }

    async fn load_state(&self, id: WebsiteId) -> Result<WebsiteRuntimeState, RepositoryError> {
        Ok(self
            .states
            .lock()
            .get(&id)
            .cloned()
            .unwrap_or_else(|| WebsiteRuntimeState::unknown(id)))
    }

    async fn save_state(&self, state: &WebsiteRuntimeState) -> Result<(), RepositoryError> {
        self.states.lock().insert(state.website_id, state.clone());
        Ok(())
    }

    async fn list_states(&self) -> Result<Vec<WebsiteRuntimeState>, RepositoryError> {
        Ok(self.states.lock().values().cloned().collect())
    }

    async fn record_check(&self, check: &WebsiteCheck) -> Result<(), RepositoryError> {
        self.checks.lock().push(check.clone());
        Ok(())
    }

    async fn recent_checks(
        &self,
        id: WebsiteId,
        limit: u32,
    ) -> Result<Vec<WebsiteCheck>, RepositoryError> {
        let mut checks: Vec<WebsiteCheck> = self
            .checks
            .lock()
            .iter()
            .filter(|c| c.website_id == id)
            .cloned()
            .collect();
        checks.sort_by_key(|c| std::cmp::Reverse(c.checked_at));
        checks.truncate(limit as usize);
        Ok(checks)
    }

    async fn uptime(
        &self,
        id: WebsiteId,
        window: TimeWindow,
    ) -> Result<UptimeSummary, RepositoryError> {
        let checks = self.checks.lock();
        let relevant: Vec<&WebsiteCheck> = checks
            .iter()
            .filter(|c| c.website_id == id && window.contains(c.checked_at))
            .collect();
        Ok(UptimeSummary {
            total_checks: relevant.len() as u32,
            successful_checks: relevant.iter().filter(|c| c.is_success()).count() as u32,
        })
    }

    async fn prune_checks(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError> {
        let mut checks = self.checks.lock();
        let before_count = checks.len();
        checks.retain(|c| c.checked_at >= before);
        Ok((before_count - checks.len()) as u64)
    }
}

/// In-memory metric storage with a working rollup implementation.
#[derive(Default)]
pub struct FakeMetricsRepository {
    samples: Mutex<Vec<MetricSample>>,
    rollups: Mutex<Vec<MetricRollup>>,
    fail_writes: Mutex<bool>,
}

impl FakeMetricsRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn samples(&self) -> Vec<MetricSample> {
        self.samples.lock().clone()
    }

    pub fn rollup_rows(&self) -> Vec<MetricRollup> {
        self.rollups.lock().clone()
    }

    /// Makes writes fail, for exercising the "history lost but status kept" path.
    pub fn fail_writes(&self, fail: bool) {
        *self.fail_writes.lock() = fail;
    }

    pub fn clear(&self) {
        self.samples.lock().clear();
        self.rollups.lock().clear();
    }
}

#[async_trait]
impl MetricsRepository for FakeMetricsRepository {
    async fn record_samples(&self, samples: &[MetricSample]) -> Result<(), RepositoryError> {
        if *self.fail_writes.lock() {
            return Err(RepositoryError::Backend("scripted write failure".into()));
        }
        self.samples.lock().extend_from_slice(samples);
        Ok(())
    }

    async fn series(
        &self,
        server: ServerId,
        kind: MetricKind,
        window: TimeWindow,
        resolution: Resolution,
    ) -> Result<MetricSeries, RepositoryError> {
        let points: Vec<SeriesPoint> = if resolution == Resolution::Raw {
            let samples = self.samples.lock();
            let mut points: Vec<SeriesPoint> = samples
                .iter()
                .filter(|s| s.server_id == server && s.kind == kind && window.contains(s.timestamp))
                .map(|s| SeriesPoint::flat(s.timestamp, s.value))
                .collect();
            points.sort_by_key(|p| p.timestamp);
            points
        } else {
            let rollups = self.rollups.lock();
            let mut points: Vec<SeriesPoint> = rollups
                .iter()
                .filter(|r| {
                    r.server_id == server
                        && r.kind == kind
                        && r.resolution == resolution
                        && window.contains(r.bucket_start)
                })
                .map(|r| SeriesPoint {
                    timestamp: r.bucket_start,
                    avg: r.avg,
                    min: r.min,
                    max: r.max,
                })
                .collect();
            points.sort_by_key(|p| p.timestamp);
            points
        };

        Ok(MetricSeries {
            kind,
            resolution,
            window,
            points,
        })
    }

    async fn latest(
        &self,
        server: ServerId,
        kind: MetricKind,
    ) -> Result<Option<MetricSample>, RepositoryError> {
        Ok(self
            .samples
            .lock()
            .iter()
            .filter(|s| s.server_id == server && s.kind == kind)
            .max_by_key(|s| s.timestamp)
            .cloned())
    }

    async fn build_rollups(
        &self,
        resolution: Resolution,
        window: TimeWindow,
    ) -> Result<u64, RepositoryError> {
        let Some(source) = resolution.source() else {
            return Ok(0);
        };

        // Group the source tier into buckets of this tier's width.
        let mut buckets: HashMap<(ServerId, MetricKind, DateTime<Utc>), Vec<(f64, f64, f64, u32)>> =
            HashMap::new();

        if source == Resolution::Raw {
            for sample in self.samples.lock().iter() {
                if !window.contains(sample.timestamp) {
                    continue;
                }
                let bucket = resolution.bucket_start(sample.timestamp);
                buckets
                    .entry((sample.server_id, sample.kind, bucket))
                    .or_default()
                    .push((sample.value, sample.value, sample.value, 1));
            }
        } else {
            for rollup in self
                .rollups
                .lock()
                .iter()
                .filter(|r| r.resolution == source)
            {
                if !window.contains(rollup.bucket_start) {
                    continue;
                }
                let bucket = resolution.bucket_start(rollup.bucket_start);
                buckets
                    .entry((rollup.server_id, rollup.kind, bucket))
                    .or_default()
                    .push((rollup.avg, rollup.min, rollup.max, rollup.count));
            }
        }

        let mut written = 0;
        let mut rollups = self.rollups.lock();
        for ((server_id, kind, bucket_start), values) in buckets {
            if values.is_empty() {
                continue;
            }
            let count: u32 = values.iter().map(|v| v.3).sum();
            let sum: f64 = values
                .iter()
                .map(|(avg, _, _, c)| avg * f64::from(*c))
                .sum();
            let min = values.iter().map(|v| v.1).fold(f64::INFINITY, f64::min);
            let max = values.iter().map(|v| v.2).fold(f64::NEG_INFINITY, f64::max);
            let avg = if count > 0 {
                sum / f64::from(count)
            } else {
                0.0
            };

            rollups.retain(|r| {
                !(r.server_id == server_id
                    && r.kind == kind
                    && r.resolution == resolution
                    && r.bucket_start == bucket_start)
            });
            rollups.push(MetricRollup {
                server_id,
                kind,
                resolution,
                bucket_start,
                min,
                max,
                avg,
                sum,
                count,
            });
            written += 1;
        }
        Ok(written)
    }

    async fn rollups(
        &self,
        server: ServerId,
        kind: MetricKind,
        resolution: Resolution,
        window: TimeWindow,
    ) -> Result<Vec<MetricRollup>, RepositoryError> {
        let mut rows: Vec<MetricRollup> = self
            .rollups
            .lock()
            .iter()
            .filter(|r| {
                r.server_id == server
                    && r.kind == kind
                    && r.resolution == resolution
                    && window.contains(r.bucket_start)
            })
            .cloned()
            .collect();
        rows.sort_by_key(|r| r.bucket_start);
        Ok(rows)
    }

    async fn last_rollup_bucket(
        &self,
        resolution: Resolution,
    ) -> Result<Option<DateTime<Utc>>, RepositoryError> {
        Ok(self
            .rollups
            .lock()
            .iter()
            .filter(|r| r.resolution == resolution)
            .map(|r| r.bucket_start)
            .max())
    }

    async fn prune(
        &self,
        resolution: Resolution,
        before: DateTime<Utc>,
    ) -> Result<u64, RepositoryError> {
        if resolution == Resolution::Raw {
            let mut samples = self.samples.lock();
            let count = samples.len();
            samples.retain(|s| s.timestamp >= before);
            Ok((count - samples.len()) as u64)
        } else {
            let mut rollups = self.rollups.lock();
            let count = rollups.len();
            rollups.retain(|r| !(r.resolution == resolution && r.bucket_start < before));
            Ok((count - rollups.len()) as u64)
        }
    }
}

/// In-memory alert storage.
#[derive(Default)]
pub struct FakeAlertRepository {
    rules: Mutex<HashMap<AlertRuleId, AlertRule>>,
    states: Mutex<HashMap<(AlertRuleId, AlertSubject), AlertRuleState>>,
    incidents: Mutex<HashMap<IncidentId, Incident>>,
}

impl FakeAlertRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_rule(&self, rule: AlertRule) {
        self.rules.lock().insert(rule.id, rule);
    }

    pub fn incidents(&self) -> Vec<Incident> {
        self.incidents.lock().values().cloned().collect()
    }
}

#[async_trait]
impl AlertRepository for FakeAlertRepository {
    async fn list_rules(&self) -> Result<Vec<AlertRule>, RepositoryError> {
        Ok(self.rules.lock().values().cloned().collect())
    }

    async fn get_rule(&self, id: AlertRuleId) -> Result<AlertRule, RepositoryError> {
        self.rules
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| RepositoryError::not_found("alert rule", id))
    }

    async fn save_rule(&self, rule: &AlertRule) -> Result<(), RepositoryError> {
        self.rules.lock().insert(rule.id, rule.clone());
        Ok(())
    }

    async fn delete_rule(&self, id: AlertRuleId) -> Result<(), RepositoryError> {
        self.rules.lock().remove(&id);
        Ok(())
    }

    async fn load_rule_state(
        &self,
        rule: AlertRuleId,
        subject: AlertSubject,
    ) -> Result<Option<AlertRuleState>, RepositoryError> {
        Ok(self.states.lock().get(&(rule, subject)).cloned())
    }

    async fn save_rule_state(&self, state: &AlertRuleState) -> Result<(), RepositoryError> {
        self.states
            .lock()
            .insert((state.rule_id, state.subject), state.clone());
        Ok(())
    }

    async fn open_incidents(&self) -> Result<Vec<Incident>, RepositoryError> {
        Ok(self
            .incidents
            .lock()
            .values()
            .filter(|i| i.is_open())
            .cloned()
            .collect())
    }

    async fn recent_incidents(&self, limit: u32) -> Result<Vec<Incident>, RepositoryError> {
        let mut incidents: Vec<Incident> = self.incidents.lock().values().cloned().collect();
        incidents.sort_by_key(|i| std::cmp::Reverse(i.opened_at));
        incidents.truncate(limit as usize);
        Ok(incidents)
    }

    async fn save_incident(&self, incident: &Incident) -> Result<(), RepositoryError> {
        self.incidents.lock().insert(incident.id, incident.clone());
        Ok(())
    }

    async fn get_incident(&self, id: IncidentId) -> Result<Incident, RepositoryError> {
        self.incidents
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| RepositoryError::not_found("incident", id))
    }

    async fn prune_incidents(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError> {
        let mut incidents = self.incidents.lock();
        let count = incidents.len();
        incidents.retain(|_, i| i.resolved_at.is_none() || i.opened_at >= before);
        Ok((count - incidents.len()) as u64)
    }
}

/// In-memory event log.
#[derive(Default)]
pub struct FakeEventRepository {
    events: Mutex<Vec<EventEnvelope>>,
}

impl FakeEventRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn all(&self) -> Vec<EventEnvelope> {
        self.events.lock().clone()
    }
}

#[async_trait]
impl EventRepository for FakeEventRepository {
    async fn append(&self, event: &EventEnvelope) -> Result<(), RepositoryError> {
        self.events.lock().push(event.clone());
        Ok(())
    }

    async fn recent(&self, limit: u32) -> Result<Vec<EventEnvelope>, RepositoryError> {
        let mut events = self.events.lock().clone();
        events.sort_by_key(|e| std::cmp::Reverse(e.occurred_at));
        events.truncate(limit as usize);
        Ok(events)
    }

    async fn recent_for_subject(
        &self,
        subject: AlertSubject,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>, RepositoryError> {
        let mut events: Vec<EventEnvelope> = self
            .events
            .lock()
            .iter()
            .filter(|e| e.event.subject() == Some(subject))
            .cloned()
            .collect();
        events.sort_by_key(|e| std::cmp::Reverse(e.occurred_at));
        events.truncate(limit as usize);
        Ok(events)
    }

    async fn in_window(&self, window: TimeWindow) -> Result<Vec<EventEnvelope>, RepositoryError> {
        let mut events: Vec<EventEnvelope> = self
            .events
            .lock()
            .iter()
            .filter(|e| window.contains(e.occurred_at))
            .cloned()
            .collect();
        events.sort_by_key(|e| e.occurred_at);
        Ok(events)
    }

    async fn prune(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError> {
        let mut events = self.events.lock();
        let count = events.len();
        events.retain(|e| e.occurred_at >= before);
        Ok((count - events.len()) as u64)
    }
}

/// In-memory analytics storage.
#[derive(Default)]
pub struct FakeAnalyticsRepository {
    integrations: Mutex<HashMap<IntegrationId, AnalyticsIntegration>>,
    snapshots: Mutex<Vec<AnalyticsSnapshot>>,
    series: Mutex<Vec<AnalyticsTimeSeries>>,
}

impl FakeAnalyticsRepository {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, integration: AnalyticsIntegration) {
        self.integrations.lock().insert(integration.id, integration);
    }

    pub fn snapshot_count(&self) -> usize {
        self.snapshots.lock().len()
    }
}

#[async_trait]
impl AnalyticsRepository for FakeAnalyticsRepository {
    async fn list_integrations(&self) -> Result<Vec<AnalyticsIntegration>, RepositoryError> {
        Ok(self.integrations.lock().values().cloned().collect())
    }

    async fn list_integrations_for_website(
        &self,
        website: WebsiteId,
    ) -> Result<Vec<AnalyticsIntegration>, RepositoryError> {
        Ok(self
            .integrations
            .lock()
            .values()
            .filter(|i| i.website_id == website)
            .cloned()
            .collect())
    }

    async fn get_integration(
        &self,
        id: IntegrationId,
    ) -> Result<AnalyticsIntegration, RepositoryError> {
        self.integrations
            .lock()
            .get(&id)
            .cloned()
            .ok_or_else(|| RepositoryError::not_found("integration", id))
    }

    async fn save_integration(
        &self,
        integration: &AnalyticsIntegration,
    ) -> Result<(), RepositoryError> {
        self.integrations
            .lock()
            .insert(integration.id, integration.clone());
        Ok(())
    }

    async fn delete_integration(&self, id: IntegrationId) -> Result<(), RepositoryError> {
        self.integrations.lock().remove(&id);
        Ok(())
    }

    async fn save_snapshot(&self, snapshot: &AnalyticsSnapshot) -> Result<(), RepositoryError> {
        let mut snapshots = self.snapshots.lock();
        snapshots.retain(|s| {
            !(s.website_id == snapshot.website_id
                && s.provider == snapshot.provider
                && s.range == snapshot.range)
        });
        snapshots.push(snapshot.clone());
        Ok(())
    }

    async fn snapshot(
        &self,
        website: WebsiteId,
        provider: &ProviderId,
        range: DateRange,
    ) -> Result<Option<AnalyticsSnapshot>, RepositoryError> {
        Ok(self
            .snapshots
            .lock()
            .iter()
            .find(|s| s.website_id == website && s.provider == *provider && s.range == range)
            .cloned())
    }

    async fn save_time_series(&self, series: &AnalyticsTimeSeries) -> Result<(), RepositoryError> {
        let mut stored = self.series.lock();
        stored.retain(|s| {
            !(s.website_id == series.website_id
                && s.provider == series.provider
                && s.metric == series.metric
                && s.interval == series.interval
                && s.range == series.range)
        });
        stored.push(series.clone());
        Ok(())
    }

    async fn time_series(
        &self,
        website: WebsiteId,
        provider: &ProviderId,
        metric: AnalyticsMetric,
        interval: AnalyticsInterval,
        range: DateRange,
    ) -> Result<Option<AnalyticsTimeSeries>, RepositoryError> {
        Ok(self
            .series
            .lock()
            .iter()
            .find(|s| {
                s.website_id == website
                    && s.provider == *provider
                    && s.metric == metric
                    && s.interval == interval
                    && s.range == range
            })
            .cloned())
    }

    async fn prune(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError> {
        let mut snapshots = self.snapshots.lock();
        let count = snapshots.len();
        snapshots.retain(|s| s.fetched_at >= before);
        Ok((count - snapshots.len()) as u64)
    }
}

/// In-memory screenshot metadata storage.
#[derive(Default)]
pub struct FakeScreenshotRepository {
    screenshots: Mutex<HashMap<WebsiteId, Screenshot>>,
}

impl FakeScreenshotRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ScreenshotRepository for FakeScreenshotRepository {
    async fn get(&self, website: WebsiteId) -> Result<Option<Screenshot>, RepositoryError> {
        Ok(self.screenshots.lock().get(&website).cloned())
    }

    async fn save(&self, screenshot: &Screenshot) -> Result<(), RepositoryError> {
        self.screenshots
            .lock()
            .insert(screenshot.website_id, screenshot.clone());
        Ok(())
    }

    async fn list(&self) -> Result<Vec<Screenshot>, RepositoryError> {
        Ok(self.screenshots.lock().values().cloned().collect())
    }

    async fn delete(&self, website: WebsiteId) -> Result<(), RepositoryError> {
        self.screenshots.lock().remove(&website);
        Ok(())
    }
}

/// In-memory secret store.
///
/// Fine for tests; obviously not for anything else, which is why it lives behind the
/// `testing` feature and is never compiled into a release binary.
#[derive(Default)]
pub struct FakeSecretStore {
    secrets: Mutex<HashMap<(CredentialRef, SecretKind), Vec<u8>>>,
}

impl FakeSecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, reference: CredentialRef, kind: SecretKind, value: &str) {
        self.secrets
            .lock()
            .insert((reference, kind), value.as_bytes().to_vec());
    }

    /// How many secrets are held. Used to prove a rejected form leaves nothing behind.
    pub fn len(&self) -> usize {
        self.secrets.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.lock().is_empty()
    }
}

#[async_trait]
impl SecretStore for FakeSecretStore {
    async fn store(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
        secret: Secret,
    ) -> Result<(), SecretStoreError> {
        self.secrets
            .lock()
            .insert((reference, kind), secret.expose().to_vec());
        Ok(())
    }

    async fn retrieve(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<Secret, SecretStoreError> {
        self.secrets
            .lock()
            .get(&(reference, kind))
            .map(|bytes| Secret::new(bytes.clone()))
            .ok_or_else(|| SecretStoreError::NotFound(format!("{reference}/{}", kind.as_str())))
    }

    async fn delete(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<(), SecretStoreError> {
        self.secrets.lock().remove(&(reference, kind));
        Ok(())
    }

    async fn contains(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<bool, SecretStoreError> {
        Ok(self.secrets.lock().contains_key(&(reference, kind)))
    }

    async fn delete_all(&self, reference: CredentialRef) -> Result<(), SecretStoreError> {
        self.secrets.lock().retain(|(r, _), _| *r != reference);
        Ok(())
    }

    fn backend_description(&self) -> String {
        "in-memory (test only)".to_owned()
    }
}

/// Records every notification it is asked to deliver.
#[derive(Default)]
pub struct RecordingNotificationProvider {
    delivered: Mutex<Vec<vds_domain::alerts::Notification>>,
    available: Mutex<bool>,
    fail: Mutex<bool>,
}

impl RecordingNotificationProvider {
    pub fn new() -> Self {
        Self {
            delivered: Mutex::new(Vec::new()),
            available: Mutex::new(true),
            fail: Mutex::new(false),
        }
    }

    pub fn delivered(&self) -> Vec<vds_domain::alerts::Notification> {
        self.delivered.lock().clone()
    }

    pub fn set_available(&self, available: bool) {
        *self.available.lock() = available;
    }

    pub fn fail_delivery(&self, fail: bool) {
        *self.fail.lock() = fail;
    }
}

#[async_trait]
impl NotificationProvider for RecordingNotificationProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("recording")
    }

    fn display_name(&self) -> &'static str {
        "Recording (test)"
    }

    fn capabilities(&self) -> NotificationCapabilities {
        NotificationCapabilities {
            supports_rich_body: true,
            supports_sound: false,
            supports_delivery_confirmation: true,
            max_body_chars: None,
        }
    }

    async fn is_available(&self) -> bool {
        *self.available.lock()
    }

    async fn notify(
        &self,
        notification: &vds_domain::alerts::Notification,
    ) -> Result<(), NotificationError> {
        if *self.fail.lock() {
            return Err(NotificationError::Delivery("scripted failure".into()));
        }
        self.delivered.lock().push(notification.clone());
        Ok(())
    }
}

/// Shared bundle of fakes, so tests do not repeat the wiring.
pub struct Fakes {
    pub servers: Arc<FakeServerRepository>,
    pub websites: Arc<FakeWebsiteRepository>,
    pub metrics: Arc<FakeMetricsRepository>,
    pub alerts: Arc<FakeAlertRepository>,
    pub events_log: Arc<FakeEventRepository>,
    pub analytics: Arc<FakeAnalyticsRepository>,
    pub screenshots: Arc<FakeScreenshotRepository>,
    pub secrets: Arc<FakeSecretStore>,
    pub bus: Arc<RecordingEventPublisher>,
}

impl Fakes {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(FakeServerRepository::new()),
            websites: Arc::new(FakeWebsiteRepository::new()),
            metrics: Arc::new(FakeMetricsRepository::new()),
            alerts: Arc::new(FakeAlertRepository::new()),
            events_log: Arc::new(FakeEventRepository::new()),
            analytics: Arc::new(FakeAnalyticsRepository::new()),
            screenshots: Arc::new(FakeScreenshotRepository::new()),
            secrets: Arc::new(FakeSecretStore::new()),
            bus: Arc::new(RecordingEventPublisher::new()),
        }
    }
}

impl Default for Fakes {
    fn default() -> Self {
        Self::new()
    }
}
