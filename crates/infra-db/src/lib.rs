//! # `vds-infra-db` — SQLite storage
//!
//! Implements the repository ports from [`vds_domain::ports`] against SQLite, compiled
//! from vendored C source so it cross-compiles to ARM and Android without a system
//! package. See `docs/adr/005-metrics-storage.md`.
//!
//! Nothing here leaks upwards: `rusqlite::Error` is translated into
//! [`RepositoryError`](vds_domain::ports::RepositoryError) at the boundary, which is what
//! makes a PostgreSQL implementation a contained piece of work.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod connection;
mod convert;

pub mod migrations;

mod alerts;
mod analytics;
mod metrics;
mod screenshots;
mod servers;
mod websites;

pub use alerts::{SqliteAlertRepository, SqliteEventRepository};
pub use analytics::SqliteAnalyticsRepository;
pub use connection::Database;
pub use metrics::SqliteMetricsRepository;
pub use screenshots::SqliteScreenshotRepository;
pub use servers::SqliteServerRepository;
pub use websites::SqliteWebsiteRepository;
