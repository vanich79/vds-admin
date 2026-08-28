//! Persistence ports.
//!
//! Deliberately narrow: each trait exposes the queries the application layer actually
//! performs, not a generic ORM surface. That is what makes a PostgreSQL implementation
//! a contained piece of work rather than an open-ended one.

use crate::alerts::{AlertRule, AlertRuleState, Incident};
use crate::analytics::{
    AnalyticsIntegration, AnalyticsInterval, AnalyticsMetric, AnalyticsSnapshot,
    AnalyticsTimeSeries, DateRange,
};
use crate::events::{AlertSubject, EventEnvelope};
use crate::ids::{AlertRuleId, IncidentId, IntegrationId, ProviderId, ServerId, WebsiteId};
use crate::metrics::{
    MetricKind, MetricRollup, MetricSample, MetricSeries, Resolution, TimeWindow,
};
use crate::screenshot::Screenshot;
use crate::server::{Server, ServerRuntimeState};
use crate::website::{UptimeSummary, Website, WebsiteCheck, WebsiteRuntimeState};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Why a storage operation failed.
///
/// Deliberately does not expose the underlying driver's error type: the application
/// layer must not be able to match on `rusqlite::Error`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryError {
    #[error("{entity} {id} not found")]
    NotFound { entity: &'static str, id: String },
    #[error("constraint violated: {0}")]
    Conflict(String),
    #[error("storage backend failed: {0}")]
    Backend(String),
    #[error("stored data is corrupt: {0}")]
    Corrupt(String),
    #[error("migration failed: {0}")]
    Migration(String),
}

impl RepositoryError {
    pub fn not_found(entity: &'static str, id: impl ToString) -> Self {
        RepositoryError::NotFound {
            entity,
            id: id.to_string(),
        }
    }

    /// Whether retrying the operation could succeed (a lock contention, say).
    pub fn is_transient(&self) -> bool {
        matches!(self, RepositoryError::Backend(_))
    }
}

/// Servers and their derived runtime state.
#[async_trait]
pub trait ServerRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<Server>, RepositoryError>;
    async fn get(&self, id: ServerId) -> Result<Server, RepositoryError>;
    async fn save(&self, server: &Server) -> Result<(), RepositoryError>;
    async fn delete(&self, id: ServerId) -> Result<(), RepositoryError>;

    async fn load_state(&self, id: ServerId) -> Result<ServerRuntimeState, RepositoryError>;
    async fn save_state(&self, state: &ServerRuntimeState) -> Result<(), RepositoryError>;
    async fn list_states(&self) -> Result<Vec<ServerRuntimeState>, RepositoryError>;
}

/// Websites, their checks and their derived runtime state.
#[async_trait]
pub trait WebsiteRepository: Send + Sync {
    async fn list(&self) -> Result<Vec<Website>, RepositoryError>;
    async fn list_for_server(&self, server: ServerId) -> Result<Vec<Website>, RepositoryError>;
    async fn get(&self, id: WebsiteId) -> Result<Website, RepositoryError>;
    async fn save(&self, website: &Website) -> Result<(), RepositoryError>;
    async fn delete(&self, id: WebsiteId) -> Result<(), RepositoryError>;

    async fn load_state(&self, id: WebsiteId) -> Result<WebsiteRuntimeState, RepositoryError>;
    async fn save_state(&self, state: &WebsiteRuntimeState) -> Result<(), RepositoryError>;
    async fn list_states(&self) -> Result<Vec<WebsiteRuntimeState>, RepositoryError>;

    async fn record_check(&self, check: &WebsiteCheck) -> Result<(), RepositoryError>;
    async fn recent_checks(
        &self,
        id: WebsiteId,
        limit: u32,
    ) -> Result<Vec<WebsiteCheck>, RepositoryError>;

    /// Availability over a window, used for the "Uptime 24h" figure.
    async fn uptime(
        &self,
        id: WebsiteId,
        window: TimeWindow,
    ) -> Result<UptimeSummary, RepositoryError>;

    /// Deletes checks older than the cutoff. Returns how many rows went.
    async fn prune_checks(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError>;
}

/// Time-series storage, including the rollup tiers.
#[async_trait]
pub trait MetricsRepository: Send + Sync {
    /// Persists a batch of raw samples. Batched because one collection cycle produces
    /// many samples and one transaction per sample would be pathological.
    async fn record_samples(&self, samples: &[MetricSample]) -> Result<(), RepositoryError>;

    /// Reads a chart-ready series, choosing storage tier by the window's own resolution.
    async fn series(
        &self,
        server: ServerId,
        kind: MetricKind,
        window: TimeWindow,
        resolution: Resolution,
    ) -> Result<MetricSeries, RepositoryError>;

    /// The most recent raw sample for a metric, if any.
    async fn latest(
        &self,
        server: ServerId,
        kind: MetricKind,
    ) -> Result<Option<MetricSample>, RepositoryError>;

    /// Computes rollups for one tier over a window and stores them.
    ///
    /// Returns the number of buckets written.
    async fn build_rollups(
        &self,
        resolution: Resolution,
        window: TimeWindow,
    ) -> Result<u64, RepositoryError>;

    /// Reads raw rollup rows, used by the aggregation service and by tests.
    async fn rollups(
        &self,
        server: ServerId,
        kind: MetricKind,
        resolution: Resolution,
        window: TimeWindow,
    ) -> Result<Vec<MetricRollup>, RepositoryError>;

    /// The newest bucket already computed for a tier, so aggregation can resume.
    async fn last_rollup_bucket(
        &self,
        resolution: Resolution,
    ) -> Result<Option<DateTime<Utc>>, RepositoryError>;

    /// Applies retention to one tier. Returns how many rows went.
    async fn prune(
        &self,
        resolution: Resolution,
        before: DateTime<Utc>,
    ) -> Result<u64, RepositoryError>;
}

/// Analytics integrations and their cached results.
#[async_trait]
pub trait AnalyticsRepository: Send + Sync {
    async fn list_integrations(&self) -> Result<Vec<AnalyticsIntegration>, RepositoryError>;
    async fn list_integrations_for_website(
        &self,
        website: WebsiteId,
    ) -> Result<Vec<AnalyticsIntegration>, RepositoryError>;
    async fn get_integration(
        &self,
        id: IntegrationId,
    ) -> Result<AnalyticsIntegration, RepositoryError>;
    async fn save_integration(
        &self,
        integration: &AnalyticsIntegration,
    ) -> Result<(), RepositoryError>;
    async fn delete_integration(&self, id: IntegrationId) -> Result<(), RepositoryError>;

    async fn save_snapshot(&self, snapshot: &AnalyticsSnapshot) -> Result<(), RepositoryError>;

    /// The cached snapshot for a website/provider/range, if one exists.
    async fn snapshot(
        &self,
        website: WebsiteId,
        provider: &ProviderId,
        range: DateRange,
    ) -> Result<Option<AnalyticsSnapshot>, RepositoryError>;

    async fn save_time_series(&self, series: &AnalyticsTimeSeries) -> Result<(), RepositoryError>;

    async fn time_series(
        &self,
        website: WebsiteId,
        provider: &ProviderId,
        metric: AnalyticsMetric,
        interval: AnalyticsInterval,
        range: DateRange,
    ) -> Result<Option<AnalyticsTimeSeries>, RepositoryError>;

    /// Deletes cached analytics older than the cutoff.
    async fn prune(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError>;
}

/// Screenshot metadata. The image bytes live on the filesystem, not in the database.
#[async_trait]
pub trait ScreenshotRepository: Send + Sync {
    async fn get(&self, website: WebsiteId) -> Result<Option<Screenshot>, RepositoryError>;
    async fn save(&self, screenshot: &Screenshot) -> Result<(), RepositoryError>;
    async fn list(&self) -> Result<Vec<Screenshot>, RepositoryError>;
    async fn delete(&self, website: WebsiteId) -> Result<(), RepositoryError>;
}

/// Alert rules, their per-subject state, and incidents.
#[async_trait]
pub trait AlertRepository: Send + Sync {
    async fn list_rules(&self) -> Result<Vec<AlertRule>, RepositoryError>;
    async fn get_rule(&self, id: AlertRuleId) -> Result<AlertRule, RepositoryError>;
    async fn save_rule(&self, rule: &AlertRule) -> Result<(), RepositoryError>;
    async fn delete_rule(&self, id: AlertRuleId) -> Result<(), RepositoryError>;

    async fn load_rule_state(
        &self,
        rule: AlertRuleId,
        subject: AlertSubject,
    ) -> Result<Option<AlertRuleState>, RepositoryError>;
    async fn save_rule_state(&self, state: &AlertRuleState) -> Result<(), RepositoryError>;

    async fn open_incidents(&self) -> Result<Vec<Incident>, RepositoryError>;
    async fn recent_incidents(&self, limit: u32) -> Result<Vec<Incident>, RepositoryError>;
    async fn save_incident(&self, incident: &Incident) -> Result<(), RepositoryError>;
    async fn get_incident(&self, id: IncidentId) -> Result<Incident, RepositoryError>;

    /// Deletes resolved incidents older than the cutoff.
    async fn prune_incidents(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError>;
}

/// The persisted event log.
#[async_trait]
pub trait EventRepository: Send + Sync {
    async fn append(&self, event: &EventEnvelope) -> Result<(), RepositoryError>;
    async fn recent(&self, limit: u32) -> Result<Vec<EventEnvelope>, RepositoryError>;
    async fn recent_for_subject(
        &self,
        subject: AlertSubject,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>, RepositoryError>;

    /// Events in a window, used by the correlation engine.
    async fn in_window(&self, window: TimeWindow) -> Result<Vec<EventEnvelope>, RepositoryError>;

    async fn prune(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_carries_the_entity_and_id() {
        let id = ServerId::new();
        let err = RepositoryError::not_found("server", id);
        assert_eq!(err.to_string(), format!("server {id} not found"));
    }

    #[test]
    fn only_backend_failures_are_worth_retrying() {
        assert!(RepositoryError::Backend("database is locked".into()).is_transient());
        assert!(!RepositoryError::not_found("server", "x").is_transient());
        assert!(!RepositoryError::Corrupt("bad row".into()).is_transient());
    }
}
