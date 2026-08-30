//! The internal event vocabulary.
//!
//! Producers publish; subscribers react. Adding a consumer — webhooks, audit logging,
//! automation, correlation — must never require changing a producer, which is why the
//! event type is a plain data enum with no behaviour attached.

use crate::analytics::AnalyticsMetric;
use crate::ids::{AlertRuleId, EventId, IncidentId, ServerId, WebsiteId};
use crate::metrics::MetricKind;
use crate::status::Status;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Something that happened in the system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DomainEvent {
    ServerStatusChanged {
        server_id: ServerId,
        from: Status,
        to: Status,
        reason: Option<String>,
    },
    ServerMetricsCollected {
        server_id: ServerId,
        /// Number of metrics persisted, used by the UI to decide whether to refresh.
        metric_count: usize,
    },
    ServerCollectionFailed {
        server_id: ServerId,
        consecutive_failures: u32,
        error: String,
    },
    WebsiteStatusChanged {
        website_id: WebsiteId,
        from: Status,
        to: Status,
        reason: Option<String>,
    },
    WebsiteChecked {
        website_id: WebsiteId,
        status: Status,
        response_ms: Option<u32>,
    },
    MetricThresholdExceeded {
        server_id: ServerId,
        metric: MetricKind,
        value: f64,
        threshold: f64,
        status: Status,
    },
    SslExpiringSoon {
        website_id: WebsiteId,
        days_remaining: i64,
    },
    TrafficAnomalyDetected {
        website_id: WebsiteId,
        metric: AnalyticsMetric,
        current: f64,
        baseline: f64,
        change_percent: f64,
    },
    AnalyticsUpdated {
        website_id: WebsiteId,
        provider: crate::ids::ProviderId,
    },
    AnalyticsRefreshFailed {
        website_id: WebsiteId,
        provider: crate::ids::ProviderId,
        error: String,
    },
    ScreenshotUpdated {
        website_id: WebsiteId,
    },
    ScreenshotFailed {
        website_id: WebsiteId,
        error: String,
    },
    IncidentOpened {
        incident_id: IncidentId,
        rule_id: AlertRuleId,
        subject: AlertSubject,
        severity: Status,
        summary: String,
    },
    IncidentResolved {
        incident_id: IncidentId,
        rule_id: AlertRuleId,
        subject: AlertSubject,
    },
    ContainerStateChanged {
        server_id: ServerId,
        container: String,
        state: String,
    },
    ServiceStateChanged {
        server_id: ServerId,
        service: String,
        state: String,
    },
    /// Someone changed a file on a server through this application.
    ///
    /// The audit trail for the one part of the product that writes. Everything else here
    /// observes; if a stolen credential is ever used to alter a configuration file, this
    /// is the record that says which file, when, and on which machine.
    FileChanged {
        server_id: ServerId,
        path: String,
        action: FileAction,
    },
}

/// What was done to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
    Written,
    Deleted,
    DirectoryCreated,
}

impl FileAction {
    pub fn as_str(self) -> &'static str {
        match self {
            FileAction::Written => "written",
            FileAction::Deleted => "deleted",
            FileAction::DirectoryCreated => "directory_created",
        }
    }
}

impl DomainEvent {
    /// Stable machine-readable discriminant, used for persistence and filtering.
    pub fn kind(&self) -> &'static str {
        match self {
            DomainEvent::ServerStatusChanged { .. } => "server_status_changed",
            DomainEvent::ServerMetricsCollected { .. } => "server_metrics_collected",
            DomainEvent::ServerCollectionFailed { .. } => "server_collection_failed",
            DomainEvent::WebsiteStatusChanged { .. } => "website_status_changed",
            DomainEvent::WebsiteChecked { .. } => "website_checked",
            DomainEvent::MetricThresholdExceeded { .. } => "metric_threshold_exceeded",
            DomainEvent::SslExpiringSoon { .. } => "ssl_expiring_soon",
            DomainEvent::TrafficAnomalyDetected { .. } => "traffic_anomaly_detected",
            DomainEvent::AnalyticsUpdated { .. } => "analytics_updated",
            DomainEvent::AnalyticsRefreshFailed { .. } => "analytics_refresh_failed",
            DomainEvent::ScreenshotUpdated { .. } => "screenshot_updated",
            DomainEvent::ScreenshotFailed { .. } => "screenshot_failed",
            DomainEvent::IncidentOpened { .. } => "incident_opened",
            DomainEvent::IncidentResolved { .. } => "incident_resolved",
            DomainEvent::ContainerStateChanged { .. } => "container_state_changed",
            DomainEvent::ServiceStateChanged { .. } => "service_state_changed",
            DomainEvent::FileChanged { .. } => "file_changed",
        }
    }

    /// Severity for the event log and the UI's "recent events" panel.
    pub fn severity(&self) -> Status {
        match self {
            DomainEvent::ServerStatusChanged { to, .. }
            | DomainEvent::WebsiteStatusChanged { to, .. } => *to,
            DomainEvent::MetricThresholdExceeded { status, .. } => *status,
            DomainEvent::IncidentOpened { severity, .. } => *severity,
            DomainEvent::ServerCollectionFailed { .. }
            | DomainEvent::AnalyticsRefreshFailed { .. }
            | DomainEvent::ScreenshotFailed { .. } => Status::Warning,
            DomainEvent::SslExpiringSoon { days_remaining, .. } => {
                if *days_remaining <= 3 {
                    Status::Critical
                } else {
                    Status::Warning
                }
            }
            DomainEvent::TrafficAnomalyDetected { .. } => Status::Warning,
            DomainEvent::IncidentResolved { .. }
            | DomainEvent::WebsiteChecked { .. }
            | DomainEvent::ServerMetricsCollected { .. }
            | DomainEvent::AnalyticsUpdated { .. }
            | DomainEvent::ScreenshotUpdated { .. } => Status::Healthy,
            DomainEvent::ContainerStateChanged { .. }
            | DomainEvent::ServiceStateChanged { .. }
            | DomainEvent::FileChanged { .. } => Status::Unknown,
        }
    }

    /// The server or website this event concerns, when it concerns one.
    pub fn subject(&self) -> Option<AlertSubject> {
        match self {
            DomainEvent::ServerStatusChanged { server_id, .. }
            | DomainEvent::ServerMetricsCollected { server_id, .. }
            | DomainEvent::ServerCollectionFailed { server_id, .. }
            | DomainEvent::MetricThresholdExceeded { server_id, .. }
            | DomainEvent::ContainerStateChanged { server_id, .. }
            | DomainEvent::ServiceStateChanged { server_id, .. }
            | DomainEvent::FileChanged { server_id, .. } => Some(AlertSubject::Server(*server_id)),
            DomainEvent::WebsiteStatusChanged { website_id, .. }
            | DomainEvent::WebsiteChecked { website_id, .. }
            | DomainEvent::SslExpiringSoon { website_id, .. }
            | DomainEvent::TrafficAnomalyDetected { website_id, .. }
            | DomainEvent::AnalyticsUpdated { website_id, .. }
            | DomainEvent::AnalyticsRefreshFailed { website_id, .. }
            | DomainEvent::ScreenshotUpdated { website_id }
            | DomainEvent::ScreenshotFailed { website_id, .. } => {
                Some(AlertSubject::Website(*website_id))
            }
            DomainEvent::IncidentOpened { subject, .. }
            | DomainEvent::IncidentResolved { subject, .. } => Some(*subject),
        }
    }

    /// Whether this event is worth showing a user in an activity feed.
    ///
    /// Routine successes are published for subscribers that want them (the UI refresh
    /// path) but would drown a human-facing log.
    pub fn is_noteworthy(&self) -> bool {
        !matches!(
            self,
            DomainEvent::WebsiteChecked { .. } | DomainEvent::ServerMetricsCollected { .. }
        )
    }
}

/// A published event with its metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub id: EventId,
    pub occurred_at: DateTime<Utc>,
    pub event: DomainEvent,
}

impl EventEnvelope {
    pub fn new(event: DomainEvent, occurred_at: DateTime<Utc>) -> Self {
        Self {
            id: EventId::new(),
            occurred_at,
            event,
        }
    }
}

/// What an alert or incident is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum AlertSubject {
    Server(ServerId),
    Website(WebsiteId),
}

impl AlertSubject {
    pub fn server_id(&self) -> Option<ServerId> {
        match self {
            AlertSubject::Server(id) => Some(*id),
            AlertSubject::Website(_) => None,
        }
    }

    pub fn website_id(&self) -> Option<WebsiteId> {
        match self {
            AlertSubject::Website(id) => Some(*id),
            AlertSubject::Server(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kinds_are_unique() {
        let events = sample_events();
        let mut kinds: Vec<&str> = events.iter().map(DomainEvent::kind).collect();
        let count = kinds.len();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), count, "two events share a discriminant");
    }

    #[test]
    fn every_event_reports_a_subject() {
        for event in sample_events() {
            assert!(event.subject().is_some(), "{} has no subject", event.kind());
        }
    }

    #[test]
    fn routine_successes_are_not_noteworthy() {
        let checked = DomainEvent::WebsiteChecked {
            website_id: WebsiteId::new(),
            status: Status::Healthy,
            response_ms: Some(10),
        };
        assert!(!checked.is_noteworthy());

        let changed = DomainEvent::WebsiteStatusChanged {
            website_id: WebsiteId::new(),
            from: Status::Healthy,
            to: Status::Offline,
            reason: None,
        };
        assert!(changed.is_noteworthy());
    }

    #[test]
    fn ssl_severity_escalates_as_expiry_approaches() {
        let soon = DomainEvent::SslExpiringSoon {
            website_id: WebsiteId::new(),
            days_remaining: 10,
        };
        assert_eq!(soon.severity(), Status::Warning);

        let imminent = DomainEvent::SslExpiringSoon {
            website_id: WebsiteId::new(),
            days_remaining: 2,
        };
        assert_eq!(imminent.severity(), Status::Critical);
    }

    #[test]
    fn events_round_trip_through_json() {
        for event in sample_events() {
            let json = serde_json::to_string(&event).expect("event serialises");
            let back: DomainEvent = serde_json::from_str(&json).expect("event deserialises");
            assert_eq!(back, event);
        }
    }

    fn sample_events() -> Vec<DomainEvent> {
        let server = ServerId::new();
        let website = WebsiteId::new();
        let provider = crate::ids::ProviderId::new("yandex_metrica");
        vec![
            DomainEvent::ServerStatusChanged {
                server_id: server,
                from: Status::Healthy,
                to: Status::Offline,
                reason: Some("timeout".into()),
            },
            DomainEvent::ServerMetricsCollected {
                server_id: server,
                metric_count: 8,
            },
            DomainEvent::ServerCollectionFailed {
                server_id: server,
                consecutive_failures: 2,
                error: "connection refused".into(),
            },
            DomainEvent::WebsiteStatusChanged {
                website_id: website,
                from: Status::Healthy,
                to: Status::Critical,
                reason: None,
            },
            DomainEvent::WebsiteChecked {
                website_id: website,
                status: Status::Healthy,
                response_ms: Some(120),
            },
            DomainEvent::MetricThresholdExceeded {
                server_id: server,
                metric: MetricKind::CpuUsage,
                value: 97.0,
                threshold: 95.0,
                status: Status::Critical,
            },
            DomainEvent::SslExpiringSoon {
                website_id: website,
                days_remaining: 7,
            },
            DomainEvent::TrafficAnomalyDetected {
                website_id: website,
                metric: AnalyticsMetric::Visitors,
                current: 6_500.0,
                baseline: 10_000.0,
                change_percent: -35.0,
            },
            DomainEvent::AnalyticsUpdated {
                website_id: website,
                provider: provider.clone(),
            },
            DomainEvent::AnalyticsRefreshFailed {
                website_id: website,
                provider,
                error: "429".into(),
            },
            DomainEvent::ScreenshotUpdated {
                website_id: website,
            },
            DomainEvent::ScreenshotFailed {
                website_id: website,
                error: "no browser".into(),
            },
            DomainEvent::IncidentOpened {
                incident_id: IncidentId::new(),
                rule_id: AlertRuleId::new(),
                subject: AlertSubject::Server(server),
                severity: Status::Critical,
                summary: "CPU above 90% for 5 minutes".into(),
            },
            DomainEvent::IncidentResolved {
                incident_id: IncidentId::new(),
                rule_id: AlertRuleId::new(),
                subject: AlertSubject::Server(server),
            },
            DomainEvent::ContainerStateChanged {
                server_id: server,
                container: "web".into(),
                state: "exited".into(),
            },
            DomainEvent::ServiceStateChanged {
                server_id: server,
                service: "nginx".into(),
                state: "failed".into(),
            },
        ]
    }
}
