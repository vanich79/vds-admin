//! # `vds-infra-ssh` — agentless SSH monitoring
//!
//! Mode A from the architecture: connect over SSH, run the shared collectors, return a
//! snapshot. See `docs/adr/002-monitoring-architecture.md` and `docs/SSH.md`.
//!
//! Three things here are worth knowing about:
//!
//! * **Batching.** A collection cycle's dozen-odd commands go over one channel in one
//!   round trip ([`batch`]), because a round trip per command is most of a cycle's cost
//!   at fleet scale.
//! * **Pooling.** Sessions are reused per server. The SSH handshake is expensive, and
//!   repeating it every fifteen seconds for hundreds of servers is not viable.
//! * **Host keys.** Trust-on-first-use, then pinned. A changed key is refused, never
//!   silently re-trusted ([`known_hosts`]).
//!
//! `russh` is used rather than an OpenSSL-backed client so the whole thing
//! cross-compiles to ARM and Android without a system TLS library.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod batch;
pub mod files;
pub mod known_hosts;
pub mod probe;
pub mod session;

pub use known_hosts::{HostKeyVerdict, KnownHosts};
pub use probe::SshServerProbe;
pub use session::{SshCommandRunner, SshCredential, SshSession, SshSettings};
