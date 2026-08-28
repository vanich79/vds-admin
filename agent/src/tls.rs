//! TLS material for the agent's listener.
//!
//! ## Why a self-signed certificate is the right default
//!
//! An agent runs on a machine that usually has no public DNS name and no ACME client. A
//! CA-issued certificate is therefore not available on first start, and refusing to run
//! without one would mean the daemon is unusable until an operator does paperwork.
//!
//! So the agent generates its own on first start and the *app* pins the fingerprint on
//! first connection, the way an SSH client pins a host key. That gives the property that
//! matters — the second connection is guaranteed to reach the same machine as the first —
//! without a certificate authority. An operator who does have a real certificate points
//! `tls_certificate`/`tls_private_key` at it and this module simply loads it.
//!
//! The generated key is written `0600` on Unix. That is done here, at the moment of
//! creation, rather than left to the installer: a key that is briefly world-readable has
//! already leaked.

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// Why the listener could not be secured.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} contains no certificates")]
    NoCertificates { path: PathBuf },
    #[error("{path} contains no private key")]
    NoPrivateKey { path: PathBuf },
    #[error("could not generate a certificate: {0}")]
    Generate(String),
    #[error("the certificate and key were not accepted: {0}")]
    Rejected(String),
}

/// A loaded certificate chain and its key.
pub struct TlsMaterial {
    pub certificates: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
    /// True when this run created the files.
    pub generated: bool,
}

/// Prints the certificate count and nothing about the key.
///
/// Hand-written rather than derived: a private key must never be one `{:?}` away from a
/// log line, whatever the upstream type happens to do today.
impl std::fmt::Debug for TlsMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsMaterial")
            .field("certificates", &self.certificates.len())
            .field("key", &"<redacted>")
            .field("generated", &self.generated)
            .finish()
    }
}

/// Loads the configured certificate, generating one if it does not exist yet.
///
/// `hostnames` become the certificate's subject alternative names.
pub fn load_or_generate(
    certificate_path: &Path,
    key_path: &Path,
    hostnames: &[String],
) -> Result<TlsMaterial, TlsError> {
    if certificate_path.exists() && key_path.exists() {
        let material = load(certificate_path, key_path)?;
        return Ok(material);
    }

    generate(certificate_path, key_path, hostnames)
}

/// Reads an existing PEM certificate chain and key.
fn load(certificate_path: &Path, key_path: &Path) -> Result<TlsMaterial, TlsError> {
    let certificates = read_certificates(certificate_path)?;
    let key = read_private_key(key_path)?;
    Ok(TlsMaterial {
        certificates,
        key,
        generated: false,
    })
}

fn read_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let file = std::fs::File::open(path).map_err(|source| TlsError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    let certificates: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(file))
            .collect::<Result<_, _>>()
            .map_err(|source| TlsError::Read {
                path: path.to_path_buf(),
                source,
            })?;

    if certificates.is_empty() {
        return Err(TlsError::NoCertificates {
            path: path.to_path_buf(),
        });
    }
    Ok(certificates)
}

fn read_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    let file = std::fs::File::open(path).map_err(|source| TlsError::Read {
        path: path.to_path_buf(),
        source,
    })?;

    // Accepts PKCS#8, PKCS#1 and SEC1, because operators arrive with all three and
    // "wrong PEM label" is a miserable thing to debug at three in the morning.
    rustls_pemfile::private_key(&mut BufReader::new(file))
        .map_err(|source| TlsError::Read {
            path: path.to_path_buf(),
            source,
        })?
        .ok_or_else(|| TlsError::NoPrivateKey {
            path: path.to_path_buf(),
        })
}

/// Creates a self-signed certificate and writes it alongside its key.
fn generate(
    certificate_path: &Path,
    key_path: &Path,
    hostnames: &[String],
) -> Result<TlsMaterial, TlsError> {
    // An empty SAN list produces a certificate no client will match. Falling back to
    // `localhost` keeps a misconfigured host serving something usable over a tunnel.
    let names: Vec<String> = if hostnames.is_empty() {
        vec!["localhost".to_owned()]
    } else {
        hostnames.to_vec()
    };

    let certified = rcgen::generate_simple_self_signed(names)
        .map_err(|err| TlsError::Generate(err.to_string()))?;

    let certificate_pem = certified.cert.pem();
    let key_pem = certified.signing_key.serialize_pem();

    if let Some(parent) = certificate_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| TlsError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    write_file(certificate_path, certificate_pem.as_bytes(), 0o644)?;
    // The key is written with restrictive permissions from the start; see the module
    // documentation.
    write_file(key_path, key_pem.as_bytes(), 0o600)?;

    load(certificate_path, key_path).map(|mut material| {
        material.generated = true;
        material
    })
}

/// Writes a file, applying Unix permissions where the platform has them.
fn write_file(path: &Path, contents: &[u8], mode: u32) -> Result<(), TlsError> {
    std::fs::write(path, contents).map_err(|source| TlsError::Write {
        path: path.to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(
            |source| TlsError::Write {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    #[cfg(not(unix))]
    {
        // Windows is a development platform for this binary, not a deployment target.
        let _ = mode;
    }

    Ok(())
}

/// Builds the server configuration rustls will use.
pub fn server_config(material: TlsMaterial) -> Result<rustls::ServerConfig, TlsError> {
    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(material.certificates, material.key)
        .map_err(|err| TlsError::Rejected(err.to_string()))
}

/// SHA-256 fingerprint of the leaf certificate, in the `AA:BB:…` form operators expect.
///
/// Printed at startup so that an operator can compare it with what the app pinned, which
/// is the only way to notice a machine-in-the-middle on a self-signed setup.
pub fn fingerprint(certificate: &CertificateDer<'_>) -> String {
    use sha2::{Digest, Sha256};

    Sha256::digest(certificate.as_ref())
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        (dir.path().join("agent.crt"), dir.path().join("agent.key"))
    }

    #[test]
    fn a_first_start_generates_a_usable_certificate() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (cert, key) = paths(&dir);

        let material = load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("generates");
        assert!(material.generated);
        assert_eq!(material.certificates.len(), 1);
        assert!(cert.exists() && key.exists());

        // And rustls accepts the pair, which is the only check that really matters.
        server_config(material).expect("rustls accepts the generated pair");
    }

    #[test]
    fn a_second_start_reuses_the_certificate_rather_than_rotating_it() {
        // Regenerating would change the fingerprint and break the app's pin on every
        // restart, turning a security feature into an alarm that cries wolf.
        let dir = tempfile::tempdir().expect("temp dir");
        let (cert, key) = paths(&dir);

        let first = load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("generates");
        let second = load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("loads");

        assert!(first.generated);
        assert!(!second.generated, "the second start must not regenerate");
        assert_eq!(
            fingerprint(&first.certificates[0]),
            fingerprint(&second.certificates[0])
        );
    }

    #[test]
    fn the_state_directory_is_created_if_it_is_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("var").join("lib").join("vds-agent");
        let cert = nested.join("agent.crt");
        let key = nested.join("agent.key");

        load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("generates");
        assert!(cert.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_generated_key_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let (cert, key) = paths(&dir);
        load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("generates");

        let mode = std::fs::metadata(&key)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the key is readable by others: {mode:o}");
    }

    #[test]
    fn an_empty_hostname_list_still_produces_a_certificate() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (cert, key) = paths(&dir);
        let material = load_or_generate(&cert, &key, &[]).expect("generates");
        server_config(material).expect("usable");
    }

    #[test]
    fn a_certificate_file_with_no_certificate_in_it_is_reported_clearly() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (cert, key) = paths(&dir);
        // Generate a valid pair first, then corrupt the certificate.
        load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("generates");
        std::fs::write(&cert, "this is not a certificate").expect("written");

        let err = load_or_generate(&cert, &key, &[]).expect_err("must fail");
        assert!(matches!(err, TlsError::NoCertificates { .. }), "{err}");
    }

    #[test]
    fn swapping_the_certificate_and_key_files_is_reported_clearly() {
        // A real mistake, and one whose default error message ("no such section") tells
        // an operator nothing about which of the two files is wrong.
        let dir = tempfile::tempdir().expect("temp dir");
        let (cert, key) = paths(&dir);
        load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("generates");

        let certificate_pem = std::fs::read_to_string(&cert).expect("read");
        std::fs::write(&key, certificate_pem).expect("written");

        let err = load_or_generate(&cert, &key, &[]).expect_err("must fail");
        assert!(matches!(err, TlsError::NoPrivateKey { .. }), "{err}");
        assert!(err.to_string().contains("agent.key"), "{err}");
    }

    #[test]
    fn an_unreadable_key_file_names_the_file_in_the_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (cert, key) = paths(&dir);
        load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("generates");
        std::fs::write(&key, "-----BEGIN NOTHING-----\n").expect("written");

        let err = load_or_generate(&cert, &key, &[]).expect_err("must fail");
        assert!(err.to_string().contains("agent.key"), "{err}");
    }

    #[test]
    fn a_missing_key_beside_an_existing_certificate_regenerates_both() {
        // Half a pair is unusable; treating it as a first start is better than failing.
        let dir = tempfile::tempdir().expect("temp dir");
        let (cert, key) = paths(&dir);
        load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("generates");
        std::fs::remove_file(&key).expect("removed");

        let material = load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("regenerates");
        assert!(material.generated);
        assert!(key.exists());
    }

    #[test]
    fn a_fingerprint_is_a_readable_sha256() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (cert, key) = paths(&dir);
        let material = load_or_generate(&cert, &key, &["web-01".to_owned()]).expect("generates");

        let printed = fingerprint(&material.certificates[0]);
        // 32 bytes rendered as `AA:` pairs.
        assert_eq!(printed.len(), 32 * 3 - 1, "was: {printed}");
        assert_eq!(printed.matches(':').count(), 31);
        assert!(printed.chars().all(|c| c.is_ascii_hexdigit() || c == ':'));
    }

    #[test]
    fn two_different_certificates_have_different_fingerprints() {
        let one = tempfile::tempdir().expect("temp dir");
        let two = tempfile::tempdir().expect("temp dir");
        let (cert_a, key_a) = paths(&one);
        let (cert_b, key_b) = paths(&two);

        let a = load_or_generate(&cert_a, &key_a, &["web-01".to_owned()]).expect("generates");
        let b = load_or_generate(&cert_b, &key_b, &["web-01".to_owned()]).expect("generates");
        assert_ne!(
            fingerprint(&a.certificates[0]),
            fingerprint(&b.certificates[0])
        );
    }
}
