//! # `vds-infra-web` — website availability checks
//!
//! Implements the [`WebsiteChecker`](vds_application::monitoring::website::WebsiteChecker)
//! port: DNS resolution, TCP connection, TLS certificate inspection and the HTTP request,
//! each timed and reported separately so a failure says *what* broke.
//!
//! Certificate inspection deliberately uses a permissive verifier on a separate
//! connection — see [`tls`] for why that is the only way to report an expired
//! certificate rather than simply failing on one. The HTTP request itself uses ordinary
//! strict verification.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod checker;
pub mod dns;
pub mod tls;

pub use checker::{CheckerError, HttpWebsiteChecker};
pub use dns::{DnsError, DnsResolver};
pub use tls::{CertificateInspector, TlsInspectionError};
