//! Alerting: rule evaluation, incident lifecycle and notification fan-out.

mod dispatcher;
mod engine;

pub use dispatcher::{DeliveryReport, NotificationDispatcher, is_worth_retrying, notification_for};
pub use engine::{AlertAction, AlertEngine, AlertObservation, Evaluation};

use crate::scheduler::JobOutcome;
use std::sync::Arc;
use vds_domain::alerts::Incident;
use vds_domain::events::DomainEvent;
use vds_domain::ports::{AlertRepository, Clock, EventPublisher};

/// Drives one alerting pass: evaluate every rule against every in-scope subject, then
/// open, resolve and notify as required.
///
/// The observations are supplied by the caller — assembling them needs the server,
/// website and analytics repositories, and keeping that out of here is what lets the
/// firing logic stay pure.
pub struct AlertService {
    engine: AlertEngine,
    alerts: Arc<dyn AlertRepository>,
    dispatcher: Arc<NotificationDispatcher>,
    events: Arc<dyn EventPublisher>,
    clock: Arc<dyn Clock>,
}

/// What one alerting pass did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AlertPassReport {
    pub opened: usize,
    pub resolved: usize,
    pub renotified: usize,
    pub evaluated: usize,
}

impl AlertService {
    pub fn new(
        alerts: Arc<dyn AlertRepository>,
        dispatcher: Arc<NotificationDispatcher>,
        events: Arc<dyn EventPublisher>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            engine: AlertEngine::new(),
            alerts,
            dispatcher,
            events,
            clock,
        }
    }

    /// Evaluates every rule against every observation.
    pub async fn evaluate_all(&self, observations: &[AlertObservation]) -> AlertPassReport {
        let mut report = AlertPassReport::default();

        let rules = match self.alerts.list_rules().await {
            Ok(rules) => rules,
            Err(err) => {
                tracing::error!(error = %err, "could not load alert rules");
                return report;
            }
        };

        let now = self.clock.now();

        for rule in &rules {
            for observation in observations {
                // A website rule against a server can never fire; skipping early keeps
                // the state table from filling with dead (rule, subject) pairs.
                let applicable = match observation.subject {
                    vds_domain::events::AlertSubject::Server(_) => {
                        rule.condition.applies_to_servers()
                    }
                    vds_domain::events::AlertSubject::Website(_) => {
                        rule.condition.applies_to_websites()
                    }
                };
                if !applicable {
                    continue;
                }

                report.evaluated += 1;
                let previous = self
                    .alerts
                    .load_rule_state(rule.id, observation.subject)
                    .await
                    .ok()
                    .flatten();

                let mut evaluation = self.engine.evaluate(rule, observation, previous, now);

                match &evaluation.action {
                    AlertAction::Open { summary } => {
                        let incident =
                            Incident::open(rule, observation.subject, summary.clone(), now);
                        evaluation.state.incident_id = Some(incident.id);

                        if let Err(err) = self.alerts.save_incident(&incident).await {
                            tracing::error!(error = %err, "could not persist incident");
                        }

                        self.events.publish(DomainEvent::IncidentOpened {
                            incident_id: incident.id,
                            rule_id: rule.id,
                            subject: observation.subject,
                            severity: rule.severity,
                            summary: summary.clone(),
                        });

                        let notification = notification_for(
                            rule,
                            observation.subject,
                            incident.id,
                            summary.clone(),
                            now,
                        );
                        self.dispatcher.dispatch(&notification, rule).await;
                        report.opened += 1;
                    }
                    AlertAction::Renotify { summary } => {
                        // An acknowledged incident stops nagging until it recurs.
                        let acknowledged = match evaluation.state.incident_id {
                            Some(id) => self
                                .alerts
                                .get_incident(id)
                                .await
                                .map(|i| i.acknowledged)
                                .unwrap_or(false),
                            None => false,
                        };

                        if !acknowledged && let Some(incident_id) = evaluation.state.incident_id {
                            let notification = notification_for(
                                rule,
                                observation.subject,
                                incident_id,
                                summary.clone(),
                                now,
                            );
                            self.dispatcher.dispatch(&notification, rule).await;
                            report.renotified += 1;
                        }
                    }
                    AlertAction::Resolve => {
                        if let Some(incident_id) = evaluation.state.incident_id {
                            if let Ok(mut incident) = self.alerts.get_incident(incident_id).await {
                                incident.resolved_at = Some(now);
                                if let Err(err) = self.alerts.save_incident(&incident).await {
                                    tracing::error!(error = %err, "could not resolve incident");
                                }
                            }
                            self.events.publish(DomainEvent::IncidentResolved {
                                incident_id,
                                rule_id: rule.id,
                                subject: observation.subject,
                            });
                        }
                        evaluation.state.incident_id = None;
                        report.resolved += 1;
                    }
                    AlertAction::StartedPending | AlertAction::None => {}
                }

                if let Err(err) = self.alerts.save_rule_state(&evaluation.state).await {
                    tracing::error!(error = %err, "could not persist alert state");
                }
            }
        }

        report
    }

    /// Acknowledges an incident, silencing its repeat notifications.
    pub async fn acknowledge(
        &self,
        incident_id: vds_domain::ids::IncidentId,
    ) -> Result<(), vds_domain::ports::RepositoryError> {
        let mut incident = self.alerts.get_incident(incident_id).await?;
        incident.acknowledged = true;
        self.alerts.save_incident(&incident).await
    }

    /// Runs as a scheduled job.
    pub async fn run_as_job(&self, observations: &[AlertObservation]) -> JobOutcome {
        self.evaluate_all(observations).await;
        JobOutcome::Success
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeAlertRepository, RecordingNotificationProvider};
    use chrono::DateTime;
    use vds_domain::Status;
    use vds_domain::alerts::{AlertCondition, AlertRule};
    use vds_domain::events::AlertSubject;
    use vds_domain::ids::{ServerId, WebsiteId};
    use vds_domain::metrics::MetricKind;
    use vds_domain::ports::{FixedClock, NotificationProvider, RecordingEventPublisher};
    use vds_domain::status::ThresholdDirection;

    fn at(secs: i64) -> DateTime<chrono::Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    struct Harness {
        service: AlertService,
        alerts: Arc<FakeAlertRepository>,
        notifier: Arc<RecordingNotificationProvider>,
        events: Arc<RecordingEventPublisher>,
        clock: FixedClock,
    }

    fn harness() -> Harness {
        let alerts = Arc::new(FakeAlertRepository::new());
        let notifier = Arc::new(RecordingNotificationProvider::new());
        let dispatcher = Arc::new(NotificationDispatcher::new(
            vec![Arc::clone(&notifier) as Arc<dyn NotificationProvider>],
            Status::Warning,
        ));
        let events = Arc::new(RecordingEventPublisher::new());
        let clock = FixedClock::new(at(0));

        let service = AlertService::new(
            Arc::clone(&alerts) as Arc<dyn AlertRepository>,
            dispatcher,
            Arc::clone(&events) as Arc<dyn EventPublisher>,
            Arc::new(clock.clone()),
        );

        Harness {
            service,
            alerts,
            notifier,
            events,
            clock,
        }
    }

    fn cpu_rule() -> AlertRule {
        AlertRule::new(
            "CPU high",
            AlertCondition::MetricThreshold {
                metric: MetricKind::CpuUsage,
                direction: ThresholdDirection::Above,
                value: 90.0,
            },
            Status::Warning,
            at(0),
        )
    }

    fn server_observation(id: ServerId, cpu: f64) -> AlertObservation {
        AlertObservation::new(AlertSubject::Server(id), "prod-01", Status::Healthy)
            .with_metric(MetricKind::CpuUsage, cpu)
    }

    #[tokio::test]
    async fn a_breach_opens_an_incident_and_notifies() {
        let h = harness();
        h.alerts.insert_rule(cpu_rule());
        let server = ServerId::new();

        let report = h
            .service
            .evaluate_all(&[server_observation(server, 95.0)])
            .await;

        assert_eq!(report.opened, 1);
        assert_eq!(h.alerts.incidents().len(), 1);
        assert_eq!(h.notifier.delivered().len(), 1);
        assert!(h.events.contains(|e| e.kind() == "incident_opened"));
    }

    #[tokio::test]
    async fn a_continuing_breach_does_not_open_a_second_incident() {
        let h = harness();
        h.alerts.insert_rule(cpu_rule());
        let server = ServerId::new();

        h.service
            .evaluate_all(&[server_observation(server, 95.0)])
            .await;
        h.clock.set(at(60));
        let report = h
            .service
            .evaluate_all(&[server_observation(server, 96.0)])
            .await;

        assert_eq!(report.opened, 0);
        assert_eq!(h.alerts.incidents().len(), 1);
        assert_eq!(
            h.notifier.delivered().len(),
            1,
            "must not re-notify before the interval"
        );
    }

    #[tokio::test]
    async fn recovery_resolves_the_incident() {
        let h = harness();
        h.alerts.insert_rule(cpu_rule());
        let server = ServerId::new();

        h.service
            .evaluate_all(&[server_observation(server, 95.0)])
            .await;
        h.clock.set(at(60));
        let report = h
            .service
            .evaluate_all(&[server_observation(server, 10.0)])
            .await;

        assert_eq!(report.resolved, 1);
        let incidents = h.alerts.incidents();
        assert_eq!(incidents.len(), 1);
        assert!(!incidents[0].is_open());
        assert_eq!(incidents[0].resolved_at, Some(at(60)));
        assert!(h.events.contains(|e| e.kind() == "incident_resolved"));
    }

    #[tokio::test]
    async fn an_acknowledged_incident_stops_nagging() {
        let h = harness();
        let mut rule = cpu_rule();
        rule.renotify_after_secs = 60;
        h.alerts.insert_rule(rule);
        let server = ServerId::new();

        h.service
            .evaluate_all(&[server_observation(server, 95.0)])
            .await;
        let incident_id = h.alerts.incidents()[0].id;
        h.service
            .acknowledge(incident_id)
            .await
            .expect("acknowledged");

        h.clock.set(at(600));
        let report = h
            .service
            .evaluate_all(&[server_observation(server, 95.0)])
            .await;

        assert_eq!(report.renotified, 0);
        assert_eq!(
            h.notifier.delivered().len(),
            1,
            "only the original notification"
        );
    }

    #[tokio::test]
    async fn an_unacknowledged_incident_renotifies_after_the_interval() {
        let h = harness();
        let mut rule = cpu_rule();
        rule.renotify_after_secs = 60;
        h.alerts.insert_rule(rule);
        let server = ServerId::new();

        h.service
            .evaluate_all(&[server_observation(server, 95.0)])
            .await;
        h.clock.set(at(600));
        let report = h
            .service
            .evaluate_all(&[server_observation(server, 95.0)])
            .await;

        assert_eq!(report.renotified, 1);
        assert_eq!(h.notifier.delivered().len(), 2);
    }

    #[tokio::test]
    async fn a_website_rule_is_never_evaluated_against_a_server() {
        // Otherwise the state table fills with (rule, subject) pairs that can never fire.
        let h = harness();
        h.alerts.insert_rule(AlertRule::new(
            "site down",
            AlertCondition::WebsiteOffline,
            Status::Critical,
            at(0),
        ));

        let report = h
            .service
            .evaluate_all(&[server_observation(ServerId::new(), 95.0)])
            .await;
        assert_eq!(report.evaluated, 0);
        assert_eq!(report.opened, 0);
    }

    #[tokio::test]
    async fn each_subject_gets_its_own_incident() {
        let h = harness();
        h.alerts.insert_rule(cpu_rule());

        let report = h
            .service
            .evaluate_all(&[
                server_observation(ServerId::new(), 95.0),
                server_observation(ServerId::new(), 97.0),
            ])
            .await;

        assert_eq!(report.opened, 2);
        assert_eq!(h.alerts.incidents().len(), 2);
    }

    #[tokio::test]
    async fn one_subject_recovering_does_not_resolve_anothers_incident() {
        let h = harness();
        h.alerts.insert_rule(cpu_rule());
        let a = ServerId::new();
        let b = ServerId::new();

        h.service
            .evaluate_all(&[server_observation(a, 95.0), server_observation(b, 97.0)])
            .await;
        h.clock.set(at(60));
        h.service
            .evaluate_all(&[server_observation(a, 10.0), server_observation(b, 97.0)])
            .await;

        let incidents = h.alerts.incidents();
        assert_eq!(incidents.iter().filter(|i| i.is_open()).count(), 1);
    }

    #[tokio::test]
    async fn a_website_offline_rule_fires_for_a_website() {
        let h = harness();
        h.alerts.insert_rule(AlertRule::new(
            "site down",
            AlertCondition::WebsiteOffline,
            Status::Critical,
            at(0),
        ));

        let observation = AlertObservation::new(
            AlertSubject::Website(WebsiteId::new()),
            "example.com",
            Status::Offline,
        );
        let report = h.service.evaluate_all(&[observation]).await;

        assert_eq!(report.opened, 1);
        assert_eq!(h.notifier.delivered()[0].severity, Status::Critical);
    }

    #[tokio::test]
    async fn no_rules_means_no_work_and_no_failure() {
        let h = harness();
        let report = h
            .service
            .evaluate_all(&[server_observation(ServerId::new(), 99.0)])
            .await;
        assert_eq!(report, AlertPassReport::default());
    }

    #[tokio::test]
    async fn the_default_rule_set_fires_on_the_documented_examples() {
        // End-to-end over the rules a fresh install ships with.
        let h = harness();
        for rule in AlertRule::defaults(at(0)) {
            h.alerts.insert_rule(rule);
        }

        let mut observation = AlertObservation::new(
            AlertSubject::Server(ServerId::new()),
            "prod-01",
            Status::Offline,
        )
        .with_metric(MetricKind::MemoryUsage, 97.0)
        .with_metric(MetricKind::DiskUsage, 95.0);
        observation.stopped_containers = vec!["worker".into()];

        let report = h.service.evaluate_all(&[observation]).await;

        // RAM > 95%, Disk > 90%, server offline, container stopped. The CPU rule has a
        // five-minute hold, so it is pending rather than firing.
        assert_eq!(report.opened, 4, "report was {report:?}");
    }

    #[tokio::test]
    async fn the_job_wrapper_succeeds() {
        let h = harness();
        assert_eq!(h.service.run_as_job(&[]).await, JobOutcome::Success);
    }
}
