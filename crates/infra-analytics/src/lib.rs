//! # `vds-infra-analytics` — analytics providers
//!
//! Implements [`AnalyticsProvider`](vds_domain::ports::AnalyticsProvider). Yandex.Metrica
//! is the one concrete provider today; the point of the port is that the next one costs a
//! single new module and a registration line. See
//! `docs/adr/003-analytics-provider-architecture.md`.
//!
//! ## Adding a provider
//!
//! 1. Add a module here implementing `AnalyticsProvider`.
//! 2. Declare its [`AnalyticsCapabilities`](vds_domain::analytics::AnalyticsCapabilities)
//!    honestly — the UI hides what a provider says it cannot do.
//! 3. Register it in `vds-composition`.
//! 4. Add tests, ideally including a mapping table like
//!    [`yandex::mapping`] so the translation is reviewable on its own.
//!
//! Nothing in the domain, the application layer, the database schema or the UI changes.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

#[cfg(feature = "demo-providers")]
pub mod demo;
pub mod yandex;

pub use yandex::{PROVIDER_ID as YANDEX_METRICA_PROVIDER_ID, YandexMetricaProvider};
