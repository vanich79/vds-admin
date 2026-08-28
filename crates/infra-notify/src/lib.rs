//! # `vds-infra-notify` — notification channels
//!
//! Implements [`NotificationProvider`](vds_domain::ports::NotificationProvider). Desktop
//! toasts and webhooks ship today; Telegram, email and push are one module each, with no
//! change to the dispatcher, the alert engine or the domain.
//!
//! ## Adding a channel
//!
//! 1. Implement `NotificationProvider` in a new module.
//! 2. Report [`NotificationCapabilities`](vds_domain::ports::NotificationCapabilities)
//!    honestly, and make `is_available` mean it — a channel that says it is available and
//!    then always fails produces an alert about the alerting.
//! 3. Register it in `vds-composition`.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod desktop;
mod webhook;

pub use desktop::DesktopNotificationProvider;
pub use webhook::{WebhookNotificationProvider, WebhookPayload};
