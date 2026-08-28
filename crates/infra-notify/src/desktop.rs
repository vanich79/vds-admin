//! Desktop notifications.
//!
//! Uses the platform's own notification system — the Windows toast API, the macOS
//! Notification Center, and freedesktop notifications on Linux.
//!
//! Two honest limitations, both reflected in the reported capabilities:
//!
//! * **No delivery confirmation.** The platform accepts a notification; whether anyone
//!   saw it is unknowable. A notification channel that cannot confirm delivery must not
//!   be someone's only channel for a production outage, and the settings screen says so.
//! * **Not available on a headless machine.** A Linux server with no session bus has no
//!   notification daemon, and `is_available` returns false there rather than failing on
//!   every alert.

use async_trait::async_trait;
use vds_domain::Status;
use vds_domain::alerts::Notification;
use vds_domain::ids::ProviderId;
use vds_domain::ports::{NotificationCapabilities, NotificationError, NotificationProvider};

/// The provider's stable identifier.
pub const PROVIDER_ID: &str = "desktop";

/// Sends notifications to the desktop.
#[derive(Debug, Clone)]
pub struct DesktopNotificationProvider {
    application_name: String,
    /// Whether to ask the platform to play a sound.
    sound: bool,
}

impl DesktopNotificationProvider {
    pub fn new(application_name: impl Into<String>, sound: bool) -> Self {
        Self {
            application_name: application_name.into(),
            sound,
        }
    }

    /// Whether this build can plausibly show a desktop notification.
    ///
    /// Android and iOS have their own notification systems that this provider does not
    /// speak, so it reports itself unavailable there rather than pretending.
    fn platform_supported() -> bool {
        cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        ))
    }

    /// Whether a session that can display anything appears to exist.
    ///
    /// On Linux, notifications go over the session D-Bus. A systemd service or an SSH
    /// session has neither, and attempting delivery there fails on every single alert.
    #[cfg(target_os = "linux")]
    fn session_available() -> bool {
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("DISPLAY").is_some()
    }

    #[cfg(not(target_os = "linux"))]
    fn session_available() -> bool {
        true
    }

    /// Maps a severity onto the platform's urgency levels.
    fn is_urgent(severity: Status) -> bool {
        matches!(severity, Status::Critical | Status::Offline)
    }
}

impl Default for DesktopNotificationProvider {
    fn default() -> Self {
        Self::new("VDS Admin", false)
    }
}

#[async_trait]
impl NotificationProvider for DesktopNotificationProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn display_name(&self) -> &'static str {
        "Desktop notification"
    }

    fn capabilities(&self) -> NotificationCapabilities {
        NotificationCapabilities {
            supports_rich_body: true,
            supports_sound: true,
            // The platform accepts it; whether a human saw it is unknowable.
            supports_delivery_confirmation: false,
            // Notification daemons truncate long bodies anyway, and a wall of text in a
            // toast is unreadable. The full detail is in the app.
            max_body_chars: Some(256),
        }
    }

    async fn is_available(&self) -> bool {
        Self::platform_supported() && Self::session_available()
    }

    async fn notify(&self, notification: &Notification) -> Result<(), NotificationError> {
        if !self.is_available().await {
            return Err(NotificationError::Unavailable(
                "no desktop session is available to show a notification".to_owned(),
            ));
        }

        let application = self.application_name.clone();
        let summary = notification.title.clone();
        let body = truncate(&notification.body, 256);
        let urgent = Self::is_urgent(notification.severity);
        let sound = self.sound;

        // `notify-rust` is blocking, and on Linux it is a D-Bus round trip that can stall
        // if the daemon is wedged. Keeping it off the runtime's worker threads means a
        // hung notification daemon cannot stall monitoring.
        tokio::task::spawn_blocking(move || {
            let mut builder = notify_rust::Notification::new();
            builder.appname(&application).summary(&summary).body(&body);

            if sound {
                builder.sound_name("message-new-instant");
            }

            #[cfg(all(unix, not(target_os = "macos")))]
            {
                builder.urgency(if urgent {
                    notify_rust::Urgency::Critical
                } else {
                    notify_rust::Urgency::Normal
                });
                // A critical alert should stay on screen until acknowledged rather than
                // vanishing while the operator is looking elsewhere.
                if urgent {
                    builder.timeout(notify_rust::Timeout::Never);
                }
            }
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            let _ = urgent;

            builder
                .show()
                .map(|_| ())
                .map_err(|e| NotificationError::Delivery(e.to_string()))
        })
        .await
        .map_err(|e| NotificationError::Delivery(format!("notification task failed: {e}")))?
    }
}

/// Shortens text to fit a toast, on a character boundary.
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    // Counting characters rather than bytes: slicing a multi-byte character in half
    // would panic, and a Cyrillic or CJK hostname is entirely ordinary here.
    let kept: String = text.chars().take(limit.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use vds_domain::events::AlertSubject;
    use vds_domain::ids::{IncidentId, ServerId};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn notification(severity: Status, body: &str) -> Notification {
        Notification {
            incident_id: IncidentId::new(),
            severity,
            title: "Critical: Server offline".into(),
            body: body.to_owned(),
            subject: AlertSubject::Server(ServerId::new()),
            created_at: at(0),
        }
    }

    #[test]
    fn the_provider_admits_it_cannot_confirm_delivery() {
        // This drives a warning in Settings: a toast must not be someone's only channel
        // for a production outage.
        let capabilities = DesktopNotificationProvider::default().capabilities();
        assert!(!capabilities.supports_delivery_confirmation);
        assert!(capabilities.supports_rich_body);
        assert_eq!(capabilities.max_body_chars, Some(256));
    }

    #[test]
    fn severe_alerts_are_marked_urgent() {
        assert!(DesktopNotificationProvider::is_urgent(Status::Critical));
        assert!(DesktopNotificationProvider::is_urgent(Status::Offline));
        assert!(!DesktopNotificationProvider::is_urgent(Status::Warning));
        assert!(!DesktopNotificationProvider::is_urgent(Status::Healthy));
    }

    #[test]
    fn a_long_body_is_shortened_to_fit_a_toast() {
        let long = "x".repeat(1_000);
        let short = truncate(&long, 256);
        assert_eq!(short.chars().count(), 256);
        assert!(short.ends_with('…'));
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // A Cyrillic or CJK hostname is entirely ordinary; slicing by bytes would panic.
        let cyrillic = "сервер-в-москве ".repeat(40);
        let short = truncate(&cyrillic, 100);
        assert_eq!(short.chars().count(), 100);
        assert!(short.is_char_boundary(short.len()));

        let japanese = "サーバーがダウンしています".repeat(40);
        assert_eq!(truncate(&japanese, 50).chars().count(), 50);
    }

    #[test]
    fn a_short_body_is_left_alone() {
        assert_eq!(
            truncate("prod-01 is unreachable", 256),
            "prod-01 is unreachable"
        );
        assert_eq!(truncate("", 10), "");
    }

    #[tokio::test]
    async fn availability_reflects_the_platform_and_the_session() {
        let provider = DesktopNotificationProvider::default();
        let available = provider.is_available().await;

        if !DesktopNotificationProvider::platform_supported() {
            assert!(!available, "a mobile build has no desktop notifications");
        }
        // On a desktop with a session this is true; on headless CI it is false. Both are
        // correct, and neither is an error.
        assert_eq!(
            available,
            DesktopNotificationProvider::platform_supported()
                && DesktopNotificationProvider::session_available()
        );
    }

    #[tokio::test]
    async fn an_unavailable_desktop_reports_unavailable_rather_than_a_delivery_failure() {
        // The dispatcher skips unavailable channels; a delivery failure would be logged
        // as an error on every single alert.
        let provider = DesktopNotificationProvider::default();
        if provider.is_available().await {
            return;
        }

        let err = provider
            .notify(&notification(Status::Critical, "body"))
            .await
            .expect_err("must fail");
        assert!(
            matches!(err, NotificationError::Unavailable(_)),
            "got {err:?}"
        );
        assert!(!err.is_retryable());
    }

    /// Actually shows a notification.
    ///
    /// Ignored by default: it pops a toast on the developer's desktop.
    #[tokio::test]
    #[ignore = "shows a real desktop notification"]
    async fn a_real_notification_is_shown() {
        let provider = DesktopNotificationProvider::new("VDS Admin (test)", false);
        if !provider.is_available().await {
            return;
        }
        provider
            .notify(&notification(
                Status::Warning,
                "This is a test notification.",
            ))
            .await
            .expect("shown");
    }
}
