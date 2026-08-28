//! # `vds-infra-collectors` — Linux metric collectors
//!
//! One parsing layer, shared by two very different callers:
//!
//! * the desktop/mobile app, which drives it over SSH (`vds-infra-ssh`);
//! * `vds-agent`, which drives it against the local machine.
//!
//! The split that makes that possible is described in
//! `docs/adr/002-monitoring-architecture.md`: a [`Collector`](vds_domain::ports::Collector)
//! declares the [`Command`](vds_domain::ports::Command)s it needs and then *parses*
//! their output. It performs no I/O, so every parser is a plain synchronous function
//! that can be tested against captured real-world output.
//!
//! ```no_run
//! use vds_infra_collectors::{CollectorRegistry, LocalCommandRunner};
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let registry = CollectorRegistry::linux();
//! let runner = LocalCommandRunner::default();
//! let snapshot = registry
//!     .collect(&runner, Default::default(), chrono::Utc::now())
//!     .await?;
//! println!("{:?}", snapshot.cpu.total_percent);
//! # Ok(()) }
//! ```
//!
//! ## Design rules every collector follows
//!
//! * Unfamiliar input degrades to
//!   [`MetricValue::NotAvailable`](vds_domain::metrics::MetricValue::NotAvailable); it
//!   never panics and never substitutes a zero.
//! * A host that simply lacks a feature — no Docker, no systemd, no thermal sensors —
//!   yields [`CollectError::Unsupported`](vds_domain::ports::CollectError::Unsupported),
//!   which explicitly does *not* count against the server's health.
//! * One collector failing never voids the rest of the cycle.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod cpu;
pub mod disk;
pub mod docker;
pub mod load;
pub mod memory;
pub mod network;
pub mod parse;
pub mod process;
pub mod registry;
pub mod runner;
pub mod service;
pub mod system;
pub mod temperature;

pub use cpu::CpuCollector;
pub use disk::DiskCollector;
pub use docker::DockerCollector;
pub use load::LoadCollector;
pub use memory::MemoryCollector;
pub use network::NetworkCollector;
pub use process::ProcessCollector;
pub use registry::{CollectionPlan, CollectorRegistry};
pub use runner::{LocalCommandRunner, ScriptedCommandRunner};
pub use service::ServiceCollector;
pub use system::SystemCollector;
pub use temperature::TemperatureCollector;
