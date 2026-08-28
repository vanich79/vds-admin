//! Temporal correlation between events.
//!
//! This module is careful about one thing above all: it never claims causation. It
//! reports that a traffic anomaly and a CPU spike happened close together on related
//! subjects, and it labels that a *possible* correlation. Deciding whether one caused
//! the other is a human's job — or, later, a much more sophisticated engine's.
//!
//! The MVP is deliberately just a time-window join. `docs/adr/` records why: an
//! explainable heuristic that a user can check beats a confident-sounding model that
//! nobody can audit.

use chrono::Duration;
use std::collections::HashMap;
use vds_domain::events::{AlertSubject, DomainEvent, EventEnvelope};
use vds_domain::ids::{ServerId, WebsiteId};
use vds_domain::metrics::TimeWindow;

/// How close in time two events must be to be considered together.
pub const DEFAULT_CORRELATION_WINDOW: Duration = Duration::minutes(30);

/// How confident the engine is that two events are related.
///
/// Deliberately coarse. There is no statistical basis for finer gradations here, and
/// pretending otherwise would be dishonest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CorrelationStrength {
    /// The subjects are directly related — the website is hosted on that server.
    Direct,
    /// The subjects are unrelated but the timing lines up.
    Circumstantial,
}

/// A possible relationship between a symptom and an earlier event.
///
/// The wording of [`Correlation::describe`] is part of the contract: "possible", never
/// "caused by".
#[derive(Debug, Clone, PartialEq)]
pub struct Correlation {
    /// The event the user is looking at.
    pub subject_event: EventEnvelope,
    /// An event shortly before it that may be related.
    pub related_event: EventEnvelope,
    pub strength: CorrelationStrength,
    /// How long before the subject event the related one occurred.
    pub lead_time: Duration,
}

impl Correlation {
    /// A phrasing safe to show a user.
    pub fn describe(&self) -> String {
        let minutes = self.lead_time.num_minutes();
        let timing = if minutes <= 0 {
            "at the same time".to_owned()
        } else {
            format!("{minutes} minutes earlier")
        };
        format!(
            "Possible infrastructure event {timing}: {}",
            summarise_event(&self.related_event.event)
        )
    }
}

/// Which subjects are related to which, so the engine can tell a direct relationship
/// from a coincidence.
#[derive(Debug, Clone, Default)]
pub struct SubjectGraph {
    website_to_server: HashMap<WebsiteId, ServerId>,
}

impl SubjectGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn link(&mut self, website: WebsiteId, server: ServerId) {
        self.website_to_server.insert(website, server);
    }

    /// Whether two subjects are directly related.
    pub fn are_related(&self, a: AlertSubject, b: AlertSubject) -> bool {
        match (a, b) {
            (AlertSubject::Website(website), AlertSubject::Server(server))
            | (AlertSubject::Server(server), AlertSubject::Website(website)) => {
                self.website_to_server.get(&website) == Some(&server)
            }
            (AlertSubject::Server(a), AlertSubject::Server(b)) => a == b,
            (AlertSubject::Website(a), AlertSubject::Website(b)) => a == b,
        }
    }
}

/// Finds events that plausibly relate to a given one.
#[derive(Debug, Clone)]
pub struct CorrelationEngine {
    window: Duration,
    graph: SubjectGraph,
}

impl CorrelationEngine {
    pub fn new(graph: SubjectGraph) -> Self {
        Self {
            window: DEFAULT_CORRELATION_WINDOW,
            graph,
        }
    }

    pub fn with_window(mut self, window: Duration) -> Self {
        self.window = window;
        self
    }

    /// Events in `history` that occurred shortly before `subject_event` and could relate
    /// to it, strongest first.
    pub fn correlate(
        &self,
        subject_event: &EventEnvelope,
        history: &[EventEnvelope],
    ) -> Vec<Correlation> {
        let Some(subject) = subject_event.event.subject() else {
            return Vec::new();
        };
        let window_start = subject_event.occurred_at - self.window;

        let mut correlations: Vec<Correlation> = history
            .iter()
            .filter(|candidate| candidate.id != subject_event.id)
            // Only *earlier* events can explain a later one. Including later events
            // would produce backwards narratives.
            .filter(|candidate| {
                candidate.occurred_at <= subject_event.occurred_at
                    && candidate.occurred_at >= window_start
            })
            // Routine successes are not explanations.
            .filter(|candidate| is_explanatory(&candidate.event))
            .filter_map(|candidate| {
                let candidate_subject = candidate.event.subject()?;
                let strength = if self.graph.are_related(subject, candidate_subject) {
                    CorrelationStrength::Direct
                } else {
                    CorrelationStrength::Circumstantial
                };
                Some(Correlation {
                    subject_event: subject_event.clone(),
                    related_event: candidate.clone(),
                    strength,
                    lead_time: subject_event.occurred_at - candidate.occurred_at,
                })
            })
            .collect();

        // Direct relationships first; within a strength, the closest in time first.
        correlations.sort_by(|a, b| {
            a.strength
                .cmp(&b.strength)
                .then_with(|| a.lead_time.cmp(&b.lead_time))
        });
        correlations
    }

    /// The window this engine considers.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// A time window centred on an event, for fetching candidate history.
    pub fn history_window(&self, event: &EventEnvelope) -> TimeWindow {
        TimeWindow::new(
            event.occurred_at - self.window,
            event.occurred_at + Duration::seconds(1),
        )
    }
}

/// Whether an event could plausibly explain a later problem.
fn is_explanatory(event: &DomainEvent) -> bool {
    matches!(
        event,
        DomainEvent::ServerStatusChanged { .. }
            | DomainEvent::MetricThresholdExceeded { .. }
            | DomainEvent::ServerCollectionFailed { .. }
            | DomainEvent::WebsiteStatusChanged { .. }
            | DomainEvent::ContainerStateChanged { .. }
            | DomainEvent::ServiceStateChanged { .. }
            | DomainEvent::IncidentOpened { .. }
    )
}

/// A short description of an event, for the correlation panel.
fn summarise_event(event: &DomainEvent) -> String {
    match event {
        DomainEvent::ServerStatusChanged { to, .. } => format!("server became {to}"),
        DomainEvent::MetricThresholdExceeded { metric, value, .. } => {
            format!("{} reached {:.1}", metric.label(), value)
        }
        DomainEvent::ServerCollectionFailed { .. } => "server collection failed".to_owned(),
        DomainEvent::WebsiteStatusChanged { to, .. } => format!("website became {to}"),
        DomainEvent::ContainerStateChanged {
            container, state, ..
        } => {
            format!("container {container} is {state}")
        }
        DomainEvent::ServiceStateChanged { service, state, .. } => {
            format!("service {service} is {state}")
        }
        DomainEvent::IncidentOpened { summary, .. } => summary.clone(),
        other => other.kind().replace('_', " "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use vds_domain::Status;
    use vds_domain::analytics::AnalyticsMetric;
    use vds_domain::metrics::MetricKind;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn envelope(event: DomainEvent, occurred_at: DateTime<Utc>) -> EventEnvelope {
        EventEnvelope::new(event, occurred_at)
    }

    /// The scenario from the brief: CPU spike, then slow responses, then a traffic drop.
    struct Scenario {
        engine: CorrelationEngine,
        traffic_anomaly: EventEnvelope,
        cpu_spike: EventEnvelope,
        history: Vec<EventEnvelope>,
    }

    fn scenario() -> Scenario {
        let server = ServerId::new();
        let website = WebsiteId::new();
        let mut graph = SubjectGraph::new();
        graph.link(website, server);

        // 14:20 CPU spike, 14:22 website slower, 14:25 traffic anomaly.
        let cpu_spike = envelope(
            DomainEvent::MetricThresholdExceeded {
                server_id: server,
                metric: MetricKind::CpuUsage,
                value: 97.0,
                threshold: 90.0,
                status: Status::Critical,
            },
            at(14 * 3_600 + 20 * 60),
        );
        let website_degraded = envelope(
            DomainEvent::WebsiteStatusChanged {
                website_id: website,
                from: Status::Healthy,
                to: Status::Warning,
                reason: None,
            },
            at(14 * 3_600 + 22 * 60),
        );
        let traffic_anomaly = envelope(
            DomainEvent::TrafficAnomalyDetected {
                website_id: website,
                metric: AnalyticsMetric::Visitors,
                current: 6_500.0,
                baseline: 10_000.0,
                change_percent: -35.0,
            },
            at(14 * 3_600 + 25 * 60),
        );

        Scenario {
            engine: CorrelationEngine::new(graph),
            history: vec![cpu_spike.clone(), website_degraded, traffic_anomaly.clone()],
            traffic_anomaly,
            cpu_spike,
        }
    }

    #[test]
    fn a_traffic_anomaly_surfaces_the_earlier_infrastructure_events() {
        let s = scenario();
        let correlations = s.engine.correlate(&s.traffic_anomaly, &s.history);

        assert_eq!(correlations.len(), 2);
        assert!(
            correlations
                .iter()
                .all(|c| c.strength == CorrelationStrength::Direct)
        );
    }

    #[test]
    fn the_closest_related_event_comes_first() {
        let s = scenario();
        let correlations = s.engine.correlate(&s.traffic_anomaly, &s.history);
        assert_eq!(correlations[0].lead_time, Duration::minutes(3));
        assert_eq!(correlations[1].lead_time, Duration::minutes(5));
    }

    #[test]
    fn the_wording_never_claims_causation() {
        // This is a contract, not a style preference: the brief is explicit that the
        // engine must not assert cause.
        let s = scenario();
        let correlations = s.engine.correlate(&s.traffic_anomaly, &s.history);
        for correlation in &correlations {
            let text = correlation.describe();
            assert!(text.starts_with("Possible"), "wording was: {text}");
            assert!(
                !text.to_lowercase().contains("caused"),
                "wording was: {text}"
            );
            assert!(
                !text.to_lowercase().contains("because"),
                "wording was: {text}"
            );
        }
    }

    #[test]
    fn later_events_are_never_offered_as_explanations() {
        // An event after the symptom cannot explain it.
        let s = scenario();
        let correlations = s.engine.correlate(&s.cpu_spike, &s.history);
        assert!(
            correlations
                .iter()
                .all(|c| c.related_event.occurred_at <= s.cpu_spike.occurred_at),
            "a later event was offered as an explanation"
        );
    }

    #[test]
    fn events_outside_the_window_are_ignored() {
        let s = scenario();
        let ancient = envelope(
            DomainEvent::ServerStatusChanged {
                server_id: ServerId::new(),
                from: Status::Healthy,
                to: Status::Offline,
                reason: None,
            },
            at(3_600),
        );
        let mut history = s.history.clone();
        history.push(ancient);

        let correlations = s.engine.correlate(&s.traffic_anomaly, &history);
        assert_eq!(
            correlations.len(),
            2,
            "an event hours earlier must not be correlated"
        );
    }

    #[test]
    fn an_unrelated_server_is_only_circumstantial() {
        let s = scenario();
        let unrelated = envelope(
            DomainEvent::MetricThresholdExceeded {
                server_id: ServerId::new(),
                metric: MetricKind::MemoryUsage,
                value: 99.0,
                threshold: 95.0,
                status: Status::Critical,
            },
            at(14 * 3_600 + 24 * 60),
        );
        let mut history = s.history.clone();
        history.push(unrelated);

        let correlations = s.engine.correlate(&s.traffic_anomaly, &history);
        // Direct relationships must be ranked above coincidences.
        assert_eq!(correlations[0].strength, CorrelationStrength::Direct);
        assert!(
            correlations
                .iter()
                .any(|c| c.strength == CorrelationStrength::Circumstantial)
        );
    }

    #[test]
    fn routine_successes_are_not_offered_as_explanations() {
        let s = scenario();
        let routine = envelope(
            DomainEvent::WebsiteChecked {
                website_id: WebsiteId::new(),
                status: Status::Healthy,
                response_ms: Some(90),
            },
            at(14 * 3_600 + 24 * 60),
        );
        let mut history = s.history.clone();
        history.push(routine);

        let correlations = s.engine.correlate(&s.traffic_anomaly, &history);
        assert!(
            correlations
                .iter()
                .all(|c| c.related_event.event.kind() != "website_checked")
        );
    }

    #[test]
    fn an_event_never_correlates_with_itself() {
        let s = scenario();
        let correlations = s.engine.correlate(&s.traffic_anomaly, &s.history);
        assert!(
            correlations
                .iter()
                .all(|c| c.related_event.id != s.traffic_anomaly.id)
        );
    }

    #[test]
    fn no_history_yields_no_correlations_rather_than_a_guess() {
        let s = scenario();
        assert!(s.engine.correlate(&s.traffic_anomaly, &[]).is_empty());
    }

    #[test]
    fn the_window_is_configurable() {
        let s = scenario();
        let narrow = s.engine.clone().with_window(Duration::minutes(4));
        let correlations = narrow.correlate(&s.traffic_anomaly, &s.history);
        // Only the event three minutes earlier is now in range.
        assert_eq!(correlations.len(), 1);
        assert_eq!(correlations[0].lead_time, Duration::minutes(3));
    }

    #[test]
    fn the_history_window_covers_the_lookback_period() {
        let s = scenario();
        let window = s.engine.history_window(&s.traffic_anomaly);
        assert_eq!(
            window.duration(),
            DEFAULT_CORRELATION_WINDOW + Duration::seconds(1)
        );
        assert!(window.contains(s.cpu_spike.occurred_at));
    }

    #[test]
    fn subject_relationships_are_symmetric() {
        let server = ServerId::new();
        let website = WebsiteId::new();
        let mut graph = SubjectGraph::new();
        graph.link(website, server);

        assert!(graph.are_related(AlertSubject::Website(website), AlertSubject::Server(server)));
        assert!(graph.are_related(AlertSubject::Server(server), AlertSubject::Website(website)));
        assert!(!graph.are_related(
            AlertSubject::Website(WebsiteId::new()),
            AlertSubject::Server(server)
        ));
    }

    #[test]
    fn simultaneous_events_are_described_as_such() {
        let s = scenario();
        let simultaneous = envelope(
            DomainEvent::ServerStatusChanged {
                server_id: ServerId::new(),
                from: Status::Healthy,
                to: Status::Warning,
                reason: None,
            },
            s.traffic_anomaly.occurred_at,
        );
        let correlations = s.engine.correlate(&s.traffic_anomaly, &[simultaneous]);
        assert!(correlations[0].describe().contains("at the same time"));
    }
}
