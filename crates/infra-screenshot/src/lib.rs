//! # `vds-infra-screenshot` — website previews
//!
//! Implements [`ScreenshotProvider`](vds_domain::ports::ScreenshotProvider) with a local
//! headless browser, and
//! [`ScreenshotStore`](vds_application::screenshots::ScreenshotStore) with the
//! filesystem. See `docs/adr/004-screenshot-architecture.md`.
//!
//! The capture policy — when to refresh, how to present a stale image, what to show when
//! a site is down — lives in `vds-application`, not here. This crate only knows how to
//! take a picture and where to put it.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod chromium;
#[cfg(feature = "demo-providers")]
pub mod demo;
pub mod image_ops;
pub mod store;

pub use chromium::{ChromiumScreenshotProvider, PROVIDER_ID as CHROMIUM_PROVIDER_ID};
pub use store::FilesystemScreenshotStore;
