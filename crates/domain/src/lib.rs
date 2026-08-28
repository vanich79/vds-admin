//! # `vds-domain` — the core of VDS Admin
//!
//! Entities, value objects, domain events and **ports**: the traits that everything
//! outside must implement. This crate has no I/O dependencies at all — no database, no
//! HTTP client, no SSH library, no GUI toolkit — and `scripts/check-layering.sh`
//! enforces that in CI.
//!
//! ```text
//! presentation ──▶ application ──▶ domain ◀── infrastructure
//! ```
//!
//! ## Layout
//!
//! | Module | Contents |
//! |---|---|
//! | [`status`] | the one [`Status`] model and its thresholds |
//! | [`ids`] | typed identifiers |
//! | [`metrics`] | metric vocabulary, samples, rollups, time ranges |
//! | [`server`] | the server aggregate and its snapshot |
//! | [`website`] | the website aggregate and its checks |
//! | [`analytics`] | provider-independent web analytics |
//! | [`screenshot`] | screenshot records and freshness policy |
//! | [`alerts`] | alert rules and incidents |
//! | [`events`] | the domain event vocabulary |
//! | [`ports`] | every trait the outside world implements |
//!
//! ## Conventions
//!
//! * A measurement that could not be taken is [`metrics::MetricValue::NotAvailable`],
//!   never a substituted zero.
//! * Absence of data is [`Status::Unknown`], never [`Status::Healthy`].
//! * Secret material exists only behind [`ports::SecretStore`]; entities carry
//!   [`ids::CredentialRef`] handles.

#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod alerts;
pub mod analytics;
pub mod events;
pub mod ids;
pub mod metrics;
pub mod ports;
pub mod screenshot;
pub mod server;
pub mod status;
pub mod website;

pub use status::{Status, Threshold, ThresholdDirection};

/// The types most consumers need, in one import.
pub mod prelude {
    pub use crate::alerts::{AlertCondition, AlertRule, AlertRuleState, AlertState, Incident};
    pub use crate::analytics::{
        AnalyticsCapabilities, AnalyticsIntegration, AnalyticsInterval, AnalyticsMetric,
        AnalyticsPeriod, AnalyticsSnapshot, AnalyticsTimeSeries, DateRange,
    };
    pub use crate::events::{AlertSubject, DomainEvent, EventEnvelope};
    pub use crate::ids::{
        AlertRuleId, CollectorId, CredentialRef, EventId, IncidentId, IntegrationId, ProviderId,
        ServerId, WebsiteId,
    };
    pub use crate::metrics::{
        MetricKind, MetricResult, MetricSample, MetricSeries, MetricValue, Resolution, TimeRange,
        TimeWindow,
    };
    pub use crate::ports::*;
    pub use crate::screenshot::{Screenshot, ScreenshotPresentation, ScreenshotRefreshPolicy};
    pub use crate::server::{
        ConnectionMode, ConnectionSettings, Server, ServerRuntimeState, ServerSnapshot,
    };
    pub use crate::status::{Status, Threshold, ThresholdDirection};
    pub use crate::website::{Website, WebsiteCheck, WebsiteRuntimeState};
}
