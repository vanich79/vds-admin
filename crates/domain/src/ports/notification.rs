//! The notification provider port.
//!
//! Desktop and webhook delivery ship in v1; Telegram, email and push are additions of
//! one file each.

use crate::alerts::Notification;
use crate::ids::ProviderId;
use async_trait::async_trait;

/// What a notification channel can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationCapabilities {
    pub supports_rich_body: bool,
    pub supports_sound: bool,
    /// Whether delivery can be confirmed. Desktop toasts generally cannot be.
    pub supports_delivery_confirmation: bool,
    /// Longest body the channel accepts, in characters.
    pub max_body_chars: Option<usize>,
}

/// Why a notification could not be delivered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotificationError {
    #[error("this channel is not available: {0}")]
    Unavailable(String),
    #[error("delivery failed: {0}")]
    Delivery(String),
    #[error("the channel is not configured: {0}")]
    NotConfigured(String),
    #[error("rate limited by the channel")]
    RateLimited,
    #[error("timed out after {seconds}s")]
    Timeout { seconds: u64 },
}

impl NotificationError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            NotificationError::Delivery(_)
                | NotificationError::RateLimited
                | NotificationError::Timeout { .. }
        )
    }
}

/// Delivers alert notifications somewhere.
#[async_trait]
pub trait NotificationProvider: Send + Sync {
    fn id(&self) -> ProviderId;

    fn display_name(&self) -> &'static str;

    fn capabilities(&self) -> NotificationCapabilities;

    /// Whether the channel is configured and usable right now.
    async fn is_available(&self) -> bool;

    async fn notify(&self, notification: &Notification) -> Result<(), NotificationError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_problems_are_not_retryable() {
        assert!(!NotificationError::NotConfigured("no webhook URL".into()).is_retryable());
        assert!(!NotificationError::Unavailable("headless".into()).is_retryable());
        assert!(NotificationError::Delivery("502".into()).is_retryable());
        assert!(NotificationError::RateLimited.is_retryable());
    }
}
