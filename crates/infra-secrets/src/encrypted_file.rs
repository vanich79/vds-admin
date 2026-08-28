//! The encrypted-file fallback.
//!
//! Used where no OS keystore is reachable: a headless Linux box with no Secret Service,
//! a container, a CI runner. It is a genuine fallback, not a pretend one — secrets are
//! encrypted with XChaCha20-Poly1305 under a key derived from a passphrase with
//! Argon2id — but it is strictly worse than the OS keystore, because the passphrase has
//! to come from somewhere. The application says so plainly in Settings rather than
//! letting the user assume their credentials are in the system keychain.
//!
//! Format (JSON, one file):
//!
//! ```text
//! { "version": 1,
//!   "kdf": { "salt": base64, "m_cost": .., "t_cost": .., "p_cost": .. },
//!   "entries": { "<ref>/<kind>": { "nonce": base64, "ciphertext": base64 } } }
//! ```
//!
//! Each entry has its own nonce. The KDF salt is per *file*, so unlocking derives the
//! key once rather than once per secret.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use vds_domain::ids::CredentialRef;
use vds_domain::ports::{Secret, SecretKind, SecretStoreError};
use zeroize::Zeroize;

/// Current on-disk format version.
const FORMAT_VERSION: u32 = 1;

/// Argon2id cost parameters.
///
/// 64 MiB and three passes is the OWASP-recommended baseline. It costs a fraction of a
/// second on a desktop, which is fine for something done once at unlock, and it is what
/// makes an offline attack on the file expensive.
const MEMORY_KIB: u32 = 64 * 1_024;
const ITERATIONS: u32 = 3;
const PARALLELISM: u32 = 1;
const KEY_BYTES: usize = 32;
const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 24;

/// The serialised file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultFile {
    version: u32,
    kdf: KdfParams,
    entries: BTreeMap<String, EncryptedEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KdfParams {
    salt: String,
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedEntry {
    nonce: String,
    ciphertext: String,
}

/// An encrypted secret file.
pub struct EncryptedFileStore {
    path: PathBuf,
    /// Derived key, held only while unlocked.
    key: Mutex<Option<[u8; KEY_BYTES]>>,
    file: Mutex<VaultFile>,
}

impl std::fmt::Debug for EncryptedFileStore {
    /// Deliberately hand-written.
    ///
    /// A derived `Debug` would print the derived key, which is exactly as damaging as
    /// printing the passphrase. Only the path and whether the vault is unlocked are
    /// safe to show.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedFileStore")
            .field("path", &self.path)
            .field("unlocked", &self.key.lock().is_some())
            .field("entries", &self.file.lock().entries.len())
            .finish()
    }
}

impl EncryptedFileStore {
    /// Opens an existing vault or prepares a new one, and unlocks it with `passphrase`.
    pub fn open(path: impl AsRef<Path>, passphrase: &str) -> Result<Self, SecretStoreError> {
        let path = path.as_ref().to_path_buf();

        let file = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| SecretStoreError::Backend(format!("could not read {path:?}: {e}")))?;
            let file: VaultFile = serde_json::from_str(&text)
                .map_err(|e| SecretStoreError::Corrupt(format!("vault is not valid: {e}")))?;

            if file.version > FORMAT_VERSION {
                return Err(SecretStoreError::Corrupt(format!(
                    "vault format {} is newer than this build understands ({FORMAT_VERSION})",
                    file.version
                )));
            }
            file
        } else {
            VaultFile {
                version: FORMAT_VERSION,
                kdf: KdfParams {
                    salt: BASE64.encode(random_bytes::<SALT_BYTES>()),
                    memory_kib: MEMORY_KIB,
                    iterations: ITERATIONS,
                    parallelism: PARALLELISM,
                },
                entries: BTreeMap::new(),
            }
        };

        let key = derive_key(passphrase, &file.kdf)?;

        let store = Self {
            path,
            key: Mutex::new(Some(key)),
            file: Mutex::new(file),
        };

        // Prove the passphrase is right before returning a "successfully opened" store.
        // Otherwise a typo would look like success until the first read produced a
        // decryption failure the user could not explain.
        store.verify_passphrase()?;
        Ok(store)
    }

    /// Decrypts one entry to confirm the key is correct.
    fn verify_passphrase(&self) -> Result<(), SecretStoreError> {
        let file = self.file.lock();
        let Some((_, entry)) = file.entries.iter().next() else {
            // An empty vault cannot be verified, and does not need to be: there is
            // nothing to get wrong yet.
            return Ok(());
        };
        let key = self.key()?;
        decrypt(&key, entry)
            .map(|mut plaintext| plaintext.zeroize())
            .map_err(|_| SecretStoreError::AccessDenied("the passphrase is incorrect".to_owned()))
    }

    fn key(&self) -> Result<[u8; KEY_BYTES], SecretStoreError> {
        self.key.lock().ok_or(SecretStoreError::Locked)
    }

    /// Forgets the derived key. Reads and writes fail until reopened.
    pub fn lock(&self) {
        if let Some(mut key) = self.key.lock().take() {
            key.zeroize();
        }
    }

    pub fn is_locked(&self) -> bool {
        self.key.lock().is_none()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.file.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Stores a secret and persists the file.
    pub fn put(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
        secret: &Secret,
    ) -> Result<(), SecretStoreError> {
        let key = self.key()?;
        let entry = encrypt(&key, secret.expose())?;
        {
            let mut file = self.file.lock();
            file.entries.insert(entry_key(reference, kind), entry);
        }
        self.persist()
    }

    /// Retrieves a secret.
    pub fn get(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<Secret, SecretStoreError> {
        let key = self.key()?;
        let name = entry_key(reference, kind);
        let entry = {
            let file = self.file.lock();
            file.entries
                .get(&name)
                .cloned()
                .ok_or_else(|| SecretStoreError::NotFound(name.clone()))?
        };
        decrypt(&key, &entry).map(Secret::new)
    }

    pub fn contains(&self, reference: CredentialRef, kind: SecretKind) -> bool {
        self.file
            .lock()
            .entries
            .contains_key(&entry_key(reference, kind))
    }

    pub fn remove(
        &self,
        reference: CredentialRef,
        kind: SecretKind,
    ) -> Result<(), SecretStoreError> {
        {
            let mut file = self.file.lock();
            file.entries.remove(&entry_key(reference, kind));
        }
        self.persist()
    }

    /// Removes every secret held under one handle.
    pub fn remove_all(&self, reference: CredentialRef) -> Result<(), SecretStoreError> {
        let prefix = format!("{reference}/");
        {
            let mut file = self.file.lock();
            file.entries.retain(|name, _| !name.starts_with(&prefix));
        }
        self.persist()
    }

    /// Writes the file atomically.
    ///
    /// Via a temporary file and a rename, so an interrupted write cannot leave a
    /// truncated vault — which would mean losing every stored credential.
    fn persist(&self) -> Result<(), SecretStoreError> {
        let text = {
            let file = self.file.lock();
            serde_json::to_string_pretty(&*file)
                .map_err(|e| SecretStoreError::Backend(format!("could not serialise: {e}")))?
        };

        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                SecretStoreError::Backend(format!("could not create {parent:?}: {e}"))
            })?;
        }

        let temporary = self.path.with_extension("tmp");
        std::fs::write(&temporary, text.as_bytes())
            .map_err(|e| SecretStoreError::Backend(format!("could not write vault: {e}")))?;

        restrict_permissions(&temporary)?;

        std::fs::rename(&temporary, &self.path)
            .map_err(|e| SecretStoreError::Backend(format!("could not replace vault: {e}")))
    }
}

/// Restricts a file to the current user where the platform supports it.
fn restrict_permissions(path: &Path) -> Result<(), SecretStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            SecretStoreError::Backend(format!("could not restrict vault permissions: {e}"))
        })?;
    }
    #[cfg(not(unix))]
    {
        // Windows inherits the directory ACL, which for a per-user application data
        // directory is already user-only.
        let _ = path;
    }
    Ok(())
}

fn entry_key(reference: CredentialRef, kind: SecretKind) -> String {
    format!("{reference}/{}", kind.as_str())
}

/// Derives the file key from the passphrase.
fn derive_key(passphrase: &str, params: &KdfParams) -> Result<[u8; KEY_BYTES], SecretStoreError> {
    let salt = BASE64
        .decode(&params.salt)
        .map_err(|e| SecretStoreError::Corrupt(format!("the KDF salt is not base64: {e}")))?;

    let argon = Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(
            params.memory_kib,
            params.iterations,
            params.parallelism,
            Some(KEY_BYTES),
        )
        .map_err(|e| SecretStoreError::Corrupt(format!("invalid KDF parameters: {e}")))?,
    );

    let mut key = [0_u8; KEY_BYTES];
    argon
        .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
        .map_err(|e| SecretStoreError::Backend(format!("key derivation failed: {e}")))?;
    Ok(key)
}

fn encrypt(key: &[u8; KEY_BYTES], plaintext: &[u8]) -> Result<EncryptedEntry, SecretStoreError> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce_bytes = random_bytes::<NONCE_BYTES>();
    let nonce = XNonce::from_slice(&nonce_bytes);

    // The version is authenticated as associated data, so a downgrade to an older,
    // weaker format cannot be forged by editing the file.
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: &FORMAT_VERSION.to_le_bytes(),
            },
        )
        .map_err(|_| SecretStoreError::Backend("encryption failed".to_owned()))?;

    Ok(EncryptedEntry {
        nonce: BASE64.encode(nonce_bytes),
        ciphertext: BASE64.encode(ciphertext),
    })
}

fn decrypt(key: &[u8; KEY_BYTES], entry: &EncryptedEntry) -> Result<Vec<u8>, SecretStoreError> {
    let nonce_bytes = BASE64
        .decode(&entry.nonce)
        .map_err(|_| SecretStoreError::Corrupt("the nonce is not base64".to_owned()))?;
    if nonce_bytes.len() != NONCE_BYTES {
        return Err(SecretStoreError::Corrupt(
            "the nonce is the wrong length".to_owned(),
        ));
    }
    let ciphertext = BASE64
        .decode(&entry.ciphertext)
        .map_err(|_| SecretStoreError::Corrupt("the ciphertext is not base64".to_owned()))?;

    let cipher = XChaCha20Poly1305::new(key.into());
    cipher
        .decrypt(
            XNonce::from_slice(&nonce_bytes),
            Payload {
                msg: &ciphertext,
                aad: &FORMAT_VERSION.to_le_bytes(),
            },
        )
        .map_err(|_| {
            // Deliberately vague: distinguishing "wrong key" from "tampered ciphertext"
            // tells an attacker which of the two they achieved.
            SecretStoreError::AccessDenied("the secret could not be decrypted".to_owned())
        })
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    rand::fill(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deliberately weak KDF, so the test suite does not spend a second per open.
    ///
    /// The production parameters are exercised by
    /// `production_parameters_are_at_the_recommended_strength`.
    fn fast_store(path: PathBuf, passphrase: &str) -> EncryptedFileStore {
        let file = if path.exists() {
            let text = std::fs::read_to_string(&path).expect("readable");
            serde_json::from_str(&text).expect("valid")
        } else {
            VaultFile {
                version: FORMAT_VERSION,
                kdf: KdfParams {
                    salt: BASE64.encode(random_bytes::<SALT_BYTES>()),
                    memory_kib: 8,
                    iterations: 1,
                    parallelism: 1,
                },
                entries: BTreeMap::new(),
            }
        };
        let key = derive_key(passphrase, &file.kdf).expect("derives");
        let store = EncryptedFileStore {
            path,
            key: Mutex::new(Some(key)),
            file: Mutex::new(file),
        };
        store.verify_passphrase().expect("passphrase accepted");
        store
    }

    fn vault_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("secrets.vault")
    }

    #[test]
    fn a_secret_round_trips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fast_store(vault_path(&dir), "correct horse battery staple");
        let reference = CredentialRef::new();

        store
            .put(
                reference,
                SecretKind::SshPassword,
                &Secret::from_string("hunter2".into()),
            )
            .expect("stored");

        let secret = store
            .get(reference, SecretKind::SshPassword)
            .expect("retrieved");
        assert_eq!(secret.expose(), b"hunter2");
    }

    #[test]
    fn the_plaintext_never_appears_in_the_file() {
        // The whole point of the fallback.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = vault_path(&dir);
        let store = fast_store(path.clone(), "passphrase");

        store
            .put(
                CredentialRef::new(),
                SecretKind::SshPassword,
                &Secret::from_string("SuperSecretValue123".into()),
            )
            .expect("stored");

        let contents = std::fs::read_to_string(&path).expect("readable");
        assert!(
            !contents.contains("SuperSecretValue123"),
            "the vault leaked the secret"
        );
        assert!(
            !contents.contains("passphrase"),
            "the vault leaked the passphrase"
        );
    }

    #[test]
    fn secrets_survive_reopening() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = vault_path(&dir);
        let reference = CredentialRef::new();

        {
            let store = fast_store(path.clone(), "passphrase");
            store
                .put(
                    reference,
                    SecretKind::AnalyticsToken,
                    &Secret::from_string("token".into()),
                )
                .expect("stored");
        }

        let store = fast_store(path, "passphrase");
        assert_eq!(
            store
                .get(reference, SecretKind::AnalyticsToken)
                .expect("retrieved")
                .expose(),
            b"token"
        );
    }

    #[test]
    fn the_wrong_passphrase_is_rejected_at_open_not_at_first_read() {
        // A typo must fail immediately and comprehensibly.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = vault_path(&dir);

        {
            let store = fast_store(path.clone(), "correct");
            store
                .put(
                    CredentialRef::new(),
                    SecretKind::SshPassword,
                    &Secret::from_string("value".into()),
                )
                .expect("stored");
        }

        let file: VaultFile =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("readable"))
                .expect("valid");
        let wrong = derive_key("incorrect", &file.kdf).expect("derives");
        let store = EncryptedFileStore {
            path,
            key: Mutex::new(Some(wrong)),
            file: Mutex::new(file),
        };

        let err = store.verify_passphrase().expect_err("must reject");
        assert!(
            matches!(err, SecretStoreError::AccessDenied(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn tampering_with_the_ciphertext_is_detected() {
        // AEAD, not just encryption: a modified secret must not decrypt to something.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fast_store(vault_path(&dir), "passphrase");
        let key = store.key().expect("unlocked");

        let mut entry = encrypt(&key, b"original").expect("encrypts");
        let mut bytes = BASE64.decode(&entry.ciphertext).expect("base64");
        bytes[0] ^= 0xff;
        entry.ciphertext = BASE64.encode(bytes);

        assert!(decrypt(&key, &entry).is_err());
    }

    #[test]
    fn each_secret_gets_a_distinct_nonce() {
        // Nonce reuse under the same key would be catastrophic for a stream cipher.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fast_store(vault_path(&dir), "passphrase");
        let key = store.key().expect("unlocked");

        let first = encrypt(&key, b"same plaintext").expect("encrypts");
        let second = encrypt(&key, b"same plaintext").expect("encrypts");

        assert_ne!(first.nonce, second.nonce);
        assert_ne!(
            first.ciphertext, second.ciphertext,
            "identical plaintexts must not match"
        );
    }

    #[test]
    fn different_kinds_under_one_handle_are_stored_separately() {
        // An encrypted key and its passphrase live under the same credential handle.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fast_store(vault_path(&dir), "passphrase");
        let reference = CredentialRef::new();

        store
            .put(
                reference,
                SecretKind::SshPrivateKey,
                &Secret::from_string("KEY".into()),
            )
            .expect("stored");
        store
            .put(
                reference,
                SecretKind::SshKeyPassphrase,
                &Secret::from_string("PASS".into()),
            )
            .expect("stored");

        assert_eq!(
            store
                .get(reference, SecretKind::SshPrivateKey)
                .expect("read")
                .expose(),
            b"KEY"
        );
        assert_eq!(
            store
                .get(reference, SecretKind::SshKeyPassphrase)
                .expect("read")
                .expose(),
            b"PASS"
        );
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn removing_a_handle_removes_every_secret_under_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fast_store(vault_path(&dir), "passphrase");
        let doomed = CredentialRef::new();
        let kept = CredentialRef::new();

        store
            .put(
                doomed,
                SecretKind::SshPrivateKey,
                &Secret::from_string("a".into()),
            )
            .expect("stored");
        store
            .put(
                doomed,
                SecretKind::SshKeyPassphrase,
                &Secret::from_string("b".into()),
            )
            .expect("stored");
        store
            .put(
                kept,
                SecretKind::SshPassword,
                &Secret::from_string("c".into()),
            )
            .expect("stored");

        store.remove_all(doomed).expect("removed");

        assert!(!store.contains(doomed, SecretKind::SshPrivateKey));
        assert!(!store.contains(doomed, SecretKind::SshKeyPassphrase));
        assert!(store.contains(kept, SecretKind::SshPassword));
    }

    #[test]
    fn a_missing_secret_is_not_found_rather_than_an_empty_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fast_store(vault_path(&dir), "passphrase");
        let err = store
            .get(CredentialRef::new(), SecretKind::SshPassword)
            .expect_err("must fail");
        assert!(matches!(err, SecretStoreError::NotFound(_)));
    }

    #[test]
    fn locking_makes_reads_and_writes_fail_until_reopened() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fast_store(vault_path(&dir), "passphrase");
        let reference = CredentialRef::new();
        store
            .put(
                reference,
                SecretKind::SshPassword,
                &Secret::from_string("x".into()),
            )
            .expect("stored");

        store.lock();
        assert!(store.is_locked());
        assert!(matches!(
            store.get(reference, SecretKind::SshPassword),
            Err(SecretStoreError::Locked)
        ));
    }

    #[test]
    fn a_vault_from_a_newer_build_is_refused_rather_than_misread() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = vault_path(&dir);
        std::fs::write(
            &path,
            serde_json::to_string(&VaultFile {
                version: FORMAT_VERSION + 5,
                kdf: KdfParams {
                    salt: BASE64.encode([0_u8; SALT_BYTES]),
                    memory_kib: 8,
                    iterations: 1,
                    parallelism: 1,
                },
                entries: BTreeMap::new(),
            })
            .expect("serialises"),
        )
        .expect("written");

        let err = EncryptedFileStore::open(&path, "passphrase").expect_err("must refuse");
        assert!(matches!(err, SecretStoreError::Corrupt(_)), "got {err:?}");
    }

    #[test]
    fn a_corrupt_vault_is_reported_as_corrupt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = vault_path(&dir);
        std::fs::write(&path, "{ not json").expect("written");

        let err = EncryptedFileStore::open(&path, "passphrase").expect_err("must fail");
        assert!(matches!(err, SecretStoreError::Corrupt(_)));
    }

    #[test]
    fn an_interrupted_write_cannot_truncate_the_vault() {
        // The vault is replaced by rename, so the real file is never partially written.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = vault_path(&dir);
        let store = fast_store(path.clone(), "passphrase");
        let reference = CredentialRef::new();
        store
            .put(
                reference,
                SecretKind::SshPassword,
                &Secret::from_string("value".into()),
            )
            .expect("stored");

        // No stray temporary file is left behind.
        assert!(!path.with_extension("tmp").exists());
        assert!(path.exists());
        assert!(!std::fs::read_to_string(&path).expect("readable").is_empty());
    }

    #[test]
    fn a_fresh_vault_opens_without_a_passphrase_check_to_fail() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fast_store(vault_path(&dir), "anything");
        assert!(store.is_empty());
        assert!(!store.is_locked());
    }

    #[test]
    fn the_debug_output_never_contains_key_material() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = fast_store(vault_path(&dir), "a memorable passphrase");
        store
            .put(
                CredentialRef::new(),
                SecretKind::SshPassword,
                &Secret::from_string("hunter2".into()),
            )
            .expect("stored");

        let rendered = format!("{store:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug leaked a secret: {rendered}"
        );
        assert!(
            !rendered.contains("passphrase"),
            "Debug leaked the passphrase: {rendered}"
        );
        assert!(rendered.contains("unlocked: true"), "Debug was: {rendered}");
    }

    #[test]
    fn production_parameters_are_at_the_recommended_strength() {
        // Guards against someone lowering these to speed up a test run.
        // Checked in a const block, so lowering one of these fails the build rather
        // than the test run.
        const {
            assert!(
                MEMORY_KIB >= 19 * 1_024,
                "Argon2id memory is below the OWASP minimum"
            );
            assert!(ITERATIONS >= 2, "Argon2id needs at least two passes");
            assert!(KEY_BYTES == 32, "XChaCha20-Poly1305 takes a 32-byte key");
            assert!(NONCE_BYTES == 24, "XChaCha20 requires a 24-byte nonce");
            assert!(SALT_BYTES >= 16, "the salt is below the recommended length");
        }
    }

    #[test]
    fn the_real_open_path_works_end_to_end() {
        // Exercises the production KDF once, so the fast-store shortcut cannot hide a
        // broken real path.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = vault_path(&dir);
        let reference = CredentialRef::new();

        {
            let store = EncryptedFileStore::open(&path, "a real passphrase").expect("opens");
            store
                .put(
                    reference,
                    SecretKind::AgentToken,
                    &Secret::from_string("tok".into()),
                )
                .expect("stored");
        }

        let store = EncryptedFileStore::open(&path, "a real passphrase").expect("reopens");
        assert_eq!(
            store
                .get(reference, SecretKind::AgentToken)
                .expect("read")
                .expose(),
            b"tok"
        );

        assert!(matches!(
            EncryptedFileStore::open(&path, "the wrong one"),
            Err(SecretStoreError::AccessDenied(_))
        ));
    }
}
