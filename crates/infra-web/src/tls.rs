//! TLS certificate inspection.
//!
//! # Why this does its own handshake
//!
//! The point of certificate monitoring is to warn about a certificate *before* it breaks
//! — and to report one that is already broken. A normal HTTPS client refuses to complete
//! the handshake when a certificate is expired, self-signed or for the wrong host, which
//! is correct for fetching data and useless for reporting on it: the client returns "TLS
//! error" and the operator learns nothing about which certificate, whose, or how long it
//! has been wrong.
//!
//! So certificate inspection uses a **separate** connection with a verifier that accepts
//! any chain, reads the certificate the server presented, and reports the facts. The
//! actual HTTP request in [`crate::checker`] uses ordinary, strict verification against
//! the platform trust roots.
//!
//! The inspection connection never sends a request and never reads a response body. It
//! completes the handshake, takes the certificate, and closes. Nothing it learns is
//! trusted for anything except reporting.

use chrono::{DateTime, TimeZone, Utc};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use vds_domain::website::SslInfo;
use x509_parser::prelude::*;

/// Why a certificate could not be inspected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TlsInspectionError {
    #[error("could not connect: {0}")]
    Connect(String),
    #[error("TLS handshake failed: {0}")]
    Handshake(String),
    #[error("the server presented no certificate")]
    NoCertificate,
    #[error("the certificate could not be parsed: {0}")]
    Malformed(String),
    #[error("timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("{0} is not a valid server name")]
    InvalidServerName(String),
}

/// A verifier that accepts every certificate.
///
/// **This is used only to read a certificate, never to trust one.** See the module
/// documentation: rejecting an expired certificate here would defeat the entire purpose
/// of expiry monitoring.
#[derive(Debug)]
struct InspectionOnlyVerifier {
    /// Schemes to advertise. Taken from the crypto provider so the handshake succeeds
    /// against whatever the server chooses.
    schemes: Vec<SignatureScheme>,
}

impl ServerCertVerifier for InspectionOnlyVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        // Deliberately unconditional. The caller inspects and reports; it does not
        // transmit anything over this connection.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes.clone()
    }
}

/// Reads the certificate a host presents, whatever its state.
pub struct CertificateInspector {
    config: Arc<rustls::ClientConfig>,
}

impl CertificateInspector {
    /// Builds an inspector.
    pub fn new() -> Result<Self, TlsInspectionError> {
        let provider = rustls::crypto::ring::default_provider();
        let schemes = provider
            .signature_verification_algorithms
            .supported_schemes();

        let config = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .map_err(|e| TlsInspectionError::Handshake(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(InspectionOnlyVerifier { schemes }))
            .with_no_client_auth();

        Ok(Self {
            config: Arc::new(config),
        })
    }

    /// Connects, completes a handshake, and reports the leaf certificate.
    pub async fn inspect(
        &self,
        host: &str,
        port: u16,
        timeout: Duration,
    ) -> Result<SslInfo, TlsInspectionError> {
        let server_name = ServerName::try_from(host.to_owned())
            .map_err(|_| TlsInspectionError::InvalidServerName(host.to_owned()))?;

        let connect = async {
            let stream = tokio::net::TcpStream::connect((host, port))
                .await
                .map_err(|e| TlsInspectionError::Connect(e.to_string()))?;

            let connector = tokio_rustls::TlsConnector::from(Arc::clone(&self.config));
            let tls = connector
                .connect(server_name, stream)
                .await
                .map_err(|e| TlsInspectionError::Handshake(e.to_string()))?;

            let (_, connection) = tls.get_ref();
            let chain = connection
                .peer_certificates()
                .ok_or(TlsInspectionError::NoCertificate)?
                .to_vec();
            Ok::<Vec<CertificateDer<'static>>, TlsInspectionError>(
                chain.into_iter().map(|c| c.into_owned()).collect(),
            )
        };

        let chain = tokio::time::timeout(timeout, connect).await.map_err(|_| {
            TlsInspectionError::Timeout {
                seconds: timeout.as_secs(),
            }
        })??;

        let leaf = chain.first().ok_or(TlsInspectionError::NoCertificate)?;
        parse_certificate(leaf.as_ref())
    }
}

/// Extracts the fields the domain cares about from a DER certificate.
pub fn parse_certificate(der: &[u8]) -> Result<SslInfo, TlsInspectionError> {
    let (_, certificate) =
        X509Certificate::from_der(der).map_err(|e| TlsInspectionError::Malformed(e.to_string()))?;

    let validity = certificate.validity();
    let not_before = to_datetime(validity.not_before.timestamp())?;
    let not_after = to_datetime(validity.not_after.timestamp())?;

    let san = certificate
        .subject_alternative_name()
        .ok()
        .flatten()
        .map(|ext| {
            ext.value
                .general_names
                .iter()
                .filter_map(|name| match name {
                    GeneralName::DNSName(dns) => Some((*dns).to_owned()),
                    GeneralName::IPAddress(bytes) => Some(format_ip(bytes)),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(SslInfo {
        subject: certificate.subject().to_string(),
        issuer: certificate.issuer().to_string(),
        not_before,
        not_after,
        fingerprint: fingerprint(der),
        san,
    })
}

/// SHA-256 fingerprint of a DER certificate, hex encoded.
pub fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    hex::encode(digest)
}

fn to_datetime(timestamp: i64) -> Result<DateTime<Utc>, TlsInspectionError> {
    Utc.timestamp_opt(timestamp, 0).single().ok_or_else(|| {
        TlsInspectionError::Malformed(format!("{timestamp} is not a valid certificate date"))
    })
}

/// Renders an IP SAN entry.
fn format_ip(bytes: &[u8]) -> String {
    match bytes.len() {
        4 => bytes
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join("."),
        16 => {
            let groups: Vec<String> = bytes
                .chunks(2)
                .map(|pair| format!("{:x}", u16::from(pair[0]) << 8 | u16::from(pair[1])))
                .collect();
            groups.join(":")
        }
        _ => hex::encode(bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real DER certificate, generated once and pasted here so the parser is tested
    /// against genuine bytes rather than a hand-built structure.
    ///
    /// Subject and issuer `CN=example.com`, self-signed, valid 2023-01-01 to 2033-01-01.
    const SELF_SIGNED_DER: &[u8] = include_bytes!("../tests/fixtures/example-com.der");

    #[test]
    fn a_certificate_yields_its_subject_issuer_and_validity() {
        let info = parse_certificate(SELF_SIGNED_DER).expect("parses");
        assert!(
            info.subject.contains("example.com"),
            "subject was {}",
            info.subject
        );
        assert!(
            info.issuer.contains("example.com"),
            "issuer was {}",
            info.issuer
        );
        assert!(info.not_before < info.not_after);
    }

    #[test]
    fn subject_alternative_names_are_extracted() {
        let info = parse_certificate(SELF_SIGNED_DER).expect("parses");
        assert!(
            info.san.iter().any(|name| name == "example.com"),
            "SANs were {:?}",
            info.san
        );
    }

    #[test]
    fn the_fingerprint_is_a_sha256_hex_digest() {
        let info = parse_certificate(SELF_SIGNED_DER).expect("parses");
        assert_eq!(info.fingerprint.len(), 64);
        assert!(info.fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(info.fingerprint, fingerprint(SELF_SIGNED_DER));
    }

    #[test]
    fn days_remaining_is_computed_against_the_expiry_date() {
        let info = parse_certificate(SELF_SIGNED_DER).expect("parses");
        let ten_days_before = info.not_after - chrono::Duration::days(10);
        assert_eq!(info.days_remaining(ten_days_before), 10);
        assert!(!info.is_expired(ten_days_before));

        let after = info.not_after + chrono::Duration::days(1);
        assert!(info.is_expired(after));
        assert!(info.days_remaining(after) < 0);
    }

    #[test]
    fn garbage_is_rejected_as_malformed_not_as_a_panic() {
        let err = parse_certificate(b"this is not a certificate").expect_err("must fail");
        assert!(matches!(err, TlsInspectionError::Malformed(_)));
    }

    #[test]
    fn an_empty_certificate_is_rejected() {
        assert!(parse_certificate(&[]).is_err());
    }

    #[test]
    fn a_truncated_certificate_is_rejected() {
        let truncated = &SELF_SIGNED_DER[..SELF_SIGNED_DER.len() / 2];
        assert!(parse_certificate(truncated).is_err());
    }

    #[test]
    fn ip_sans_are_rendered_readably() {
        assert_eq!(format_ip(&[127, 0, 0, 1]), "127.0.0.1");
        assert_eq!(format_ip(&[192, 168, 1, 255]), "192.168.1.255");
        let v6 = [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert_eq!(format_ip(&v6), "2001:db8:0:0:0:0:0:1");
        // Anything unexpected falls back to hex rather than producing nonsense.
        assert_eq!(format_ip(&[1, 2, 3]), "010203");
    }

    #[test]
    fn the_inspector_builds() {
        assert!(CertificateInspector::new().is_ok());
    }

    #[tokio::test]
    async fn an_unreachable_host_reports_a_connection_error_not_a_hang() {
        let inspector = CertificateInspector::new().expect("builds");
        // Port 1 on localhost is reliably closed.
        let err = inspector
            .inspect("127.0.0.1", 1, Duration::from_secs(2))
            .await
            .expect_err("must fail");
        assert!(
            matches!(
                err,
                TlsInspectionError::Connect(_) | TlsInspectionError::Timeout { .. }
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn an_invalid_server_name_is_rejected_before_connecting() {
        let inspector = CertificateInspector::new().expect("builds");
        let err = inspector
            .inspect("not a hostname!", 443, Duration::from_secs(1))
            .await
            .expect_err("must fail");
        assert!(
            matches!(err, TlsInspectionError::InvalidServerName(_)),
            "got {err:?}"
        );
    }
}
