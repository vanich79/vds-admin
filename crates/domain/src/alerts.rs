//! Alert rules and the incidents they raise.
//!
//! A rule is a *declarative* condition plus a duration it must hold for. Evaluation
//! (in `vds-application`) is a pure function of a rule, the current observation and the
//! rule's previous state, which is what makes the whole alerting path testable without
//! a clock or a network.

use crate::analytics::AnalyticsMetric;
use crate::events::AlertSubject;
use crate::ids::{AlertRuleId, IncidentId, ProviderId};
use crate::metrics::MetricKind;
use crate::status::{Status, ThresholdDirection};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// What must be true for a rule to fire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertCondition {
    /// A server metric crosses a threshold, e.g. `CPU > 90%`.
    MetricThreshold {
        metric: MetricKind,
        direction: ThresholdDirection,
        value: f64,
    },
    /// A server has been declared offline.
    ServerOffline,
    /// A website has been declared offline.
    WebsiteOffline,
    /// A website returned something other than its expected response.
    WebsiteUnexpectedResponse,
    /// A certificate is within `days` of expiring.
    SslExpiringWithin { days: i64 },
    /// Any Docker container on the server is not running.
    ContainerNotRunning {
        /// Restrict to one container by name; `None` watches all of them.
        name: Option<String>,
    },
    /// A systemd unit is in the `failed` state.
    ServiceFailed { name: Option<String> },
    /// Traffic dropped by at least `percent` against the previous period.
    TrafficDrop {
        metric: AnalyticsMetric,
        percent: f64,
    },
}

impl AlertCondition {
    /// Which kind of subject this condition can apply to.
    pub fn applies_to_servers(&self) -> bool {
        matches!(
            self,
            AlertCondition::MetricThreshold { .. }
                | AlertCondition::ServerOffline
                | AlertCondition::ContainerNotRunning { .. }
                | AlertCondition::ServiceFailed { .. }
        )
    }

    pub fn applies_to_websites(&self) -> bool {
        matches!(
            self,
            AlertCondition::WebsiteOffline
                | AlertCondition::WebsiteUnexpectedResponse
                | AlertCondition::SslExpiringWithin { .. }
                | AlertCondition::TrafficDrop { .. }
        )
    }

    /// Human-readable summary used in notifications and the rules list.
    pub fn describe(&self) -> String {
        match self {
            AlertCondition::MetricThreshold {
                metric,
                direction,
                value,
            } => {
                let arrow = match direction {
                    ThresholdDirection::Above => ">",
                    ThresholdDirection::Below => "<",
                };
                format!("{} {} {}", metric.label(), arrow, value)
            }
            AlertCondition::ServerOffline => "Server offline".to_owned(),
            AlertCondition::WebsiteOffline => "Website offline".to_owned(),
            AlertCondition::WebsiteUnexpectedResponse => "Unexpected HTTP response".to_owned(),
            AlertCondition::SslExpiringWithin { days } => {
                format!("SSL expires in less than {days} days")
            }
            AlertCondition::ContainerNotRunning { name } => match name {
                Some(n) => format!("Container {n} not running"),
                None => "Any container not running".to_owned(),
            },
            AlertCondition::ServiceFailed { name } => match name {
                Some(n) => format!("Service {n} failed"),
                None => "Any service failed".to_owned(),
            },
            AlertCondition::TrafficDrop { metric, percent } => {
                format!("{} dropped by {}%", metric.label(), percent)
            }
        }
    }
}

/// Which subjects a rule watches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertScope {
    /// Every server, or every website, depending on the condition.
    All,
    /// One specific subject.
    Subject(AlertSubject),
    /// Any subject carrying this tag.
    Tag(String),
}

impl AlertScope {
    /// Whether a subject with the given tags is in scope.
    pub fn matches(&self, subject: AlertSubject, tags: &[String]) -> bool {
        match self {
            AlertScope::All => true,
            AlertScope::Subject(target) => *target == subject,
            AlertScope::Tag(tag) => tags.iter().any(|t| t == tag),
        }
    }
}

/// A user-configured alert rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRule {
    pub id: AlertRuleId,
    pub name: String,
    pub enabled: bool,
    pub condition: AlertCondition,
    pub scope: AlertScope,
    /// How long the condition must hold continuously before an incident opens.
    ///
    /// Zero fires immediately. This is what expresses "CPU > 90% for 5 minutes".
    pub for_duration_secs: u32,
    pub severity: Status,
    /// Minimum gap between repeat notifications for the same open incident.
    pub renotify_after_secs: u32,
    /// Providers to notify. Empty means every enabled provider.
    pub notify_via: Vec<ProviderId>,
    pub created_at: DateTime<Utc>,
}

pub const DEFAULT_RENOTIFY_SECS: u32 = 3_600;

impl AlertRule {
    pub fn new(
        name: impl Into<String>,
        condition: AlertCondition,
        severity: Status,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: AlertRuleId::new(),
            name: name.into(),
            enabled: true,
            condition,
            scope: AlertScope::All,
            for_duration_secs: 0,
            severity,
            renotify_after_secs: DEFAULT_RENOTIFY_SECS,
            notify_via: Vec::new(),
            created_at: now,
        }
    }

    pub fn for_duration(&self) -> Duration {
        Duration::seconds(i64::from(self.for_duration_secs))
    }

    pub fn renotify_after(&self) -> Duration {
        Duration::seconds(i64::from(self.renotify_after_secs))
    }

    pub fn validate(&self) -> Result<(), AlertRuleValidationError> {
        if self.name.trim().is_empty() {
            return Err(AlertRuleValidationError::EmptyName);
        }
        if !self.severity.is_problem() {
            return Err(AlertRuleValidationError::NonProblemSeverity(self.severity));
        }
        if let AlertCondition::TrafficDrop { percent, .. } = &self.condition
            && !(0.0..=100.0).contains(percent)
        {
            return Err(AlertRuleValidationError::InvalidPercentage(*percent));
        }
        if let AlertCondition::SslExpiringWithin { days } = &self.condition
            && *days <= 0
        {
            return Err(AlertRuleValidationError::InvalidDayCount(*days));
        }
        // A rule scoped to a server but conditioned on a website (or vice versa) can
        // never fire; catching it here beats a silently dead rule.
        if let AlertScope::Subject(subject) = &self.scope {
            let compatible = match subject {
                AlertSubject::Server(_) => self.condition.applies_to_servers(),
                AlertSubject::Website(_) => self.condition.applies_to_websites(),
            };
            if !compatible {
                return Err(AlertRuleValidationError::ScopeConditionMismatch);
            }
        }
        Ok(())
    }

    /// The rules a fresh installation starts with.
    ///
    /// They cover the cases the brief lists as examples, and are ordinary rules the user
    /// can edit or delete.
    pub fn defaults(now: DateTime<Utc>) -> Vec<AlertRule> {
        let mut cpu = AlertRule::new(
            "CPU above 90% for 5 minutes",
            AlertCondition::MetricThreshold {
                metric: MetricKind::CpuUsage,
                direction: ThresholdDirection::Above,
                value: 90.0,
            },
            Status::Warning,
            now,
        );
        cpu.for_duration_secs = 300;

        let memory = AlertRule::new(
            "RAM above 95%",
            AlertCondition::MetricThreshold {
                metric: MetricKind::MemoryUsage,
                direction: ThresholdDirection::Above,
                value: 95.0,
            },
            Status::Critical,
            now,
        );

        let disk = AlertRule::new(
            "Disk above 90%",
            AlertCondition::MetricThreshold {
                metric: MetricKind::DiskUsage,
                direction: ThresholdDirection::Above,
                value: 90.0,
            },
            Status::Critical,
            now,
        );

        vec![
            cpu,
            memory,
            disk,
            AlertRule::new(
                "Server offline",
                AlertCondition::ServerOffline,
                Status::Critical,
                now,
            ),
            AlertRule::new(
                "Website offline",
                AlertCondition::WebsiteOffline,
                Status::Critical,
                now,
            ),
            AlertRule::new(
                "SSL certificate expiring within 14 days",
                AlertCondition::SslExpiringWithin { days: 14 },
                Status::Warning,
                now,
            ),
            AlertRule::new(
                "Docker container stopped",
                AlertCondition::ContainerNotRunning { name: None },
                Status::Warning,
                now,
            ),
        ]
    }
}

/// Why an alert rule was rejected.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AlertRuleValidationError {
    #[error("rule name must not be empty")]
    EmptyName,
    #[error("severity {0} does not represent a problem; use warning, critical or offline")]
    NonProblemSeverity(Status),
    #[error("{0} is not a percentage between 0 and 100")]
    InvalidPercentage(f64),
    #[error("day count must be positive, got {0}")]
    InvalidDayCount(i64),
    #[error("this condition cannot apply to the subject the rule is scoped to")]
    ScopeConditionMismatch,
}

/// Whether a rule is currently satisfied for one subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    /// The condition does not hold.
    Clear,
    /// The condition holds but has not held long enough to open an incident.
    Pending,
    /// The condition has held for the required duration; an incident is open.
    Firing,
}

/// The alert engine's per-(rule, subject) memory.
///
/// Persisted so that a restart does not silently reset a five-minute timer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertRuleState {
    pub rule_id: AlertRuleId,
    pub subject: AlertSubject,
    pub state: AlertState,
    /// When the condition most recently started holding.
    pub since: Option<DateTime<Utc>>,
    /// The incident opened by this rule, while one is open.
    pub incident_id: Option<IncidentId>,
    pub last_notified_at: Option<DateTime<Utc>>,
}

impl AlertRuleState {
    pub fn clear(rule_id: AlertRuleId, subject: AlertSubject) -> Self {
        Self {
            rule_id,
            subject,
            state: AlertState::Clear,
            since: None,
            incident_id: None,
            last_notified_at: None,
        }
    }
}

/// A period during which a rule was firing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Incident {
    pub id: IncidentId,
    pub rule_id: AlertRuleId,
    pub subject: AlertSubject,
    pub severity: Status,
    pub summary: String,
    pub opened_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// Whether a human has acknowledged it; acknowledged incidents stop re-notifying.
    pub acknowledged: bool,
}

impl Incident {
    pub fn open(
        rule: &AlertRule,
        subject: AlertSubject,
        summary: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: IncidentId::new(),
            rule_id: rule.id,
            subject,
            severity: rule.severity,
            summary: summary.into(),
            opened_at: at,
            resolved_at: None,
            acknowledged: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.resolved_at.is_none()
    }

    pub fn duration(&self, now: DateTime<Utc>) -> Duration {
        self.resolved_at.unwrap_or(now) - self.opened_at
    }
}

/// A notification queued for delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub incident_id: IncidentId,
    pub severity: Status,
    pub title: String,
    pub body: String,
    pub subject: AlertSubject,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ServerId, WebsiteId};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    #[test]
    fn the_default_rule_set_is_valid() {
        for rule in AlertRule::defaults(at(0)) {
            assert_eq!(
                rule.validate(),
                Ok(()),
                "default rule {:?} is invalid",
                rule.name
            );
        }
    }

    #[test]
    fn the_default_rule_set_covers_the_documented_examples() {
        let rules = AlertRule::defaults(at(0));
        let descriptions: Vec<String> = rules.iter().map(|r| r.condition.describe()).collect();
        assert!(descriptions.iter().any(|d| d.starts_with("CPU >")));
        assert!(descriptions.iter().any(|d| d.starts_with("RAM >")));
        assert!(descriptions.iter().any(|d| d.starts_with("Disk >")));
        assert!(descriptions.iter().any(|d| d == "Server offline"));
        assert!(descriptions.iter().any(|d| d == "Website offline"));
        assert!(descriptions.iter().any(|d| d.contains("SSL expires")));
        assert!(
            descriptions
                .iter()
                .any(|d| d.contains("container not running"))
        );
    }

    #[test]
    fn the_cpu_rule_carries_the_five_minute_hold() {
        let rules = AlertRule::defaults(at(0));
        let cpu = rules
            .iter()
            .find(|r| {
                matches!(
                    r.condition,
                    AlertCondition::MetricThreshold {
                        metric: MetricKind::CpuUsage,
                        ..
                    }
                )
            })
            .expect("cpu rule present");
        assert_eq!(cpu.for_duration(), Duration::minutes(5));
    }

    #[test]
    fn a_healthy_severity_is_not_a_valid_alert() {
        let rule = AlertRule::new(
            "nope",
            AlertCondition::ServerOffline,
            Status::Healthy,
            at(0),
        );
        assert_eq!(
            rule.validate(),
            Err(AlertRuleValidationError::NonProblemSeverity(
                Status::Healthy
            ))
        );
    }

    #[test]
    fn a_rule_scoped_to_the_wrong_subject_kind_is_rejected() {
        // "Website offline" scoped to a server can never fire.
        let mut rule = AlertRule::new(
            "mismatched",
            AlertCondition::WebsiteOffline,
            Status::Critical,
            at(0),
        );
        rule.scope = AlertScope::Subject(AlertSubject::Server(ServerId::new()));
        assert_eq!(
            rule.validate(),
            Err(AlertRuleValidationError::ScopeConditionMismatch)
        );
    }

    #[test]
    fn a_correctly_scoped_rule_is_accepted() {
        let mut rule = AlertRule::new(
            "scoped",
            AlertCondition::WebsiteOffline,
            Status::Critical,
            at(0),
        );
        rule.scope = AlertScope::Subject(AlertSubject::Website(WebsiteId::new()));
        assert_eq!(rule.validate(), Ok(()));
    }

    #[test]
    fn traffic_drop_percentages_must_be_percentages() {
        let rule = AlertRule::new(
            "bad",
            AlertCondition::TrafficDrop {
                metric: AnalyticsMetric::Visitors,
                percent: 150.0,
            },
            Status::Warning,
            at(0),
        );
        assert_eq!(
            rule.validate(),
            Err(AlertRuleValidationError::InvalidPercentage(150.0))
        );
    }

    #[test]
    fn ssl_rules_need_a_positive_horizon() {
        let rule = AlertRule::new(
            "bad",
            AlertCondition::SslExpiringWithin { days: 0 },
            Status::Warning,
            at(0),
        );
        assert_eq!(
            rule.validate(),
            Err(AlertRuleValidationError::InvalidDayCount(0))
        );
    }

    #[test]
    fn scope_matching_honours_tags() {
        let subject = AlertSubject::Server(ServerId::new());
        let tags = vec!["production".to_owned()];

        assert!(AlertScope::All.matches(subject, &tags));
        assert!(AlertScope::Tag("production".into()).matches(subject, &tags));
        assert!(!AlertScope::Tag("staging".into()).matches(subject, &tags));
        assert!(AlertScope::Subject(subject).matches(subject, &tags));
        assert!(
            !AlertScope::Subject(AlertSubject::Server(ServerId::new())).matches(subject, &tags)
        );
    }

    #[test]
    fn conditions_are_typed_to_the_right_subject() {
        assert!(AlertCondition::ServerOffline.applies_to_servers());
        assert!(!AlertCondition::ServerOffline.applies_to_websites());
        assert!(AlertCondition::WebsiteOffline.applies_to_websites());
        assert!(!AlertCondition::WebsiteOffline.applies_to_servers());
    }

    #[test]
    fn incident_duration_freezes_at_resolution() {
        let rule = AlertRule::new("r", AlertCondition::ServerOffline, Status::Critical, at(0));
        let mut incident = Incident::open(
            &rule,
            AlertSubject::Server(ServerId::new()),
            "down",
            at(100),
        );
        assert!(incident.is_open());
        assert_eq!(incident.duration(at(160)), Duration::seconds(60));

        incident.resolved_at = Some(at(130));
        assert!(!incident.is_open());
        assert_eq!(incident.duration(at(999)), Duration::seconds(30));
    }
}
