//! SQLite implementations of [`AlertRepository`] and [`EventRepository`].

use crate::connection::Database;
use crate::convert::*;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::Row;
use vds_domain::Status;
use vds_domain::alerts::{
    AlertCondition, AlertRule, AlertRuleState, AlertScope, AlertState, Incident,
};
use vds_domain::events::{AlertSubject, DomainEvent, EventEnvelope};
use vds_domain::ids::{AlertRuleId, EventId, IncidentId, ProviderId, ServerId, WebsiteId};
use vds_domain::metrics::TimeWindow;
use vds_domain::ports::{AlertRepository, EventRepository, RepositoryError};

/// Stores alert rules, their per-subject state, and incidents.
#[derive(Debug, Clone)]
pub struct SqliteAlertRepository {
    database: Database,
}

impl SqliteAlertRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

/// Stores the event log.
#[derive(Debug, Clone)]
pub struct SqliteEventRepository {
    database: Database,
}

impl SqliteEventRepository {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

/// A subject split into the two columns it is stored as.
///
/// Two columns rather than one JSON blob because subjects are *queried* on — the event
/// log filters by them — and an index on a JSON extract is a poor substitute for one on
/// a real column.
fn split_subject(subject: AlertSubject) -> (&'static str, String) {
    match subject {
        AlertSubject::Server(id) => ("server", id.to_string()),
        AlertSubject::Website(id) => ("website", id.to_string()),
    }
}

fn join_subject(kind: &str, id: &str) -> Result<AlertSubject, RepositoryError> {
    let uuid = parse_uuid("subject_id", id)?;
    match kind {
        "server" => Ok(AlertSubject::Server(ServerId::from_uuid(uuid))),
        "website" => Ok(AlertSubject::Website(WebsiteId::from_uuid(uuid))),
        other => Err(RepositoryError::Corrupt(format!(
            "unknown subject kind {other:?}"
        ))),
    }
}

const RULE_COLUMNS: &str = "id, name, enabled, condition_json, scope_json, for_duration_secs, \
     severity, renotify_after_secs, notify_via_json, created_at";

fn read_rule(row: &Row<'_>) -> Result<AlertRule, rusqlite::Error> {
    let id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let enabled: i64 = row.get(2)?;
    let condition_json: String = row.get(3)?;
    let scope_json: String = row.get(4)?;
    let for_duration_secs: i64 = row.get(5)?;
    let severity: String = row.get(6)?;
    let renotify_after_secs: i64 = row.get(7)?;
    let notify_via_json: String = row.get(8)?;
    let created_at: i64 = row.get(9)?;

    Ok(AlertRule {
        id: AlertRuleId::from_uuid(parse_uuid("alert_rules.id", &id).map_err(corrupt)?),
        name,
        enabled: enabled != 0,
        condition: from_json::<AlertCondition>("alert_rules.condition_json", &condition_json)
            .map_err(corrupt)?,
        scope: from_json::<AlertScope>("alert_rules.scope_json", &scope_json).map_err(corrupt)?,
        for_duration_secs: for_duration_secs.max(0) as u32,
        severity: Status::from_str_lenient(&severity),
        renotify_after_secs: renotify_after_secs.max(0) as u32,
        notify_via: from_json::<Vec<String>>("alert_rules.notify_via_json", &notify_via_json)
            .map_err(corrupt)?
            .into_iter()
            .map(ProviderId::new)
            .collect(),
        created_at: from_millis(created_at).map_err(corrupt)?,
    })
}

fn read_incident(row: &Row<'_>) -> Result<Incident, rusqlite::Error> {
    let id: String = row.get(0)?;
    let rule_id: String = row.get(1)?;
    let subject_kind: String = row.get(2)?;
    let subject_id: String = row.get(3)?;
    let severity: String = row.get(4)?;
    let summary: String = row.get(5)?;
    let opened_at: i64 = row.get(6)?;
    let resolved_at: Option<i64> = row.get(7)?;
    let acknowledged: i64 = row.get(8)?;

    Ok(Incident {
        id: IncidentId::from_uuid(parse_uuid("incidents.id", &id).map_err(corrupt)?),
        rule_id: AlertRuleId::from_uuid(
            parse_uuid("incidents.rule_id", &rule_id).map_err(corrupt)?,
        ),
        subject: join_subject(&subject_kind, &subject_id).map_err(corrupt)?,
        severity: Status::from_str_lenient(&severity),
        summary,
        opened_at: from_millis(opened_at).map_err(corrupt)?,
        resolved_at: optional_millis(resolved_at).map_err(corrupt)?,
        acknowledged: acknowledged != 0,
    })
}

const INCIDENT_COLUMNS: &str = "id, rule_id, subject_kind, subject_id, severity, summary, \
     opened_at, resolved_at, acknowledged";

#[async_trait]
impl AlertRepository for SqliteAlertRepository {
    async fn list_rules(&self) -> Result<Vec<AlertRule>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!("SELECT {RULE_COLUMNS} FROM alert_rules ORDER BY created_at");
                let mut statement = connection.prepare(&sql)?;
                statement.query_map([], read_rule)?.collect()
            })
            .await
    }

    async fn get_rule(&self, id: AlertRuleId) -> Result<AlertRule, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!("SELECT {RULE_COLUMNS} FROM alert_rules WHERE id = ?1");
                connection.query_row(&sql, [Sql(id)], read_rule)
            })
            .await
            .map_err(|err| match err {
                RepositoryError::NotFound { .. } => RepositoryError::not_found("alert rule", id),
                other => other,
            })
    }

    async fn save_rule(&self, rule: &AlertRule) -> Result<(), RepositoryError> {
        let rule = rule.clone();
        self.database
            .call(move |connection| {
                let notify_via: Vec<String> = rule
                    .notify_via
                    .iter()
                    .map(|p| p.as_str().to_owned())
                    .collect();
                connection.execute(
                    "INSERT INTO alert_rules (id, name, enabled, condition_json, scope_json,
                         for_duration_secs, severity, renotify_after_secs, notify_via_json,
                         created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                     ON CONFLICT(id) DO UPDATE SET
                         name = excluded.name,
                         enabled = excluded.enabled,
                         condition_json = excluded.condition_json,
                         scope_json = excluded.scope_json,
                         for_duration_secs = excluded.for_duration_secs,
                         severity = excluded.severity,
                         renotify_after_secs = excluded.renotify_after_secs,
                         notify_via_json = excluded.notify_via_json",
                    rusqlite::params![
                        Sql(rule.id),
                        rule.name,
                        i64::from(rule.enabled),
                        to_json(&rule.condition)?,
                        to_json(&rule.scope)?,
                        i64::from(rule.for_duration_secs),
                        rule.severity.as_str(),
                        i64::from(rule.renotify_after_secs),
                        to_json(&notify_via)?,
                        to_millis(rule.created_at),
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn delete_rule(&self, id: AlertRuleId) -> Result<(), RepositoryError> {
        self.database
            .call(move |connection| {
                connection.execute("DELETE FROM alert_rules WHERE id = ?1", [Sql(id)])?;
                Ok(())
            })
            .await
    }

    async fn load_rule_state(
        &self,
        rule: AlertRuleId,
        subject: AlertSubject,
    ) -> Result<Option<AlertRuleState>, RepositoryError> {
        let (kind, id) = split_subject(subject);
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT state, since, incident_id, last_notified_at FROM alert_rule_state
                     WHERE rule_id = ?1 AND subject_kind = ?2 AND subject_id = ?3",
                )?;
                let mut rows =
                    statement.query_map(rusqlite::params![Sql(rule), kind, id], |row| {
                        let state: String = row.get(0)?;
                        let since: Option<i64> = row.get(1)?;
                        let incident_id: Option<String> = row.get(2)?;
                        let last_notified_at: Option<i64> = row.get(3)?;

                        Ok(AlertRuleState {
                            rule_id: rule,
                            subject,
                            state: parse_alert_state(&state).map_err(corrupt)?,
                            since: optional_millis(since).map_err(corrupt)?,
                            incident_id: incident_id
                                .map(|raw| {
                                    parse_uuid("alert_rule_state.incident_id", &raw)
                                        .map(IncidentId::from_uuid)
                                        .map_err(corrupt)
                                })
                                .transpose()?,
                            last_notified_at: optional_millis(last_notified_at).map_err(corrupt)?,
                        })
                    })?;
                rows.next().transpose()
            })
            .await
    }

    async fn save_rule_state(&self, state: &AlertRuleState) -> Result<(), RepositoryError> {
        let state = state.clone();
        let (kind, id) = split_subject(state.subject);
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO alert_rule_state (rule_id, subject_kind, subject_id, state,
                         since, incident_id, last_notified_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(rule_id, subject_kind, subject_id) DO UPDATE SET
                         state = excluded.state,
                         since = excluded.since,
                         incident_id = excluded.incident_id,
                         last_notified_at = excluded.last_notified_at",
                    rusqlite::params![
                        Sql(state.rule_id),
                        kind,
                        id,
                        alert_state_str(state.state),
                        state.since.map(to_millis),
                        state.incident_id.map(|i| i.to_string()),
                        state.last_notified_at.map(to_millis),
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn open_incidents(&self) -> Result<Vec<Incident>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!(
                    "SELECT {INCIDENT_COLUMNS} FROM incidents
                     WHERE resolved_at IS NULL ORDER BY opened_at DESC"
                );
                let mut statement = connection.prepare(&sql)?;
                statement.query_map([], read_incident)?.collect()
            })
            .await
    }

    async fn recent_incidents(&self, limit: u32) -> Result<Vec<Incident>, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!(
                    "SELECT {INCIDENT_COLUMNS} FROM incidents ORDER BY opened_at DESC LIMIT ?1"
                );
                let mut statement = connection.prepare(&sql)?;
                statement
                    .query_map([i64::from(limit)], read_incident)?
                    .collect()
            })
            .await
    }

    async fn save_incident(&self, incident: &Incident) -> Result<(), RepositoryError> {
        let incident = incident.clone();
        let (kind, id) = split_subject(incident.subject);
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO incidents (id, rule_id, subject_kind, subject_id, severity,
                         summary, opened_at, resolved_at, acknowledged)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                     ON CONFLICT(id) DO UPDATE SET
                         severity = excluded.severity,
                         summary = excluded.summary,
                         resolved_at = excluded.resolved_at,
                         acknowledged = excluded.acknowledged",
                    rusqlite::params![
                        Sql(incident.id),
                        Sql(incident.rule_id),
                        kind,
                        id,
                        incident.severity.as_str(),
                        incident.summary,
                        to_millis(incident.opened_at),
                        incident.resolved_at.map(to_millis),
                        i64::from(incident.acknowledged),
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn get_incident(&self, id: IncidentId) -> Result<Incident, RepositoryError> {
        self.database
            .call(move |connection| {
                let sql = format!("SELECT {INCIDENT_COLUMNS} FROM incidents WHERE id = ?1");
                connection.query_row(&sql, [Sql(id)], read_incident)
            })
            .await
            .map_err(|err| match err {
                RepositoryError::NotFound { .. } => RepositoryError::not_found("incident", id),
                other => other,
            })
    }

    async fn prune_incidents(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError> {
        let cutoff = to_millis(before);
        self.database
            .call(move |connection| {
                // Open incidents are never pruned, however old: an outage that has been
                // running for a year is exactly the thing not to forget about.
                let deleted = connection.execute(
                    "DELETE FROM incidents WHERE resolved_at IS NOT NULL AND opened_at < ?1",
                    [cutoff],
                )?;
                Ok(deleted as u64)
            })
            .await
    }
}

fn alert_state_str(state: AlertState) -> &'static str {
    match state {
        AlertState::Clear => "clear",
        AlertState::Pending => "pending",
        AlertState::Firing => "firing",
    }
}

fn parse_alert_state(raw: &str) -> Result<AlertState, RepositoryError> {
    match raw {
        "clear" => Ok(AlertState::Clear),
        "pending" => Ok(AlertState::Pending),
        "firing" => Ok(AlertState::Firing),
        other => Err(RepositoryError::Corrupt(format!(
            "unknown alert state {other:?}"
        ))),
    }
}

fn read_event(row: &Row<'_>) -> Result<EventEnvelope, rusqlite::Error> {
    let id: String = row.get(0)?;
    let occurred_at: i64 = row.get(1)?;
    let payload_json: String = row.get(2)?;

    Ok(EventEnvelope {
        id: EventId::from_uuid(parse_uuid("events.id", &id).map_err(corrupt)?),
        occurred_at: from_millis(occurred_at).map_err(corrupt)?,
        event: from_json::<DomainEvent>("events.payload_json", &payload_json).map_err(corrupt)?,
    })
}

#[async_trait]
impl EventRepository for SqliteEventRepository {
    async fn append(&self, event: &EventEnvelope) -> Result<(), RepositoryError> {
        let event = event.clone();
        self.database
            .call(move |connection| {
                let subject = event.event.subject().map(split_subject);
                connection.execute(
                    "INSERT INTO events (id, occurred_at, kind, severity, subject_kind,
                         subject_id, payload_json)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)
                     ON CONFLICT(id) DO NOTHING",
                    rusqlite::params![
                        Sql(event.id),
                        to_millis(event.occurred_at),
                        event.event.kind(),
                        event.event.severity().as_str(),
                        subject.as_ref().map(|(kind, _)| *kind),
                        subject.as_ref().map(|(_, id)| id.as_str()),
                        to_json(&event.event)?,
                    ],
                )?;
                Ok(())
            })
            .await
    }

    async fn recent(&self, limit: u32) -> Result<Vec<EventEnvelope>, RepositoryError> {
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT id, occurred_at, payload_json FROM events
                     ORDER BY occurred_at DESC LIMIT ?1",
                )?;
                statement
                    .query_map([i64::from(limit)], read_event)?
                    .collect()
            })
            .await
    }

    async fn recent_for_subject(
        &self,
        subject: AlertSubject,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>, RepositoryError> {
        let (kind, id) = split_subject(subject);
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT id, occurred_at, payload_json FROM events
                     WHERE subject_kind = ?1 AND subject_id = ?2
                     ORDER BY occurred_at DESC LIMIT ?3",
                )?;
                statement
                    .query_map(rusqlite::params![kind, id, i64::from(limit)], read_event)?
                    .collect()
            })
            .await
    }

    async fn in_window(&self, window: TimeWindow) -> Result<Vec<EventEnvelope>, RepositoryError> {
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT id, occurred_at, payload_json FROM events
                     WHERE occurred_at >= ?1 AND occurred_at < ?2
                     ORDER BY occurred_at",
                )?;
                statement
                    .query_map(
                        rusqlite::params![to_millis(window.from), to_millis(window.to)],
                        read_event,
                    )?
                    .collect()
            })
            .await
    }

    async fn prune(&self, before: DateTime<Utc>) -> Result<u64, RepositoryError> {
        let cutoff = to_millis(before);
        self.database
            .call(move |connection| {
                let deleted =
                    connection.execute("DELETE FROM events WHERE occurred_at < ?1", [cutoff])?;
                Ok(deleted as u64)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::metrics::MetricKind;
    use vds_domain::status::ThresholdDirection;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    async fn repositories() -> (SqliteAlertRepository, SqliteEventRepository) {
        let database = Database::open_in_memory().await.expect("opens");
        (
            SqliteAlertRepository::new(database.clone()),
            SqliteEventRepository::new(database),
        )
    }

    fn cpu_rule() -> AlertRule {
        let mut rule = AlertRule::new(
            "CPU high",
            AlertCondition::MetricThreshold {
                metric: MetricKind::CpuUsage,
                direction: ThresholdDirection::Above,
                value: 90.0,
            },
            Status::Warning,
            at(1_000),
        );
        rule.for_duration_secs = 300;
        rule.notify_via = vec![ProviderId::new("desktop"), ProviderId::new("webhook")];
        rule
    }

    #[tokio::test]
    async fn a_rule_round_trips_with_its_condition_and_routing() {
        let (alerts, _) = repositories().await;
        let rule = cpu_rule();
        alerts.save_rule(&rule).await.expect("saved");
        assert_eq!(alerts.get_rule(rule.id).await.expect("loaded"), rule);
    }

    #[tokio::test]
    async fn every_default_rule_round_trips() {
        // Guards the serialisation of every condition variant the app ships with.
        let (alerts, _) = repositories().await;
        for rule in AlertRule::defaults(at(0)) {
            alerts.save_rule(&rule).await.expect("saved");
            assert_eq!(alerts.get_rule(rule.id).await.expect("loaded"), rule);
        }
        assert_eq!(alerts.list_rules().await.expect("listed").len(), 7);
    }

    #[tokio::test]
    async fn rule_state_survives_a_restart() {
        // This is what makes "CPU > 90% for 5 minutes" resilient to the app restarting
        // three minutes in.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("vds.db");
        let rule = cpu_rule();
        let subject = AlertSubject::Server(ServerId::new());

        {
            let alerts = SqliteAlertRepository::new(Database::open(&path).await.expect("opens"));
            alerts.save_rule(&rule).await.expect("saved");
            let mut state = AlertRuleState::clear(rule.id, subject);
            state.state = AlertState::Pending;
            state.since = Some(at(1_000));
            alerts.save_rule_state(&state).await.expect("saved");
        }

        let alerts = SqliteAlertRepository::new(Database::open(&path).await.expect("reopens"));
        let state = alerts
            .load_rule_state(rule.id, subject)
            .await
            .expect("read")
            .expect("present");
        assert_eq!(state.state, AlertState::Pending);
        assert_eq!(
            state.since,
            Some(at(1_000)),
            "the hold timer must not reset"
        );
    }

    #[tokio::test]
    async fn state_is_tracked_per_subject() {
        let (alerts, _) = repositories().await;
        let rule = cpu_rule();
        alerts.save_rule(&rule).await.expect("saved");

        let a = AlertSubject::Server(ServerId::new());
        let b = AlertSubject::Server(ServerId::new());

        let mut firing = AlertRuleState::clear(rule.id, a);
        firing.state = AlertState::Firing;
        alerts.save_rule_state(&firing).await.expect("saved");

        assert_eq!(
            alerts
                .load_rule_state(rule.id, a)
                .await
                .expect("read")
                .map(|s| s.state),
            Some(AlertState::Firing)
        );
        assert_eq!(
            alerts.load_rule_state(rule.id, b).await.expect("read"),
            None
        );
    }

    #[tokio::test]
    async fn deleting_a_rule_removes_its_state() {
        let (alerts, _) = repositories().await;
        let rule = cpu_rule();
        alerts.save_rule(&rule).await.expect("saved");
        let subject = AlertSubject::Server(ServerId::new());
        alerts
            .save_rule_state(&AlertRuleState::clear(rule.id, subject))
            .await
            .expect("saved");

        alerts.delete_rule(rule.id).await.expect("deleted");
        assert_eq!(
            alerts
                .load_rule_state(rule.id, subject)
                .await
                .expect("read"),
            None
        );
    }

    #[tokio::test]
    async fn incidents_round_trip_and_track_resolution() {
        let (alerts, _) = repositories().await;
        let rule = cpu_rule();
        alerts.save_rule(&rule).await.expect("saved");

        let subject = AlertSubject::Server(ServerId::new());
        let mut incident = Incident::open(&rule, subject, "prod-01: CPU is 97.0", at(2_000));
        alerts.save_incident(&incident).await.expect("saved");

        assert_eq!(alerts.open_incidents().await.expect("listed").len(), 1);

        incident.resolved_at = Some(at(3_000));
        alerts.save_incident(&incident).await.expect("resolved");

        assert!(alerts.open_incidents().await.expect("listed").is_empty());
        let loaded = alerts.get_incident(incident.id).await.expect("loaded");
        assert_eq!(loaded, incident);
        assert_eq!(loaded.duration(at(9_999)), chrono::Duration::seconds(1_000));
    }

    #[tokio::test]
    async fn acknowledgement_persists() {
        let (alerts, _) = repositories().await;
        let rule = cpu_rule();
        alerts.save_rule(&rule).await.expect("saved");

        let mut incident =
            Incident::open(&rule, AlertSubject::Server(ServerId::new()), "down", at(1));
        alerts.save_incident(&incident).await.expect("saved");
        incident.acknowledged = true;
        alerts.save_incident(&incident).await.expect("saved");

        assert!(
            alerts
                .get_incident(incident.id)
                .await
                .expect("loaded")
                .acknowledged
        );
    }

    #[tokio::test]
    async fn pruning_never_removes_an_open_incident() {
        // A months-old outage that is still running is the last thing to forget.
        let (alerts, _) = repositories().await;
        let rule = cpu_rule();
        alerts.save_rule(&rule).await.expect("saved");
        let subject = AlertSubject::Server(ServerId::new());

        let open = Incident::open(&rule, subject, "still down", at(1_000));
        alerts.save_incident(&open).await.expect("saved");

        let mut resolved = Incident::open(&rule, subject, "recovered", at(1_000));
        resolved.resolved_at = Some(at(2_000));
        alerts.save_incident(&resolved).await.expect("saved");

        assert_eq!(
            alerts.prune_incidents(at(500_000)).await.expect("pruned"),
            1
        );
        assert_eq!(alerts.open_incidents().await.expect("listed").len(), 1);
    }

    #[tokio::test]
    async fn recent_incidents_are_newest_first() {
        let (alerts, _) = repositories().await;
        let rule = cpu_rule();
        alerts.save_rule(&rule).await.expect("saved");
        let subject = AlertSubject::Server(ServerId::new());

        for seconds in [1_000, 5_000, 3_000] {
            alerts
                .save_incident(&Incident::open(&rule, subject, "x", at(seconds)))
                .await
                .expect("saved");
        }

        let recent = alerts.recent_incidents(2).await.expect("listed");
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].opened_at, at(5_000));
        assert_eq!(recent[1].opened_at, at(3_000));
    }

    #[tokio::test]
    async fn every_event_variant_round_trips_through_storage() {
        let (_, events) = repositories().await;
        let server = ServerId::new();
        let website = WebsiteId::new();

        let samples = vec![
            DomainEvent::ServerStatusChanged {
                server_id: server,
                from: Status::Healthy,
                to: Status::Offline,
                reason: Some("timeout".into()),
            },
            DomainEvent::MetricThresholdExceeded {
                server_id: server,
                metric: MetricKind::CpuUsage,
                value: 97.0,
                threshold: 90.0,
                status: Status::Critical,
            },
            DomainEvent::SslExpiringSoon {
                website_id: website,
                days_remaining: 7,
            },
            DomainEvent::ScreenshotUpdated {
                website_id: website,
            },
        ];

        for (index, event) in samples.iter().enumerate() {
            events
                .append(&EventEnvelope::new(event.clone(), at(index as i64 * 100)))
                .await
                .expect("appended");
        }

        let stored = events.recent(50).await.expect("read");
        assert_eq!(stored.len(), samples.len());
        for original in &samples {
            assert!(
                stored.iter().any(|e| e.event == *original),
                "{original:?} did not survive"
            );
        }
    }

    #[tokio::test]
    async fn events_can_be_filtered_by_subject() {
        let (_, events) = repositories().await;
        let server = ServerId::new();
        let website = WebsiteId::new();

        events
            .append(&EventEnvelope::new(
                DomainEvent::ScreenshotUpdated {
                    website_id: website,
                },
                at(1),
            ))
            .await
            .expect("appended");
        events
            .append(&EventEnvelope::new(
                DomainEvent::ServerStatusChanged {
                    server_id: server,
                    from: Status::Healthy,
                    to: Status::Offline,
                    reason: None,
                },
                at(2),
            ))
            .await
            .expect("appended");

        let for_server = events
            .recent_for_subject(AlertSubject::Server(server), 10)
            .await
            .expect("read");
        assert_eq!(for_server.len(), 1);
        assert_eq!(for_server[0].event.kind(), "server_status_changed");
    }

    #[tokio::test]
    async fn events_in_a_window_are_returned_oldest_first_for_correlation() {
        let (_, events) = repositories().await;
        let website = WebsiteId::new();
        for seconds in [100, 300, 200, 5_000] {
            events
                .append(&EventEnvelope::new(
                    DomainEvent::ScreenshotUpdated {
                        website_id: website,
                    },
                    at(seconds),
                ))
                .await
                .expect("appended");
        }

        let window = events
            .in_window(TimeWindow::new(at(0), at(1_000)))
            .await
            .expect("read");
        let times: Vec<i64> = window.iter().map(|e| e.occurred_at.timestamp()).collect();
        assert_eq!(times, vec![100, 200, 300]);
    }

    #[tokio::test]
    async fn appending_the_same_event_twice_is_idempotent() {
        let (_, events) = repositories().await;
        let envelope = EventEnvelope::new(
            DomainEvent::ScreenshotUpdated {
                website_id: WebsiteId::new(),
            },
            at(1),
        );
        events.append(&envelope).await.expect("appended");
        events.append(&envelope).await.expect("appended again");
        assert_eq!(events.recent(10).await.expect("read").len(), 1);
    }

    #[tokio::test]
    async fn pruning_removes_old_events() {
        let (_, events) = repositories().await;
        let website = WebsiteId::new();
        for seconds in [100, 9_000] {
            events
                .append(&EventEnvelope::new(
                    DomainEvent::ScreenshotUpdated {
                        website_id: website,
                    },
                    at(seconds),
                ))
                .await
                .expect("appended");
        }

        assert_eq!(events.prune(at(5_000)).await.expect("pruned"), 1);
        assert_eq!(events.recent(10).await.expect("read").len(), 1);
    }

    #[test]
    fn subjects_split_and_rejoin() {
        let server = AlertSubject::Server(ServerId::new());
        let (kind, id) = split_subject(server);
        assert_eq!(join_subject(kind, &id).expect("rejoined"), server);

        let website = AlertSubject::Website(WebsiteId::new());
        let (kind, id) = split_subject(website);
        assert_eq!(join_subject(kind, &id).expect("rejoined"), website);
    }

    #[test]
    fn an_unknown_subject_kind_is_corruption() {
        assert!(join_subject("planet", &ServerId::new().to_string()).is_err());
    }

    #[test]
    fn alert_states_round_trip() {
        for state in [AlertState::Clear, AlertState::Pending, AlertState::Firing] {
            assert_eq!(
                parse_alert_state(alert_state_str(state)).expect("valid"),
                state
            );
        }
        assert!(parse_alert_state("smouldering").is_err());
    }
}
