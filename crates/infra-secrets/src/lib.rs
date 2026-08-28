//! # `vds-infra-secrets` — credential storage
//!
//! Implements [`SecretStore`]. The OS keystore is used wherever one is reachable
//! (Windows Credential Manager, macOS Keychain, Linux Secret Service); where none is —
//! a headless server, a container, a CI runner — an encrypted file is used instead, and
//! the application says so plainly rather than letting the user assume otherwise.
//!
//! See `docs/SECURITY.md`.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod encrypted_file;
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
mod keyring_store;

pub use encrypted_file::EncryptedFileStore;
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
pub use keyring_store::OsKeyringStore;

use async_trait::async_trait;
use std::sync::Arc;
use vds_domain::ids::CredentialRef;
use vds_domain::ports::{Secret, SecretKind, SecretStore, SecretStoreError};

/// Which backend a [`ResolvedSecretStore`] ended up using.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretBackend {
    /// The platform keystore.
    OsKeyring(String),
    /// An encrypted file, because no keystore was reachable.
    EncryptedFile { path: String, reason: String },
}

impl SecretBackend {
    /// Whether the platform keystore is in use.
    pub fn is_os_keyring(&self) -> bool {
        matches!(self, SecretBackend::OsKeyring(_))
    }

    /// A description for the settings screen.
    pub fn describe(&self) -> String {
        match self {
            SecretBackend::OsKeyring(name) => name.clone(),
            SecretBackend::EncryptedFile { path, reason } => {
                format!("encrypted file at {path} (no system keystore: {reason})")
            }
        }
    }
}

/// A secret store plus a record of which backend it chose.
///
/// The application shows [`SecretBackend::describe`] in Settings so the user always
/// knows where their credentials actually live. Silently degrading to a weaker store
/// would be the kind of security surprise this project exists to avoid.
pub struct ResolvedSecretStore {
    inner: Arc<dyn SecretStore>,
    backend: SecretBackend,
}

impl ResolvedSecretStore {
    pub fn new(inner: Arc<dyn SecretStore>, backend: SecretBackend) -> Self {
        Self { inner, backend }
    }

    pub fn backend(&self) -> &SecretBackend {
        &self.backend
    }

    pub fn inner(&self) -> Arc<dyn SecretStore> {
        Arc::clone(&self.inner)
    }
}

#[async_trait]
impl SecretStore for ResolvedSecretStore {
    async fn store(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
        secret: Secret,
    ) -> Result<(), SecretStoreError> {
        self.inner.store(reference, kind, secret).await
    }

    async fn retrieve(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<Secret, SecretStoreError> {
        self.inner.retrieve(reference, kind).await
    }

    async fn delete(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<(), SecretStoreError> {
        self.inner.delete(reference, kind).await
    }

    async fn contains(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<bool, SecretStoreError> {
        self.inner.contains(reference, kind).await
    }

    async fn delete_all(&self, reference: CredentialRef) -> Result<(), SecretStoreError> {
        self.inner.delete_all(reference).await
    }

    fn backend_description(&self) -> String {
        self.backend.describe()
    }
}

/// A [`SecretStore`] wrapper around [`EncryptedFileStore`].
pub struct FileSecretStore {
    store: EncryptedFileStore,
}

impl FileSecretStore {
    pub fn open(
        path: impl AsRef<std::path::Path>,
        passphrase: &str,
    ) -> Result<Self, SecretStoreError> {
        Ok(Self {
            store: EncryptedFileStore::open(path, passphrase)?,
        })
    }

    pub fn inner(&self) -> &EncryptedFileStore {
        &self.store
    }
}

#[async_trait]
impl SecretStore for FileSecretStore {
    async fn store(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
        secret: Secret,
    ) -> Result<(), SecretStoreError> {
        self.store.put(reference, kind, &secret)
    }

    async fn retrieve(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<Secret, SecretStoreError> {
        self.store.get(reference, kind)
    }

    async fn delete(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<(), SecretStoreError> {
        self.store.remove(reference, kind)
    }

    async fn contains(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<bool, SecretStoreError> {
        Ok(self.store.contains(reference, kind))
    }

    async fn delete_all(&self, reference: CredentialRef) -> Result<(), SecretStoreError> {
        self.store.remove_all(reference)
    }

    fn backend_description(&self) -> String {
        format!("encrypted file at {}", self.store.path().display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_file_store_satisfies_the_secret_store_port() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FileSecretStore::open(dir.path().join("v.vault"), "passphrase").expect("opens");
        let reference = CredentialRef::new();

        assert!(
            !store
                .contains(reference, SecretKind::SshPassword)
                .await
                .expect("checked")
        );

        store
            .store(
                reference,
                SecretKind::SshPassword,
                Secret::from_string("hunter2".into()),
            )
            .await
            .expect("stored");

        assert!(
            store
                .contains(reference, SecretKind::SshPassword)
                .await
                .expect("checked")
        );
        assert_eq!(
            store
                .retrieve(reference, SecretKind::SshPassword)
                .await
                .expect("read")
                .expose(),
            b"hunter2"
        );

        store
            .delete(reference, SecretKind::SshPassword)
            .await
            .expect("deleted");
        assert!(
            !store
                .contains(reference, SecretKind::SshPassword)
                .await
                .expect("checked")
        );
    }

    #[tokio::test]
    async fn the_backend_is_always_described_to_the_user() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FileSecretStore::open(dir.path().join("v.vault"), "passphrase").expect("opens");
        assert!(store.backend_description().contains("encrypted file"));
    }

    #[test]
    fn a_fallback_backend_explains_why_it_was_chosen() {
        // The user must never have to guess whether their credentials are in the system
        // keychain.
        let backend = SecretBackend::EncryptedFile {
            path: "/home/u/.local/share/vds-admin/secrets.vault".into(),
            reason: "the Secret Service is not running".into(),
        };
        assert!(!backend.is_os_keyring());
        let description = backend.describe();
        assert!(description.contains("encrypted file"));
        assert!(description.contains("Secret Service is not running"));
    }

    #[test]
    fn a_keyring_backend_names_the_platform_store() {
        let backend = SecretBackend::OsKeyring("Windows Credential Manager".into());
        assert!(backend.is_os_keyring());
        assert_eq!(backend.describe(), "Windows Credential Manager");
    }

    #[tokio::test]
    async fn the_resolved_store_reports_its_backend_not_the_inner_ones() {
        let dir = tempfile::tempdir().expect("temp dir");
        let inner = Arc::new(
            FileSecretStore::open(dir.path().join("v.vault"), "passphrase").expect("opens"),
        );
        let resolved = ResolvedSecretStore::new(
            inner,
            SecretBackend::EncryptedFile {
                path: "v.vault".into(),
                reason: "headless host".into(),
            },
        );

        assert!(resolved.backend_description().contains("headless host"));

        // And it still behaves like a store.
        let reference = CredentialRef::new();
        resolved
            .store(
                reference,
                SecretKind::AgentToken,
                Secret::from_string("t".into()),
            )
            .await
            .expect("stored");
        assert_eq!(
            resolved
                .retrieve(reference, SecretKind::AgentToken)
                .await
                .expect("read")
                .expose(),
            b"t"
        );
    }

    #[tokio::test]
    async fn deleting_a_handle_clears_every_kind_under_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FileSecretStore::open(dir.path().join("v.vault"), "passphrase").expect("opens");
        let reference = CredentialRef::new();

        for kind in [SecretKind::SshPrivateKey, SecretKind::SshKeyPassphrase] {
            store
                .store(reference, kind, Secret::from_string("x".into()))
                .await
                .expect("stored");
        }

        store.delete_all(reference).await.expect("deleted");

        for kind in [SecretKind::SshPrivateKey, SecretKind::SshKeyPassphrase] {
            assert!(!store.contains(reference, kind).await.expect("checked"));
        }
    }
}
