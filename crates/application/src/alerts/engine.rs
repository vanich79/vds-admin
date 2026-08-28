//! The alert engine.
//!
//! The firing decision is a pure function of `(rule, observation, previous state, now)`.
//! That is what makes "CPU above 90% for five minutes" testable in microseconds rather
//! than by waiting five minutes, and it is why the hold timer survives a restart: the
//! state it depends on is data, and data is persisted.

use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use vds_domain::Status;
use vds_domain::alerts::{AlertCondition, AlertRule, AlertRuleState, AlertState};
use vds_domain::analytics::AnalyticsMetric;
use vds_domain::events::AlertSubject;
use vds_domain::metrics::MetricKind;
use vds_domain::status::ThresholdDirection;

/// Everything the engine needs to know about one subject at one moment.
///
/// Assembled by the caller from the repositories; the engine itself performs no I/O.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertObservation {
    pub subject: AlertSubject,
    pub name: String,
    pub tags: Vec<String>,
    /// The subject's current overall status.
    pub status: Status,
    /// Latest value per metric.
    pub metrics: HashMap<MetricKind, f64>,
    /// Days until the certificate expires, for websites.
    pub ssl_days_remaining: Option<i64>,
    /// Containers that are not running, by name.
    pub stopped_containers: Vec<String>,
    /// systemd units in the failed state.
    pub failed_services: Vec<String>,
    /// True when the last check got a response that did not match expectations.
    pub unexpected_response: bool,
    /// Traffic change against the previous period, as a percentage.
    pub traffic_change: HashMap<AnalyticsMetric, f64>,
}

impl AlertObservation {
    pub fn new(subject: AlertSubject, name: impl Into<String>, status: Status) -> Self {
        Self {
            subject,
            name: name.into(),
            tags: Vec::new(),
            status,
            metrics: HashMap::new(),
            ssl_days_remaining: None,
            stopped_containers: Vec::new(),
            failed_services: Vec::new(),
            unexpected_response: false,
            traffic_change: HashMap::new(),
        }
    }

    pub fn with_metric(mut self, kind: MetricKind, value: f64) -> Self {
        self.metrics.insert(kind, value);
        self
    }

    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }
}

/// What the engine wants done as a result of an evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertAction {
    /// Nothing changed.
    None,
    /// The condition just started holding; the hold timer is running.
    StartedPending,
    /// Open an incident and notify.
    Open { summary: String },
    /// The incident is still open and it is time to notify again.
    Renotify { summary: String },
    /// The condition cleared; close the incident.
    Resolve,
}

/// The outcome of evaluating one rule against one subject.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    pub state: AlertRuleState,
    pub action: AlertAction,
}

/// Evaluates alert rules. Stateless: all state is passed in and returned.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlertEngine;

impl AlertEngine {
    pub fn new() -> Self {
        Self
    }

    /// Evaluates one rule against one subject.
    ///
    /// `previous` is the persisted state for this (rule, subject) pair, or `None` the
    /// first time they meet.
    pub fn evaluate(
        &self,
        rule: &AlertRule,
        observation: &AlertObservation,
        previous: Option<AlertRuleState>,
        now: DateTime<Utc>,
    ) -> Evaluation {
        let mut state =
            previous.unwrap_or_else(|| AlertRuleState::clear(rule.id, observation.subject));

        if !rule.enabled || !rule.scope.matches(observation.subject, &observation.tags) {
            // A disabled rule must resolve anything it had open, rather than leaving an
            // incident stuck open forever.
            return if state.state == AlertState::Firing {
                state.state = AlertState::Clear;
                state.since = None;
                Evaluation {
                    state,
                    action: AlertAction::Resolve,
                }
            } else {
                state.state = AlertState::Clear;
                state.since = None;
                Evaluation {
                    state,
                    action: AlertAction::None,
                }
            };
        }

        let holds = condition_holds(&rule.condition, observation);

        if !holds {
            let was_firing = state.state == AlertState::Firing;
            state.state = AlertState::Clear;
            state.since = None;
            let action = if was_firing {
                AlertAction::Resolve
            } else {
                AlertAction::None
            };
            if was_firing {
                // `incident_id` is deliberately left in place: the caller needs it to
                // close the incident it refers to. Clearing it here would strand the
                // incident open forever.
                state.last_notified_at = None;
            }
            return Evaluation { state, action };
        }

        // The condition holds. How long has it held?
        let since = state.since.unwrap_or(now);
        state.since = Some(since);
        let held_for = now - since;

        match state.state {
            AlertState::Firing => {
                let due = state
                    .last_notified_at
                    .is_none_or(|last| now - last >= rule.renotify_after());
                if due {
                    state.last_notified_at = Some(now);
                    Evaluation {
                        state,
                        action: AlertAction::Renotify {
                            summary: summarise(rule, observation),
                        },
                    }
                } else {
                    Evaluation {
                        state,
                        action: AlertAction::None,
                    }
                }
            }
            AlertState::Clear | AlertState::Pending => {
                if held_for >= rule.for_duration() {
                    state.state = AlertState::Firing;
                    state.last_notified_at = Some(now);
                    Evaluation {
                        state,
                        action: AlertAction::Open {
                            summary: summarise(rule, observation),
                        },
                    }
                } else {
                    let was_pending = state.state == AlertState::Pending;
                    state.state = AlertState::Pending;
                    Evaluation {
                        state,
                        action: if was_pending {
                            AlertAction::None
                        } else {
                            AlertAction::StartedPending
                        },
                    }
                }
            }
        }
    }

    /// How much longer a pending rule must hold before it fires.
    pub fn time_until_firing(
        &self,
        rule: &AlertRule,
        state: &AlertRuleState,
        now: DateTime<Utc>,
    ) -> Option<Duration> {
        if state.state != AlertState::Pending {
            return None;
        }
        let since = state.since?;
        Some((rule.for_duration() - (now - since)).max(Duration::zero()))
    }
}

/// Whether a condition is currently true for a subject.
fn condition_holds(condition: &AlertCondition, observation: &AlertObservation) -> bool {
    match condition {
        AlertCondition::MetricThreshold {
            metric,
            direction,
            value,
        } => {
            match observation.metrics.get(metric) {
                // A metric we could not measure is not a breach. Treating "unknown" as
                // "over the limit" would page the user every time SSH hiccuped.
                None => false,
                Some(current) => match direction {
                    ThresholdDirection::Above => *current > *value,
                    ThresholdDirection::Below => *current < *value,
                },
            }
        }
        AlertCondition::ServerOffline | AlertCondition::WebsiteOffline => {
            observation.status == Status::Offline
        }
        AlertCondition::WebsiteUnexpectedResponse => observation.unexpected_response,
        AlertCondition::SslExpiringWithin { days } => observation
            .ssl_days_remaining
            .is_some_and(|remaining| remaining < *days),
        AlertCondition::ContainerNotRunning { name } => match name {
            Some(wanted) => observation.stopped_containers.iter().any(|c| c == wanted),
            None => !observation.stopped_containers.is_empty(),
        },
        AlertCondition::ServiceFailed { name } => match name {
            Some(wanted) => observation.failed_services.iter().any(|s| s == wanted),
            None => !observation.failed_services.is_empty(),
        },
        AlertCondition::TrafficDrop { metric, percent } => observation
            .traffic_change
            .get(metric)
            // A drop is negative; compare magnitudes.
            .is_some_and(|change| *change <= -*percent),
    }
}

/// Human-readable description of why a rule fired.
fn summarise(rule: &AlertRule, observation: &AlertObservation) -> String {
    let detail = match &rule.condition {
        AlertCondition::MetricThreshold { metric, .. } => observation
            .metrics
            .get(metric)
            .map(|value| format!("{} is {:.1}", metric.label(), value)),
        AlertCondition::SslExpiringWithin { .. } => observation
            .ssl_days_remaining
            .map(|days| format!("certificate expires in {days} days")),
        AlertCondition::ContainerNotRunning { .. } => {
            if observation.stopped_containers.is_empty() {
                None
            } else {
                Some(format!(
                    "containers stopped: {}",
                    observation.stopped_containers.join(", ")
                ))
            }
        }
        AlertCondition::ServiceFailed { .. } => {
            if observation.failed_services.is_empty() {
                None
            } else {
                Some(format!(
                    "services failed: {}",
                    observation.failed_services.join(", ")
                ))
            }
        }
        AlertCondition::TrafficDrop { metric, .. } => observation
            .traffic_change
            .get(metric)
            .map(|change| format!("{} changed by {:.1}%", metric.label(), change)),
        AlertCondition::ServerOffline
        | AlertCondition::WebsiteOffline
        | AlertCondition::WebsiteUnexpectedResponse => None,
    };

    match detail {
        Some(detail) => format!(
            "{}: {} ({})",
            observation.name,
            rule.condition.describe(),
            detail
        ),
        None => format!("{}: {}", observation.name, rule.condition.describe()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::alerts::AlertScope;
    use vds_domain::ids::{ServerId, WebsiteId};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn cpu_rule(hold_secs: u32) -> AlertRule {
        let mut rule = AlertRule::new(
            "CPU high",
            AlertCondition::MetricThreshold {
                metric: MetricKind::CpuUsage,
                direction: ThresholdDirection::Above,
                value: 90.0,
            },
            Status::Warning,
            at(0),
        );
        rule.for_duration_secs = hold_secs;
        rule.renotify_after_secs = 3_600;
        rule
    }

    fn server_observation(cpu: f64) -> AlertObservation {
        AlertObservation::new(
            AlertSubject::Server(ServerId::new()),
            "prod-01",
            Status::Healthy,
        )
        .with_metric(MetricKind::CpuUsage, cpu)
    }

    #[test]
    fn a_rule_with_no_hold_fires_immediately() {
        let engine = AlertEngine::new();
        let rule = cpu_rule(0);
        let evaluation = engine.evaluate(&rule, &server_observation(95.0), None, at(0));

        assert!(matches!(evaluation.action, AlertAction::Open { .. }));
        assert_eq!(evaluation.state.state, AlertState::Firing);
    }

    #[test]
    fn a_five_minute_hold_is_honoured_exactly() {
        // The headline requirement: "CPU > 90% for 5 minutes".
        let engine = AlertEngine::new();
        let rule = cpu_rule(300);
        let observation = server_observation(95.0);

        let first = engine.evaluate(&rule, &observation, None, at(0));
        assert_eq!(first.action, AlertAction::StartedPending);
        assert_eq!(first.state.state, AlertState::Pending);

        let midway = engine.evaluate(&rule, &observation, Some(first.state.clone()), at(299));
        assert_eq!(midway.action, AlertAction::None);
        assert_eq!(midway.state.state, AlertState::Pending);

        let fired = engine.evaluate(&rule, &observation, Some(midway.state), at(300));
        assert!(matches!(fired.action, AlertAction::Open { .. }));
        assert_eq!(fired.state.state, AlertState::Firing);
    }

    #[test]
    fn a_condition_that_clears_before_the_hold_expires_never_fires() {
        // Exactly the false positive the hold exists to suppress: a 30-second CPU spike.
        let engine = AlertEngine::new();
        let rule = cpu_rule(300);

        let pending = engine.evaluate(&rule, &server_observation(95.0), None, at(0));
        let cleared = engine.evaluate(
            &rule,
            &server_observation(20.0),
            Some(pending.state),
            at(30),
        );

        assert_eq!(cleared.action, AlertAction::None);
        assert_eq!(cleared.state.state, AlertState::Clear);
        assert_eq!(cleared.state.since, None);
    }

    #[test]
    fn the_hold_timer_restarts_after_the_condition_clears() {
        let engine = AlertEngine::new();
        let rule = cpu_rule(300);

        let pending = engine.evaluate(&rule, &server_observation(95.0), None, at(0));
        let cleared = engine.evaluate(
            &rule,
            &server_observation(20.0),
            Some(pending.state),
            at(200),
        );
        // Breaching again at t=250 must not fire at t=300 by reusing the old timer.
        let again = engine.evaluate(
            &rule,
            &server_observation(95.0),
            Some(cleared.state),
            at(250),
        );
        assert_eq!(again.action, AlertAction::StartedPending);

        let not_yet = engine.evaluate(
            &rule,
            &server_observation(95.0),
            Some(again.state.clone()),
            at(400),
        );
        assert_eq!(not_yet.action, AlertAction::None);

        let fired = engine.evaluate(
            &rule,
            &server_observation(95.0),
            Some(not_yet.state),
            at(550),
        );
        assert!(matches!(fired.action, AlertAction::Open { .. }));
    }

    #[test]
    fn a_firing_rule_does_not_reopen_on_every_evaluation() {
        let engine = AlertEngine::new();
        let rule = cpu_rule(0);

        let opened = engine.evaluate(&rule, &server_observation(95.0), None, at(0));
        let still = engine.evaluate(&rule, &server_observation(95.0), Some(opened.state), at(60));
        assert_eq!(still.action, AlertAction::None);
        assert_eq!(still.state.state, AlertState::Firing);
    }

    #[test]
    fn a_long_running_incident_renotifies_on_schedule() {
        let engine = AlertEngine::new();
        let rule = cpu_rule(0);

        let opened = engine.evaluate(&rule, &server_observation(95.0), None, at(0));
        let quiet = engine.evaluate(
            &rule,
            &server_observation(95.0),
            Some(opened.state),
            at(3_599),
        );
        assert_eq!(quiet.action, AlertAction::None);

        let renotified = engine.evaluate(
            &rule,
            &server_observation(95.0),
            Some(quiet.state),
            at(3_600),
        );
        assert!(matches!(renotified.action, AlertAction::Renotify { .. }));
    }

    #[test]
    fn recovery_resolves_the_incident() {
        let engine = AlertEngine::new();
        let rule = cpu_rule(0);

        let mut opened = engine.evaluate(&rule, &server_observation(95.0), None, at(0));
        // The service assigns the incident id after the engine opens the alert.
        let opened_incident = Some(vds_domain::ids::IncidentId::new());
        opened.state.incident_id = opened_incident;

        let resolved =
            engine.evaluate(&rule, &server_observation(10.0), Some(opened.state), at(60));

        assert_eq!(resolved.action, AlertAction::Resolve);
        assert_eq!(resolved.state.state, AlertState::Clear);
        // The incident reference survives the transition so the caller can close it.
        assert_eq!(resolved.state.incident_id, opened_incident);
    }

    #[test]
    fn a_metric_we_could_not_measure_is_not_a_breach() {
        // An SSH timeout leaves the CPU value missing. Treating that as "over 90%" would
        // page the user every time the network hiccuped.
        let engine = AlertEngine::new();
        let rule = cpu_rule(0);
        let blind = AlertObservation::new(
            AlertSubject::Server(ServerId::new()),
            "prod-01",
            Status::Unknown,
        );

        let evaluation = engine.evaluate(&rule, &blind, None, at(0));
        assert_eq!(evaluation.action, AlertAction::None);
        assert_eq!(evaluation.state.state, AlertState::Clear);
    }

    #[test]
    fn a_below_threshold_rule_fires_in_the_other_direction() {
        let engine = AlertEngine::new();
        let mut rule = cpu_rule(0);
        rule.condition = AlertCondition::MetricThreshold {
            metric: MetricKind::DiskUsage,
            direction: ThresholdDirection::Below,
            value: 5.0,
        };

        let low = AlertObservation::new(
            AlertSubject::Server(ServerId::new()),
            "prod-01",
            Status::Healthy,
        )
        .with_metric(MetricKind::DiskUsage, 2.0);
        assert!(matches!(
            engine.evaluate(&rule, &low, None, at(0)).action,
            AlertAction::Open { .. }
        ));

        let fine = AlertObservation::new(
            AlertSubject::Server(ServerId::new()),
            "prod-01",
            Status::Healthy,
        )
        .with_metric(MetricKind::DiskUsage, 50.0);
        assert_eq!(
            engine.evaluate(&rule, &fine, None, at(0)).action,
            AlertAction::None
        );
    }

    #[test]
    fn a_disabled_rule_resolves_what_it_had_open() {
        // Otherwise disabling a noisy rule would leave its incident open forever.
        let engine = AlertEngine::new();
        let mut rule = cpu_rule(0);
        let opened = engine.evaluate(&rule, &server_observation(95.0), None, at(0));

        rule.enabled = false;
        let disabled =
            engine.evaluate(&rule, &server_observation(95.0), Some(opened.state), at(60));
        assert_eq!(disabled.action, AlertAction::Resolve);
        assert_eq!(disabled.state.state, AlertState::Clear);
    }

    #[test]
    fn a_rule_scoped_to_a_tag_ignores_subjects_without_it() {
        let engine = AlertEngine::new();
        let mut rule = cpu_rule(0);
        rule.scope = AlertScope::Tag("production".into());

        let staging = server_observation(99.0).with_tags(vec!["staging".into()]);
        assert_eq!(
            engine.evaluate(&rule, &staging, None, at(0)).action,
            AlertAction::None
        );

        let production = server_observation(99.0).with_tags(vec!["production".into()]);
        assert!(matches!(
            engine.evaluate(&rule, &production, None, at(0)).action,
            AlertAction::Open { .. }
        ));
    }

    #[test]
    fn an_offline_server_fires_the_offline_rule() {
        let engine = AlertEngine::new();
        let rule = AlertRule::new(
            "down",
            AlertCondition::ServerOffline,
            Status::Critical,
            at(0),
        );

        let up = AlertObservation::new(
            AlertSubject::Server(ServerId::new()),
            "prod-01",
            Status::Healthy,
        );
        assert_eq!(
            engine.evaluate(&rule, &up, None, at(0)).action,
            AlertAction::None
        );

        let down = AlertObservation::new(
            AlertSubject::Server(ServerId::new()),
            "prod-01",
            Status::Offline,
        );
        assert!(matches!(
            engine.evaluate(&rule, &down, None, at(0)).action,
            AlertAction::Open { .. }
        ));
    }

    #[test]
    fn an_unknown_status_is_not_treated_as_offline() {
        // Below the failure threshold a server is Unknown, and the offline rule must
        // wait for the detector rather than firing early.
        let engine = AlertEngine::new();
        let rule = AlertRule::new(
            "down",
            AlertCondition::ServerOffline,
            Status::Critical,
            at(0),
        );
        let unknown = AlertObservation::new(
            AlertSubject::Server(ServerId::new()),
            "prod-01",
            Status::Unknown,
        );
        assert_eq!(
            engine.evaluate(&rule, &unknown, None, at(0)).action,
            AlertAction::None
        );
    }

    #[test]
    fn ssl_rules_fire_strictly_inside_the_horizon() {
        let engine = AlertEngine::new();
        let rule = AlertRule::new(
            "ssl",
            AlertCondition::SslExpiringWithin { days: 14 },
            Status::Warning,
            at(0),
        );

        let mut observation = AlertObservation::new(
            AlertSubject::Website(WebsiteId::new()),
            "example.com",
            Status::Healthy,
        );

        observation.ssl_days_remaining = Some(14);
        assert_eq!(
            engine.evaluate(&rule, &observation, None, at(0)).action,
            AlertAction::None
        );

        observation.ssl_days_remaining = Some(13);
        assert!(matches!(
            engine.evaluate(&rule, &observation, None, at(0)).action,
            AlertAction::Open { .. }
        ));
    }

    #[test]
    fn a_plain_http_site_never_fires_an_ssl_rule() {
        let engine = AlertEngine::new();
        let rule = AlertRule::new(
            "ssl",
            AlertCondition::SslExpiringWithin { days: 14 },
            Status::Warning,
            at(0),
        );
        let observation = AlertObservation::new(
            AlertSubject::Website(WebsiteId::new()),
            "example.com",
            Status::Healthy,
        );
        assert_eq!(
            engine.evaluate(&rule, &observation, None, at(0)).action,
            AlertAction::None
        );
    }

    #[test]
    fn container_rules_can_watch_one_container_or_all_of_them() {
        let engine = AlertEngine::new();
        let mut observation = AlertObservation::new(
            AlertSubject::Server(ServerId::new()),
            "prod-01",
            Status::Healthy,
        );
        observation.stopped_containers = vec!["worker".into()];

        let any = AlertRule::new(
            "any",
            AlertCondition::ContainerNotRunning { name: None },
            Status::Warning,
            at(0),
        );
        assert!(matches!(
            engine.evaluate(&any, &observation, None, at(0)).action,
            AlertAction::Open { .. }
        ));

        let specific = AlertRule::new(
            "web only",
            AlertCondition::ContainerNotRunning {
                name: Some("web".into()),
            },
            Status::Warning,
            at(0),
        );
        assert_eq!(
            engine.evaluate(&specific, &observation, None, at(0)).action,
            AlertAction::None
        );
    }

    #[test]
    fn a_traffic_drop_fires_only_past_the_configured_magnitude() {
        let engine = AlertEngine::new();
        let rule = AlertRule::new(
            "traffic",
            AlertCondition::TrafficDrop {
                metric: AnalyticsMetric::Visitors,
                percent: 30.0,
            },
            Status::Warning,
            at(0),
        );

        let mut observation = AlertObservation::new(
            AlertSubject::Website(WebsiteId::new()),
            "example.com",
            Status::Healthy,
        );

        observation
            .traffic_change
            .insert(AnalyticsMetric::Visitors, -20.0);
        assert_eq!(
            engine.evaluate(&rule, &observation, None, at(0)).action,
            AlertAction::None
        );

        observation
            .traffic_change
            .insert(AnalyticsMetric::Visitors, -35.0);
        assert!(matches!(
            engine.evaluate(&rule, &observation, None, at(0)).action,
            AlertAction::Open { .. }
        ));
    }

    #[test]
    fn a_traffic_increase_never_fires_a_drop_rule() {
        let engine = AlertEngine::new();
        let rule = AlertRule::new(
            "traffic",
            AlertCondition::TrafficDrop {
                metric: AnalyticsMetric::Visitors,
                percent: 30.0,
            },
            Status::Warning,
            at(0),
        );
        let mut observation = AlertObservation::new(
            AlertSubject::Website(WebsiteId::new()),
            "example.com",
            Status::Healthy,
        );
        observation
            .traffic_change
            .insert(AnalyticsMetric::Visitors, 120.0);
        assert_eq!(
            engine.evaluate(&rule, &observation, None, at(0)).action,
            AlertAction::None
        );
    }

    #[test]
    fn the_summary_names_the_subject_and_the_measurement() {
        let engine = AlertEngine::new();
        let rule = cpu_rule(0);
        let evaluation = engine.evaluate(&rule, &server_observation(97.3), None, at(0));

        let AlertAction::Open { summary } = evaluation.action else {
            panic!("expected an open action");
        };
        assert!(summary.contains("prod-01"), "summary was {summary}");
        assert!(summary.contains("97.3"), "summary was {summary}");
        assert!(summary.contains("CPU"), "summary was {summary}");
    }

    #[test]
    fn a_pending_rule_reports_how_long_is_left() {
        let engine = AlertEngine::new();
        let rule = cpu_rule(300);
        let pending = engine.evaluate(&rule, &server_observation(95.0), None, at(0));

        assert_eq!(
            engine.time_until_firing(&rule, &pending.state, at(120)),
            Some(Duration::seconds(180))
        );
        // A firing rule has no countdown.
        let firing = engine.evaluate(
            &rule,
            &server_observation(95.0),
            Some(pending.state),
            at(300),
        );
        assert_eq!(
            engine.time_until_firing(&rule, &firing.state, at(300)),
            None
        );
    }

    #[test]
    fn a_hold_timer_survives_a_restart_because_it_lives_in_persisted_state() {
        // Simulates the process being restarted at t=200: the engine is reconstructed
        // from nothing but the stored state, and the timer must not reset.
        let rule = cpu_rule(300);
        let before_restart =
            AlertEngine::new().evaluate(&rule, &server_observation(95.0), None, at(0));

        let after_restart = AlertEngine::new().evaluate(
            &rule,
            &server_observation(95.0),
            Some(before_restart.state),
            at(300),
        );
        assert!(matches!(after_restart.action, AlertAction::Open { .. }));
    }
}
