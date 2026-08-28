//! Host key verification.
//!
//! Trust-on-first-use, then pinned. Accepting any host key would leave every SSH
//! connection open to interception, and this application carries credentials for the
//! machines it connects to — so a changed key is refused, loudly, and never silently
//! re-trusted.
//!
//! The store is a JSON file rather than OpenSSH's `known_hosts` format: it is this
//! application's own trust decision, and silently inheriting (or worse, editing) the
//! user's `~/.ssh/known_hosts` would be presumptuous.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use vds_domain::ports::TransportError;

/// The verdict on a presented host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyVerdict {
    /// The key matches what was pinned.
    Known,
    /// No key was pinned; this one has now been recorded.
    TrustedOnFirstUse,
    /// A different key was pinned. The connection must be refused.
    Changed { expected: String, presented: String },
}

impl HostKeyVerdict {
    pub fn is_acceptable(&self) -> bool {
        !matches!(self, HostKeyVerdict::Changed { .. })
    }
}

/// The persisted file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct KnownHostsFile {
    #[serde(default)]
    hosts: BTreeMap<String, String>,
}

/// Pinned host keys.
#[derive(Debug)]
pub struct KnownHosts {
    path: Option<PathBuf>,
    entries: RwLock<BTreeMap<String, String>>,
}

impl KnownHosts {
    /// Loads a store from disk, creating an empty one if the file does not exist.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, TransportError> {
        let path = path.as_ref().to_path_buf();

        let entries = if path.exists() {
            let text = std::fs::read_to_string(&path).map_err(|e| {
                TransportError::Protocol(format!("could not read known hosts: {e}"))
            })?;
            serde_json::from_str::<KnownHostsFile>(&text)
                .map_err(|e| TransportError::Protocol(format!("known hosts file is corrupt: {e}")))?
                .hosts
        } else {
            BTreeMap::new()
        };

        Ok(Self {
            path: Some(path),
            entries: RwLock::new(entries),
        })
    }

    /// An in-memory store, for tests.
    pub fn in_memory() -> Self {
        Self {
            path: None,
            entries: RwLock::new(BTreeMap::new()),
        }
    }

    /// Checks a presented key, pinning it if the host is new.
    pub fn verify(&self, host: &str, port: u16, key_bytes: &[u8]) -> HostKeyVerdict {
        let identity = host_identity(host, port);
        let presented = fingerprint(key_bytes);

        let existing = self.entries.read().get(&identity).cloned();

        match existing {
            Some(expected) if expected == presented => HostKeyVerdict::Known,
            Some(expected) => {
                // Never auto-update. A changed key is either a rebuilt server — which the
                // user must confirm — or an interception.
                tracing::warn!(
                    host = %identity,
                    "the host key has changed; refusing to connect"
                );
                HostKeyVerdict::Changed {
                    expected,
                    presented,
                }
            }
            None => {
                self.entries.write().insert(identity.clone(), presented);
                if let Err(err) = self.persist() {
                    // Failing to persist means we will trust-on-first-use again next
                    // time, which is weaker but not wrong. Worth a warning, not a refusal.
                    tracing::warn!(error = %err, "could not persist the host key");
                }
                tracing::info!(host = %identity, "pinned a new host key");
                HostKeyVerdict::TrustedOnFirstUse
            }
        }
    }

    /// Forgets a host's pinned key, so the next connection re-pins.
    ///
    /// This is what the UI calls when a user confirms they rebuilt the server.
    pub fn forget(&self, host: &str, port: u16) -> Result<(), TransportError> {
        self.entries.write().remove(&host_identity(host, port));
        self.persist()
    }

    /// The pinned fingerprint for a host, if any.
    pub fn fingerprint_for(&self, host: &str, port: u16) -> Option<String> {
        self.entries.read().get(&host_identity(host, port)).cloned()
    }

    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn persist(&self) -> Result<(), TransportError> {
        let Some(path) = &self.path else {
            return Ok(());
        };

        let file = KnownHostsFile {
            hosts: self.entries.read().clone(),
        };
        let text = serde_json::to_string_pretty(&file)
            .map_err(|e| TransportError::Protocol(format!("could not serialise: {e}")))?;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                TransportError::Protocol(format!("could not create {parent:?}: {e}"))
            })?;
        }

        // Written via a temporary file and renamed, so an interrupted write cannot leave
        // a truncated store — which would silently re-trust every host.
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, text.as_bytes())
            .map_err(|e| TransportError::Protocol(format!("could not write: {e}")))?;
        std::fs::rename(&temporary, path)
            .map_err(|e| TransportError::Protocol(format!("could not replace: {e}")))
    }
}

/// The key a host is filed under.
///
/// The port is part of it: two SSH daemons on one machine are two hosts.
fn host_identity(host: &str, port: u16) -> String {
    format!("{host}:{port}")
}

/// SHA-256 fingerprint in the form OpenSSH displays.
pub fn fingerprint(key_bytes: &[u8]) -> String {
    let digest = Sha256::digest(key_bytes);
    format!("SHA256:{}", BASE64.encode(digest).trim_end_matches('='))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &[u8] = b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIexample-key-a";
    const KEY_B: &[u8] = b"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIexample-key-b";

    #[test]
    fn the_first_key_seen_is_pinned() {
        let store = KnownHosts::in_memory();
        assert_eq!(
            store.verify("host", 22, KEY_A),
            HostKeyVerdict::TrustedOnFirstUse
        );
        assert_eq!(store.verify("host", 22, KEY_A), HostKeyVerdict::Known);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn a_changed_key_is_refused_and_never_silently_re_pinned() {
        // The interception case. Auto-updating here would defeat the entire mechanism.
        let store = KnownHosts::in_memory();
        store.verify("host", 22, KEY_A);

        let verdict = store.verify("host", 22, KEY_B);
        assert!(matches!(verdict, HostKeyVerdict::Changed { .. }));
        assert!(!verdict.is_acceptable());

        // And it stays refused on every subsequent attempt.
        assert!(!store.verify("host", 22, KEY_B).is_acceptable());
        assert_eq!(store.fingerprint_for("host", 22), Some(fingerprint(KEY_A)));
    }

    #[test]
    fn the_refusal_names_both_fingerprints_so_a_user_can_check() {
        let store = KnownHosts::in_memory();
        store.verify("host", 22, KEY_A);

        match store.verify("host", 22, KEY_B) {
            HostKeyVerdict::Changed {
                expected,
                presented,
            } => {
                assert_eq!(expected, fingerprint(KEY_A));
                assert_eq!(presented, fingerprint(KEY_B));
                assert_ne!(expected, presented);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn forgetting_a_host_allows_it_to_be_re_pinned() {
        // What the UI calls when a user confirms they rebuilt the machine.
        let store = KnownHosts::in_memory();
        store.verify("host", 22, KEY_A);
        store.forget("host", 22).expect("forgotten");

        assert_eq!(
            store.verify("host", 22, KEY_B),
            HostKeyVerdict::TrustedOnFirstUse
        );
    }

    #[test]
    fn two_daemons_on_one_machine_are_separate_hosts() {
        let store = KnownHosts::in_memory();
        store.verify("host", 22, KEY_A);
        assert_eq!(
            store.verify("host", 2222, KEY_B),
            HostKeyVerdict::TrustedOnFirstUse
        );
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn pins_survive_a_restart() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("known_hosts.json");

        {
            let store = KnownHosts::load(&path).expect("loads");
            store.verify("host", 22, KEY_A);
        }

        let store = KnownHosts::load(&path).expect("reloads");
        assert_eq!(store.verify("host", 22, KEY_A), HostKeyVerdict::Known);
        // And a different key is still refused after the restart.
        assert!(!store.verify("host", 22, KEY_B).is_acceptable());
    }

    #[test]
    fn a_missing_store_starts_empty_rather_than_failing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = KnownHosts::load(dir.path().join("nothing-here.json")).expect("loads");
        assert!(store.is_empty());
    }

    #[test]
    fn a_corrupt_store_is_reported_rather_than_silently_discarded() {
        // Silently starting fresh would re-trust every host without telling anyone.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("known_hosts.json");
        std::fs::write(&path, "{ not json").expect("written");

        assert!(KnownHosts::load(&path).is_err());
    }

    #[test]
    fn fingerprints_are_in_the_form_openssh_prints() {
        // So a user can compare it with `ssh-keyscan` output by eye.
        let printed = fingerprint(KEY_A);
        assert!(printed.starts_with("SHA256:"));
        assert!(!printed.ends_with('='), "base64 padding must be trimmed");
        assert_ne!(printed, fingerprint(KEY_B));
        assert_eq!(printed, fingerprint(KEY_A));
    }

    #[test]
    fn an_interrupted_write_leaves_no_stray_temporary_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("known_hosts.json");
        let store = KnownHosts::load(&path).expect("loads");
        store.verify("host", 22, KEY_A);

        assert!(path.exists());
        assert!(!path.with_extension("tmp").exists());
    }
}
