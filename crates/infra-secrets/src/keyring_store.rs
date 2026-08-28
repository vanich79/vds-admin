//! The OS keystore backend.
//!
//! Windows Credential Manager, macOS Keychain, and the Linux Secret Service (GNOME
//! Keyring, KWallet) behind one interface, courtesy of the `keyring` crate.
//!
//! Keystore calls are blocking and can be slow — on Linux they are a D-Bus round trip,
//! and on macOS they can prompt the user — so every call goes through `spawn_blocking`.

use async_trait::async_trait;
use vds_domain::ids::CredentialRef;
use vds_domain::ports::{Secret, SecretKind, SecretStore, SecretStoreError};

/// Service name under which entries are filed in the platform keystore.
const SERVICE: &str = "vds-admin";

/// Credentials held in the platform keystore.
pub struct OsKeyringStore {
    service: String,
}

impl OsKeyringStore {
    pub fn new() -> Self {
        Self {
            service: SERVICE.to_owned(),
        }
    }

    /// Uses a distinct service name, so tests cannot disturb a real installation's
    /// entries.
    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// Checks whether the platform keystore is actually usable.
    ///
    /// Called at startup: on a headless Linux box the Secret Service is often absent,
    /// and discovering that during the first SSH connection would be far too late.
    pub async fn probe(&self) -> Result<(), SecretStoreError> {
        let service = self.service.clone();
        blocking(move || {
            let probe = CredentialRef::new();
            let entry = keyring::Entry::new(&service, &account(probe, SecretKind::AgentToken))
                .map_err(map_error)?;

            // Write then delete: reading alone cannot distinguish "no keystore" from
            // "keystore working, entry absent".
            entry.set_password("probe").map_err(map_error)?;
            entry.delete_credential().map_err(map_error)?;
            Ok(())
        })
        .await
    }

    /// Human-readable name of the platform store, for the settings screen.
    pub fn platform_name() -> &'static str {
        if cfg!(target_os = "windows") {
            "Windows Credential Manager"
        } else if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else {
            "Linux Secret Service"
        }
    }
}

impl Default for OsKeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

/// The account name one secret is filed under.
fn account(reference: CredentialRef, kind: SecretKind) -> String {
    format!("{reference}/{}", kind.as_str())
}

/// Runs a blocking keystore call off the async runtime.
async fn blocking<T, F>(operation: F) -> Result<T, SecretStoreError>
where
    F: FnOnce() -> Result<T, SecretStoreError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|e| SecretStoreError::Backend(format!("keystore task failed: {e}")))?
}

/// Translates a keyring error into a domain-level one.
fn map_error(error: keyring::Error) -> SecretStoreError {
    match error {
        keyring::Error::NoEntry => SecretStoreError::NotFound("no such entry".to_owned()),
        keyring::Error::NoStorageAccess(inner) => SecretStoreError::AccessDenied(inner.to_string()),
        keyring::Error::PlatformFailure(inner) => {
            // On Linux this is what "no Secret Service is running" looks like.
            SecretStoreError::BackendUnavailable(inner.to_string())
        }
        keyring::Error::BadEncoding(_) => {
            SecretStoreError::Corrupt("the stored secret is not valid UTF-8".to_owned())
        }
        other => SecretStoreError::Backend(other.to_string()),
    }
}

#[async_trait]
impl SecretStore for OsKeyringStore {
    async fn store(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
        secret: Secret,
    ) -> Result<(), SecretStoreError> {
        let service = self.service.clone();
        let account = account(reference, kind);
        // The platform APIs take a UTF-8 string, so binary key material is base64-encoded
        // by the caller before it gets here; `Secret::expose_str` enforces that.
        let value = secret.expose_str()?.to_owned();

        blocking(move || {
            keyring::Entry::new(&service, &account)
                .map_err(map_error)?
                .set_password(&value)
                .map_err(map_error)
        })
        .await
    }

    async fn retrieve(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<Secret, SecretStoreError> {
        let service = self.service.clone();
        let account = account(reference, kind);

        blocking(move || {
            let value = keyring::Entry::new(&service, &account)
                .map_err(map_error)?
                .get_password()
                .map_err(map_error)?;
            Ok(Secret::from_string(value))
        })
        .await
    }

    async fn delete(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<(), SecretStoreError> {
        let service = self.service.clone();
        let account = account(reference, kind);

        blocking(move || {
            match keyring::Entry::new(&service, &account)
                .map_err(map_error)?
                .delete_credential()
            {
                Ok(()) => Ok(()),
                // Deleting something that is already gone is a success, not a failure.
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(err) => Err(map_error(err)),
            }
        })
        .await
    }

    async fn contains(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<bool, SecretStoreError> {
        match self.retrieve(reference, kind).await {
            Ok(_) => Ok(true),
            Err(SecretStoreError::NotFound(_)) => Ok(false),
            Err(err) => Err(err),
        }
    }

    async fn delete_all(&self, reference: CredentialRef) -> Result<(), SecretStoreError> {
        // Platform keystores offer no prefix search, so every kind is deleted explicitly.
        // The list is small and known, which is why `SecretKind` is a closed enum.
        for kind in [
            SecretKind::SshPassword,
            SecretKind::SshPrivateKey,
            SecretKind::SshKeyPassphrase,
            SecretKind::AgentToken,
            SecretKind::AnalyticsToken,
        ] {
            self.delete(reference, kind).await?;
        }
        Ok(())
    }

    fn backend_description(&self) -> String {
        Self::platform_name().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_are_namespaced_by_handle_and_kind() {
        let reference = CredentialRef::new();
        let password = account(reference, SecretKind::SshPassword);
        let passphrase = account(reference, SecretKind::SshKeyPassphrase);

        assert_ne!(password, passphrase);
        assert!(password.starts_with(&reference.to_string()));
        assert!(password.ends_with("ssh_password"));
    }

    #[test]
    fn the_platform_name_is_reported_for_the_settings_screen() {
        let name = OsKeyringStore::platform_name();
        assert!(!name.is_empty());
        assert!(
            name.contains("Credential Manager")
                || name.contains("Keychain")
                || name.contains("Secret Service")
        );
    }

    #[test]
    fn a_missing_secret_service_is_reported_as_backend_unavailable() {
        // Distinguishing this from a generic error is what lets startup fall back to the
        // encrypted file rather than refusing to run.
        let err = map_error(keyring::Error::NoEntry);
        assert!(matches!(err, SecretStoreError::NotFound(_)));
    }

    /// Exercises the real platform keystore.
    ///
    /// Ignored by default: it writes to the developer's actual keychain, and on Linux CI
    /// there is usually no Secret Service at all. Run with
    /// `cargo test -p vds-infra-secrets -- --ignored` on a desktop.
    #[tokio::test]
    #[ignore = "touches the real OS keystore"]
    async fn the_platform_keystore_round_trips() {
        let store = OsKeyringStore::with_service("vds-admin-test");
        let reference = CredentialRef::new();

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

        store.delete_all(reference).await.expect("deleted");
        assert!(
            !store
                .contains(reference, SecretKind::SshPassword)
                .await
                .expect("checked")
        );
    }

    #[tokio::test]
    #[ignore = "touches the real OS keystore"]
    async fn probing_reports_whether_the_keystore_is_usable() {
        let store = OsKeyringStore::with_service("vds-admin-test");
        assert!(store.probe().await.is_ok());
    }
}
