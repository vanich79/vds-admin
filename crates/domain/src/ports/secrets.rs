//! The credential port.
//!
//! Secret *material* exists only behind this trait. The rest of the domain carries
//! [`crate::ids::CredentialRef`] handles, so no entity, no database row and no log line
//! can accidentally contain a password.

use crate::ids::CredentialRef;
use async_trait::async_trait;
use std::fmt;
use zeroize::Zeroize;

/// What a stored secret is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKind {
    SshPassword,
    SshPrivateKey,
    SshKeyPassphrase,
    /// Bearer token for a monitoring agent.
    AgentToken,
    /// OAuth token or API key for an analytics provider.
    AnalyticsToken,
}

impl SecretKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SecretKind::SshPassword => "ssh_password",
            SecretKind::SshPrivateKey => "ssh_private_key",
            SecretKind::SshKeyPassphrase => "ssh_key_passphrase",
            SecretKind::AgentToken => "agent_token",
            SecretKind::AnalyticsToken => "analytics_token",
        }
    }

    pub fn parse(raw: &str) -> Option<SecretKind> {
        match raw {
            "ssh_password" => Some(SecretKind::SshPassword),
            "ssh_private_key" => Some(SecretKind::SshPrivateKey),
            "ssh_key_passphrase" => Some(SecretKind::SshKeyPassphrase),
            "agent_token" => Some(SecretKind::AgentToken),
            "analytics_token" => Some(SecretKind::AnalyticsToken),
            _ => None,
        }
    }
}

/// Secret material.
///
/// Three properties matter, and each is enforced by the type rather than by discipline:
///
/// * `Debug` prints `<redacted>`, so a secret cannot reach a log through a derived
///   `Debug` on some enclosing struct;
/// * there is no `Serialize`, so it cannot be written to the database or a JSON payload;
/// * the buffer is zeroed on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret {
    value: Vec<u8>,
}

impl Secret {
    pub fn new(value: impl Into<Vec<u8>>) -> Self {
        Self {
            value: value.into(),
        }
    }

    pub fn from_string(value: String) -> Self {
        Self {
            value: value.into_bytes(),
        }
    }

    /// Borrows the raw bytes. Callers must not copy them into anything long-lived.
    pub fn expose(&self) -> &[u8] {
        &self.value
    }

    /// Interprets the secret as UTF-8, for passwords and tokens.
    pub fn expose_str(&self) -> Result<&str, SecretStoreError> {
        std::str::from_utf8(&self.value)
            .map_err(|_| SecretStoreError::Corrupt("secret is not valid UTF-8".into()))
    }

    pub fn len(&self) -> usize {
        self.value.len()
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Length is deliberately omitted too: it leaks information about passwords.
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

/// Why a secret operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretStoreError {
    #[error("no secret stored for {0}")]
    NotFound(String),
    #[error("the platform keystore is unavailable: {0}")]
    BackendUnavailable(String),
    #[error("access to the keystore was denied: {0}")]
    AccessDenied(String),
    #[error("stored secret is corrupt: {0}")]
    Corrupt(String),
    #[error("keystore operation failed: {0}")]
    Backend(String),
    #[error("the secret store is locked")]
    Locked,
}

/// Stores and retrieves secret material.
///
/// Implementations: the OS keystore (Windows Credential Manager, macOS Keychain, Linux
/// Secret Service, Android Keystore) and an encrypted-file fallback for hosts without
/// one.
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Stores a secret and returns nothing — the caller already holds the handle.
    async fn store(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
        secret: Secret,
    ) -> Result<(), SecretStoreError>;

    async fn retrieve(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<Secret, SecretStoreError>;

    async fn delete(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<(), SecretStoreError>;

    /// Whether a secret exists, without retrieving it.
    async fn contains(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<bool, SecretStoreError>;

    /// Removes every secret associated with a handle, e.g. when a server is deleted.
    async fn delete_all(&self, reference: CredentialRef) -> Result<(), SecretStoreError>;

    /// Human-readable description of where secrets are actually being kept, shown in
    /// settings so the user knows whether the OS keystore or the fallback is in use.
    fn backend_description(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_secret() {
        let secret = Secret::from_string("hunter2".to_owned());
        let rendered = format!("{secret:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug leaked the secret: {rendered}"
        );
        assert_eq!(rendered, "Secret(<redacted>)");
    }

    #[test]
    fn display_output_never_contains_the_secret() {
        let secret = Secret::from_string("hunter2".to_owned());
        assert_eq!(format!("{secret}"), "<redacted>");
    }

    #[test]
    fn debug_does_not_leak_the_length_either() {
        let short = Secret::from_string("a".to_owned());
        let long = Secret::from_string("a".repeat(64));
        assert_eq!(format!("{short:?}"), format!("{long:?}"));
    }

    #[test]
    fn a_secret_nested_in_a_derived_debug_struct_stays_redacted() {
        // This is the failure mode the manual Debug impl exists to prevent.
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Credentials {
            username: String,
            password: Secret,
        }

        let creds = Credentials {
            username: "root".into(),
            password: Secret::from_string("hunter2".into()),
        };
        let rendered = format!("{creds:?}");
        assert!(rendered.contains("root"));
        assert!(
            !rendered.contains("hunter2"),
            "nested Debug leaked: {rendered}"
        );
    }

    #[test]
    fn secrets_expose_their_bytes_when_explicitly_asked() {
        let secret = Secret::from_string("hunter2".to_owned());
        assert_eq!(secret.expose(), b"hunter2");
        assert_eq!(secret.expose_str(), Ok("hunter2"));
        assert_eq!(secret.len(), 7);
    }

    #[test]
    fn non_utf8_secrets_report_an_error_rather_than_panicking() {
        let secret = Secret::new(vec![0xff, 0xfe]);
        assert!(secret.expose_str().is_err());
    }

    #[test]
    fn secret_kinds_round_trip() {
        for kind in [
            SecretKind::SshPassword,
            SecretKind::SshPrivateKey,
            SecretKind::SshKeyPassphrase,
            SecretKind::AgentToken,
            SecretKind::AnalyticsToken,
        ] {
            assert_eq!(SecretKind::parse(kind.as_str()), Some(kind));
        }
    }
}
