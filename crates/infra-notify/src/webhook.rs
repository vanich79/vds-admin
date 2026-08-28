//! Webhook delivery.
//!
//! The most useful channel to ship early, because it is the one that composes: a webhook
//! reaches Slack, Discord, PagerDuty, a home-grown script or a Telegram bridge without
//! this project needing to know about any of them.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use vds_domain::alerts::Notification;
use vds_domain::ids::ProviderId;
use vds_domain::ports::{NotificationCapabilities, NotificationError, NotificationProvider};

/// The provider's stable identifier.
pub const PROVIDER_ID: &str = "webhook";

/// How long to wait for the endpoint.
///
/// Short on purpose: a notification that takes half a minute to deliver is holding up
/// the alerting pass behind it.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The JSON body posted to the endpoint.
///
/// A flat, stable shape so a receiving script does not have to dig. It is part of this
/// project's public contract — changing a field name breaks other people's integrations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookPayload {
    /// Always `"vds-admin"`, so a shared endpoint can tell where a message came from.
    pub source: String,
    pub incident_id: String,
    pub severity: String,
    pub title: String,
    pub body: String,
    /// `"server"` or `"website"`.
    pub subject_kind: String,
    pub subject_id: String,
    /// RFC 3339.
    pub occurred_at: String,
}

impl WebhookPayload {
    pub fn from_notification(notification: &Notification) -> Self {
        let (subject_kind, subject_id) = match notification.subject {
            vds_domain::events::AlertSubject::Server(id) => ("server".to_owned(), id.to_string()),
            vds_domain::events::AlertSubject::Website(id) => ("website".to_owned(), id.to_string()),
        };

        Self {
            source: "vds-admin".to_owned(),
            incident_id: notification.incident_id.to_string(),
            severity: notification.severity.as_str().to_owned(),
            title: notification.title.clone(),
            body: notification.body.clone(),
            subject_kind,
            subject_id,
            occurred_at: notification.created_at.to_rfc3339(),
        }
    }
}

/// Posts notifications to an HTTP endpoint.
pub struct WebhookNotificationProvider {
    client: reqwest::Client,
    url: Option<String>,
}

impl WebhookNotificationProvider {
    /// Builds a provider. An empty or absent URL disables the channel.
    pub fn new(url: Option<String>) -> Result<Self, NotificationError> {
        // reqwest is built without a bundled crypto provider so that aws-lc-rs stays out
        // of the tree; without this it panics on the first HTTPS request. Idempotent.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("vds-admin/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| NotificationError::Unavailable(e.to_string()))?;

        let url = url.filter(|u| !u.trim().is_empty());
        Ok(Self { client, url })
    }

    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
}

#[async_trait]
impl NotificationProvider for WebhookNotificationProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn display_name(&self) -> &'static str {
        "Webhook"
    }

    fn capabilities(&self) -> NotificationCapabilities {
        NotificationCapabilities {
            supports_rich_body: true,
            supports_sound: false,
            // An HTTP 2xx is a real acknowledgement, unlike a desktop toast.
            supports_delivery_confirmation: true,
            max_body_chars: None,
        }
    }

    async fn is_available(&self) -> bool {
        self.url.is_some()
    }

    async fn notify(&self, notification: &Notification) -> Result<(), NotificationError> {
        let Some(url) = &self.url else {
            return Err(NotificationError::NotConfigured(
                "no webhook URL is set".to_owned(),
            ));
        };

        let payload = WebhookPayload::from_notification(notification);

        let response = self
            .client
            .post(url)
            .json(&payload)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    NotificationError::Timeout {
                        seconds: TIMEOUT.as_secs(),
                    }
                } else {
                    NotificationError::Delivery(err.to_string())
                }
            })?;

        let status = response.status();
        if status.is_success() {
            Ok(())
        } else if status.as_u16() == 429 {
            Err(NotificationError::RateLimited)
        } else if status.is_client_error() {
            // A 4xx means the endpoint is wrong or rejects us; retrying will not help,
            // and an alert storm of failed deliveries helps nobody.
            Err(NotificationError::NotConfigured(format!(
                "the endpoint rejected the notification: HTTP {status}"
            )))
        } else {
            Err(NotificationError::Delivery(format!("HTTP {status}")))
        }
    }
}

impl std::fmt::Debug for WebhookNotificationProvider {
    /// Hand-written: a webhook URL frequently *is* the credential — Slack and Discord
    /// URLs contain their own secret token.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookNotificationProvider")
            .field("configured", &self.url.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use vds_domain::Status;
    use vds_domain::events::AlertSubject;
    use vds_domain::ids::{IncidentId, ServerId, WebsiteId};
    use wiremock::matchers::{body_json_schema, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn notification() -> Notification {
        Notification {
            incident_id: IncidentId::new(),
            severity: Status::Critical,
            title: "Critical: Server offline".into(),
            body: "prod-01 is unreachable".into(),
            subject: AlertSubject::Server(ServerId::new()),
            created_at: at(1_700_000_000),
        }
    }

    async fn provider_for(server: &MockServer, path_suffix: &str) -> WebhookNotificationProvider {
        WebhookNotificationProvider::new(Some(format!("{}{path_suffix}", server.uri())))
            .expect("builds")
    }

    #[tokio::test]
    async fn a_notification_is_posted_as_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .and(body_json_schema::<WebhookPayload>)
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let provider = provider_for(&server, "/hook").await;
        assert!(provider.is_available().await);
        provider.notify(&notification()).await.expect("delivered");
    }

    #[tokio::test]
    async fn the_payload_carries_everything_a_receiver_needs() {
        let notification = notification();
        let payload = WebhookPayload::from_notification(&notification);

        assert_eq!(payload.source, "vds-admin");
        assert_eq!(payload.severity, "critical");
        assert_eq!(payload.subject_kind, "server");
        assert_eq!(payload.title, "Critical: Server offline");
        assert_eq!(payload.body, "prod-01 is unreachable");
        // RFC 3339 so any language can parse it.
        assert!(
            payload.occurred_at.contains('T'),
            "timestamp was {}",
            payload.occurred_at
        );
    }

    #[test]
    fn website_subjects_are_labelled_distinctly_from_servers() {
        let mut notification = notification();
        notification.subject = AlertSubject::Website(WebsiteId::new());
        assert_eq!(
            WebhookPayload::from_notification(&notification).subject_kind,
            "website"
        );
    }

    #[tokio::test]
    async fn a_server_error_is_retryable_but_a_client_error_is_not() {
        // A wrong URL would otherwise be retried on every alert, forever.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/broken"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/gone"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let err = provider_for(&server, "/broken")
            .await
            .notify(&notification())
            .await
            .expect_err("must fail");
        assert!(err.is_retryable(), "a 5xx should be retried");

        let err = provider_for(&server, "/gone")
            .await
            .notify(&notification())
            .await
            .expect_err("must fail");
        assert!(
            matches!(err, NotificationError::NotConfigured(_)),
            "got {err:?}"
        );
        assert!(!err.is_retryable());
    }

    #[tokio::test]
    async fn a_rate_limited_endpoint_is_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/busy"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let err = provider_for(&server, "/busy")
            .await
            .notify(&notification())
            .await
            .expect_err("must fail");
        assert_eq!(err, NotificationError::RateLimited);
        assert!(err.is_retryable());
    }

    #[tokio::test]
    async fn a_slow_endpoint_does_not_hold_up_alerting_forever() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/slow"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
            .mount(&server)
            .await;

        let started = std::time::Instant::now();
        let err = provider_for(&server, "/slow")
            .await
            .notify(&notification())
            .await
            .expect_err("must time out");

        assert!(
            matches!(err, NotificationError::Timeout { .. }),
            "got {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(25),
            "the timeout was not enforced"
        );
    }

    #[tokio::test]
    async fn an_unconfigured_webhook_reports_itself_unavailable_rather_than_failing() {
        // The dispatcher skips unavailable channels instead of logging an error per alert.
        let provider = WebhookNotificationProvider::new(None).expect("builds");
        assert!(!provider.is_available().await);

        let err = provider
            .notify(&notification())
            .await
            .expect_err("must fail");
        assert!(matches!(err, NotificationError::NotConfigured(_)));
        assert!(!err.is_retryable());
    }

    #[tokio::test]
    async fn a_blank_url_counts_as_unconfigured() {
        let provider = WebhookNotificationProvider::new(Some("   ".into())).expect("builds");
        assert!(!provider.is_available().await);
    }

    #[test]
    fn the_debug_output_never_contains_the_url() {
        // Slack and Discord webhook URLs contain their own secret token.
        let provider = WebhookNotificationProvider::new(Some(
            "https://hooks.slack.com/services/T000/B000/XXXXsecretXXXX".into(),
        ))
        .expect("builds");

        let rendered = format!("{provider:?}");
        assert!(
            !rendered.contains("secret"),
            "Debug leaked the URL: {rendered}"
        );
        assert!(rendered.contains("configured: true"));
    }

    #[test]
    fn every_severity_serialises_to_a_stable_label() {
        // Receiving scripts branch on this string; it is part of the public contract.
        for (status, expected) in [
            (Status::Healthy, "healthy"),
            (Status::Warning, "warning"),
            (Status::Critical, "critical"),
            (Status::Offline, "offline"),
            (Status::Unknown, "unknown"),
        ] {
            let mut notification = notification();
            notification.severity = status;
            assert_eq!(
                WebhookPayload::from_notification(&notification).severity,
                expected
            );
        }
    }

    #[test]
    fn the_payload_round_trips_so_a_rust_receiver_can_decode_it() {
        let payload = WebhookPayload::from_notification(&notification());
        let json = serde_json::to_string(&payload).expect("serialises");
        let back: WebhookPayload = serde_json::from_slice(json.as_bytes()).expect("deserialises");
        assert_eq!(back, payload);
    }
}
