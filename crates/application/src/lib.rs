//! # `vds-application` — use cases, schedulers and engines
//!
//! The layer between the UI and the domain. It orchestrates: it decides *when* a server
//! is collected, *what* a snapshot means for a server's status, *whether* an alert
//! should fire. It never talks to SQLite, SSH, HTTP or a GUI toolkit directly — only to
//! the traits in [`vds_domain::ports`].
//!
//! ```text
//! presentation ──▶ application ──▶ domain ◀── infrastructure
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod alerts;
pub mod analytics;
pub mod config;
pub mod correlation;
pub mod dashboard;
pub mod metrics;
pub mod monitoring;
pub mod provisioning;
pub mod scheduler;
pub mod screenshots;

#[cfg(any(test, feature = "testing"))]
pub mod testing;
