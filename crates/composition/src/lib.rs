//! # `vds-composition` — the composition root
//!
//! The one place that knows which concrete implementation satisfies each port. Every
//! other crate depends on traits; this crate depends on everything and is depended on by
//! nothing except the binaries.
//!
//! That is what makes the substitutions in `docs/ARCHITECTURE.md` §16 cheap: swapping
//! SQLite for PostgreSQL, or adding Google Analytics, changes lines *here* and nowhere
//! else.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod logging;
pub mod paths;

mod wiring;

pub use paths::AppPaths;
pub use wiring::{Application, ApplicationError, SecretsSetup};
