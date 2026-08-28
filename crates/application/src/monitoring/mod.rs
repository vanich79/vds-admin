//! Monitoring use cases.
//!
//! One collection cycle for a server, one check for a website, plus the two pieces of
//! stateful logic they need: offline detection and network-rate derivation.

pub mod offline;
pub mod rates;
pub mod server;
pub mod website;

pub use offline::{CheckResult, OfflineDetector, Transition};
pub use rates::{InterfaceCounters, NetworkRates, RateTracker};
pub use server::ServerMonitor;
pub use website::WebsiteMonitor;
