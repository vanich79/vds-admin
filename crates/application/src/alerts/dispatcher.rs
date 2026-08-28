//! Notification fan-out.
//!
//! Providers are registered, not hard-coded. Adding Telegram or email is one new
//! implementation of [`NotificationProvider`] plus a registration line — no change here.

use std::sync::Arc;
use vds_domain::Status;
use vds_domain::alerts::{AlertRule, Notification};
use vds_domain::ids::ProviderId;
use vds_domain::ports::{NotificationError, NotificationProvider};

/// Result of attempting to deliver one notification everywhere.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeliveryReport {
    pub delivered: Vec<ProviderId>,
    pub failed: Vec<(ProviderId, String)>,
    /// Providers that reported themselves unavailable and were not attempted.
    pub skipped: Vec<ProviderId>,
}

impl DeliveryReport {
    /// Whether the notification reached at least one channel.
    pub fn reached_anyone(&self) -> bool {
        !self.delivered.is_empty()
    }
}

/// Sends notifications through every configured provider.
pub struct NotificationDispatcher {
    providers: Vec<Arc<dyn NotificationProvider>>,
    /// Notifications below this severity are dropped.
    min_severity: Status,
}

impl NotificationDispatcher {
    pub fn new(providers: Vec<Arc<dyn NotificationProvider>>, min_severity: Status) -> Self {
        Self {
            providers,
            min_severity,
        }
    }

    pub fn provider_ids(&self) -> Vec<ProviderId> {
        self.providers.iter().map(|p| p.id()).collect()
    }

    /// Delivers a notification.
    ///
    /// `rule` selects the channels: an empty `notify_via` means every provider, which is
    /// the sensible default for a user who has not thought about routing yet.
    pub async fn dispatch(&self, notification: &Notification, rule: &AlertRule) -> DeliveryReport {
        let mut report = DeliveryReport::default();

        if notification.severity < self.min_severity {
            return report;
        }

        for provider in &self.providers {
            let id = provider.id();
            if !rule.notify_via.is_empty() && !rule.notify_via.contains(&id) {
                continue;
            }

            if !provider.is_available().await {
                // A headless Linux box has no desktop notification daemon; that is a
                // configuration fact, not an error to report on every alert.
                report.skipped.push(id);
                continue;
            }

            match provider.notify(notification).await {
                Ok(()) => report.delivered.push(id),
                Err(err) => {
                    // One channel failing must never stop the others — that is the whole
                    // point of having several.
                    tracing::warn!(provider = %id, error = %err, "notification delivery failed");
                    report.failed.push((id, err.to_string()));
                }
            }
        }

        if !report.reached_anyone() && !self.providers.is_empty() {
            tracing::warn!(
                incident = %notification.incident_id,
                "no notification channel accepted this alert"
            );
        }

        report
    }

    /// Whether any provider is currently usable, for the settings screen.
    pub async fn any_available(&self) -> bool {
        for provider in &self.providers {
            if provider.is_available().await {
                return true;
            }
        }
        false
    }
}

/// Builds the notification a firing rule should send.
pub fn notification_for(
    rule: &AlertRule,
    subject: vds_domain::events::AlertSubject,
    incident_id: vds_domain::ids::IncidentId,
    summary: String,
    now: chrono::DateTime<chrono::Utc>,
) -> Notification {
    Notification {
        incident_id,
        severity: rule.severity,
        title: format!("{}: {}", severity_word(rule.severity), rule.name),
        body: summary,
        subject,
        created_at: now,
    }
}

fn severity_word(severity: Status) -> &'static str {
    match severity {
        Status::Critical => "Critical",
        Status::Offline => "Offline",
        Status::Warning => "Warning",
        Status::Healthy => "Resolved",
        Status::Unknown => "Unknown",
    }
}

/// Turns [`NotificationError`] into a retry decision for the scheduler.
pub fn is_worth_retrying(err: &NotificationError) -> bool {
    err.is_retryable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::RecordingNotificationProvider;
    use chrono::DateTime;
    use vds_domain::alerts::AlertCondition;
    use vds_domain::events::AlertSubject;
    use vds_domain::ids::{IncidentId, ServerId};

    fn at(secs: i64) -> DateTime<chrono::Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn rule() -> AlertRule {
        AlertRule::new(
            "Server offline",
            AlertCondition::ServerOffline,
            Status::Critical,
            at(0),
        )
    }

    fn notification(severity: Status) -> Notification {
        Notification {
            incident_id: IncidentId::new(),
            severity,
            title: "Critical: Server offline".into(),
            body: "prod-01 is unreachable".into(),
            subject: AlertSubject::Server(ServerId::new()),
            created_at: at(100),
        }
    }

    #[tokio::test]
    async fn a_notification_reaches_every_provider() {
        let a = Arc::new(RecordingNotificationProvider::new());
        let b = Arc::new(RecordingNotificationProvider::new());
        let dispatcher = NotificationDispatcher::new(
            vec![
                Arc::clone(&a) as Arc<dyn NotificationProvider>,
                Arc::clone(&b) as Arc<dyn NotificationProvider>,
            ],
            Status::Warning,
        );

        let report = dispatcher
            .dispatch(&notification(Status::Critical), &rule())
            .await;

        assert_eq!(report.delivered.len(), 2);
        assert!(report.reached_anyone());
        assert_eq!(a.delivered().len(), 1);
        assert_eq!(b.delivered().len(), 1);
    }

    #[tokio::test]
    async fn one_failing_channel_does_not_stop_the_others() {
        // The reason for having several channels in the first place.
        let broken = Arc::new(RecordingNotificationProvider::new());
        broken.fail_delivery(true);
        let working = Arc::new(RecordingNotificationProvider::new());

        let dispatcher = NotificationDispatcher::new(
            vec![
                Arc::clone(&broken) as Arc<dyn NotificationProvider>,
                Arc::clone(&working) as Arc<dyn NotificationProvider>,
            ],
            Status::Warning,
        );

        let report = dispatcher
            .dispatch(&notification(Status::Critical), &rule())
            .await;

        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.delivered.len(), 1);
        assert!(report.reached_anyone());
        assert_eq!(working.delivered().len(), 1);
    }

    #[tokio::test]
    async fn an_unavailable_channel_is_skipped_rather_than_reported_as_a_failure() {
        // A headless server has no desktop notification daemon. That is a fact about the
        // machine, not an error worth surfacing on every single alert.
        let absent = Arc::new(RecordingNotificationProvider::new());
        absent.set_available(false);

        let dispatcher = NotificationDispatcher::new(
            vec![Arc::clone(&absent) as Arc<dyn NotificationProvider>],
            Status::Warning,
        );
        let report = dispatcher
            .dispatch(&notification(Status::Critical), &rule())
            .await;

        assert_eq!(report.skipped.len(), 1);
        assert!(report.failed.is_empty());
        assert!(absent.delivered().is_empty());
    }

    #[tokio::test]
    async fn notifications_below_the_minimum_severity_are_dropped() {
        let provider = Arc::new(RecordingNotificationProvider::new());
        let dispatcher = NotificationDispatcher::new(
            vec![Arc::clone(&provider) as Arc<dyn NotificationProvider>],
            Status::Critical,
        );

        dispatcher
            .dispatch(&notification(Status::Warning), &rule())
            .await;
        assert!(provider.delivered().is_empty());

        dispatcher
            .dispatch(&notification(Status::Critical), &rule())
            .await;
        assert_eq!(provider.delivered().len(), 1);
    }

    #[tokio::test]
    async fn a_rule_can_route_to_specific_channels_only() {
        let chosen = Arc::new(RecordingNotificationProvider::new());
        let dispatcher = NotificationDispatcher::new(
            vec![Arc::clone(&chosen) as Arc<dyn NotificationProvider>],
            Status::Warning,
        );

        let mut routed = rule();
        routed.notify_via = vec![ProviderId::new("telegram")];
        dispatcher
            .dispatch(&notification(Status::Critical), &routed)
            .await;
        assert!(
            chosen.delivered().is_empty(),
            "a rule routed elsewhere must not deliver here"
        );

        routed.notify_via = vec![ProviderId::new("recording")];
        dispatcher
            .dispatch(&notification(Status::Critical), &routed)
            .await;
        assert_eq!(chosen.delivered().len(), 1);
    }

    #[tokio::test]
    async fn an_empty_route_list_means_every_channel() {
        let provider = Arc::new(RecordingNotificationProvider::new());
        let dispatcher = NotificationDispatcher::new(
            vec![Arc::clone(&provider) as Arc<dyn NotificationProvider>],
            Status::Warning,
        );

        let mut unrouted = rule();
        unrouted.notify_via.clear();
        dispatcher
            .dispatch(&notification(Status::Critical), &unrouted)
            .await;
        assert_eq!(provider.delivered().len(), 1);
    }

    #[tokio::test]
    async fn a_dispatcher_with_no_providers_is_harmless() {
        let dispatcher = NotificationDispatcher::new(Vec::new(), Status::Warning);
        let report = dispatcher
            .dispatch(&notification(Status::Critical), &rule())
            .await;
        assert!(!report.reached_anyone());
        assert!(!dispatcher.any_available().await);
    }

    #[test]
    fn the_notification_title_leads_with_the_severity() {
        let notification = notification_for(
            &rule(),
            AlertSubject::Server(ServerId::new()),
            IncidentId::new(),
            "prod-01 is unreachable".into(),
            at(0),
        );
        assert_eq!(notification.title, "Critical: Server offline");
        assert_eq!(notification.body, "prod-01 is unreachable");
        assert_eq!(notification.severity, Status::Critical);
    }
}
