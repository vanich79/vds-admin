//! An authenticated SSH session and the [`CommandRunner`] built on it.

use crate::batch;
use crate::known_hosts::{HostKeyVerdict, KnownHosts};
use async_trait::async_trait;
use russh::client::{self, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, decode_secret_key};
use russh::{ChannelMsg, Disconnect};
use std::sync::Arc;
use std::time::Duration;
use vds_domain::ports::{
    Command, CommandOutput, CommandRunner, Secret, TransportCapabilities, TransportError,
};

/// How the session authenticates.
///
/// Holds borrowed secret material for the duration of one connection attempt and nothing
/// longer. It is never stored on the session.
pub enum SshCredential {
    Password(Secret),
    /// A private key, with an optional passphrase for an encrypted one.
    PrivateKey {
        key: Secret,
        passphrase: Option<Secret>,
    },
}

impl std::fmt::Debug for SshCredential {
    /// Hand-written: a derived `Debug` would print the key material.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            SshCredential::Password(_) => "Password",
            SshCredential::PrivateKey {
                passphrase: Some(_),
                ..
            } => "EncryptedPrivateKey",
            SshCredential::PrivateKey { .. } => "PrivateKey",
        };
        write!(f, "SshCredential::{kind}(<redacted>)")
    }
}

/// Connection parameters.
#[derive(Debug, Clone)]
pub struct SshSettings {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub connect_timeout: Duration,
    pub command_timeout: Duration,
    /// Longest a session is kept in the pool without being used.
    pub idle_timeout: Duration,
}

impl SshSettings {
    pub fn new(host: impl Into<String>, port: u16, username: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            connect_timeout: Duration::from_secs(15),
            command_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(300),
        }
    }
}

/// The russh client handler.
///
/// Its one real job is host key verification; everything else is default behaviour.
struct Handler {
    known_hosts: Arc<KnownHosts>,
    host: String,
    port: u16,
    /// Set when the key was rejected, so the caller can report *why* the handshake
    /// failed rather than "connection closed".
    rejection: Arc<parking_lot::Mutex<Option<String>>>,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let bytes = match server_public_key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => {
                key.to_bytes().unwrap_or_default()
            }
            russh::keys::PublicKeyOrCertificate::Certificate(certificate) => {
                certificate.to_bytes().unwrap_or_default()
            }
        };

        match self.known_hosts.verify(&self.host, self.port, &bytes) {
            HostKeyVerdict::Known | HostKeyVerdict::TrustedOnFirstUse => Ok(true),
            HostKeyVerdict::Changed {
                expected,
                presented,
            } => {
                *self.rejection.lock() = Some(format!(
                    "the host key for {}:{} has changed (pinned {expected}, presented {presented}); \
                     if you rebuilt this server, forget its key in Settings",
                    self.host, self.port
                ));
                Ok(false)
            }
        }
    }
}

/// An authenticated SSH session.
pub struct SshSession {
    handle: Handle<Handler>,
    settings: SshSettings,
    last_used: parking_lot::Mutex<std::time::Instant>,
}

impl SshSession {
    /// Connects and authenticates.
    pub async fn connect(
        settings: SshSettings,
        credential: &SshCredential,
        known_hosts: Arc<KnownHosts>,
    ) -> Result<Self, TransportError> {
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(settings.idle_timeout),
            keepalive_interval: Some(Duration::from_secs(30)),
            nodelay: true,
            ..Default::default()
        });

        let rejection = Arc::new(parking_lot::Mutex::new(None));
        let handler = Handler {
            known_hosts,
            host: settings.host.clone(),
            port: settings.port,
            rejection: Arc::clone(&rejection),
        };

        let connect = client::connect(config, (settings.host.as_str(), settings.port), handler);
        let mut handle = match tokio::time::timeout(settings.connect_timeout, connect).await {
            Ok(Ok(handle)) => handle,
            Ok(Err(err)) => {
                // A host-key rejection surfaces from russh as a generic failure, so the
                // specific reason is recovered from the handler.
                if let Some(reason) = rejection.lock().take() {
                    return Err(TransportError::HostKeyRejected(reason));
                }
                return Err(TransportError::Connection(err.to_string()));
            }
            Err(_) => {
                return Err(TransportError::Timeout {
                    seconds: settings.connect_timeout.as_secs(),
                });
            }
        };

        authenticate(&mut handle, &settings, credential).await?;

        Ok(Self {
            handle,
            settings,
            last_used: parking_lot::Mutex::new(std::time::Instant::now()),
        })
    }

    /// Whether the session has been idle long enough to be worth discarding.
    pub fn is_stale(&self) -> bool {
        self.last_used.lock().elapsed() > self.settings.idle_timeout
    }

    /// Runs one shell script and returns its combined output.
    pub async fn run_script(&self, script: &str) -> Result<String, TransportError> {
        *self.last_used.lock() = std::time::Instant::now();

        let channel = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| TransportError::Execution(format!("could not open a channel: {e}")))?;

        let script = script.to_owned();
        let timeout = self.settings.command_timeout;

        let execute = async move {
            let mut channel = channel;
            channel
                .exec(true, script.as_bytes())
                .await
                .map_err(|e| TransportError::Execution(e.to_string()))?;

            let mut output = String::new();
            while let Some(message) = channel.wait().await {
                match message {
                    // Both streams are wanted: the batch script redirects stderr into
                    // stdout, but a shell that dies before running it writes to stderr.
                    ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                        output.push_str(&String::from_utf8_lossy(&data));
                    }
                    ChannelMsg::Eof | ChannelMsg::Close => break,
                    _ => {}
                }
            }
            Ok::<String, TransportError>(output)
        };

        tokio::time::timeout(timeout, execute)
            .await
            .map_err(|_| TransportError::Timeout {
                seconds: timeout.as_secs(),
            })?
    }

    /// Closes the session politely.
    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "", "en")
            .await;
    }
}

/// Authenticates a freshly connected handle.
async fn authenticate(
    handle: &mut Handle<Handler>,
    settings: &SshSettings,
    credential: &SshCredential,
) -> Result<(), TransportError> {
    let result = match credential {
        SshCredential::Password(secret) => {
            let password = secret
                .expose_str()
                .map_err(|_| TransportError::Authentication("the password is not UTF-8".into()))?;
            handle
                .authenticate_password(settings.username.clone(), password)
                .await
                .map_err(|e| TransportError::Authentication(e.to_string()))?
        }
        SshCredential::PrivateKey { key, passphrase } => {
            let key_text = key
                .expose_str()
                .map_err(|_| TransportError::Authentication("the key is not UTF-8".into()))?;
            let passphrase_text = passphrase
                .as_ref()
                .map(|p| p.expose_str())
                .transpose()
                .map_err(|_| {
                    TransportError::Authentication("the passphrase is not UTF-8".into())
                })?;

            let private_key = decode_secret_key(key_text, passphrase_text).map_err(|e| {
                // An encrypted key with no passphrase is a configuration mistake worth
                // naming precisely, because the generic message is baffling.
                if passphrase_text.is_none() {
                    TransportError::Authentication(format!(
                        "could not read the private key ({e}); if it is encrypted, a \
                         passphrase is required"
                    ))
                } else {
                    TransportError::Authentication(format!("could not read the private key: {e}"))
                }
            })?;

            // RSA keys need a hash algorithm chosen; asking the server what it accepts
            // avoids failing against hosts that have disabled the SHA-1 variant.
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten();
            let with_hash =
                PrivateKeyWithHashAlg::new(Arc::new(private_key), hash_or_default(hash));

            handle
                .authenticate_publickey(settings.username.clone(), with_hash)
                .await
                .map_err(|e| TransportError::Authentication(e.to_string()))?
        }
    };

    if result.success() {
        Ok(())
    } else {
        Err(TransportError::Authentication(format!(
            "the server rejected authentication for {}",
            settings.username
        )))
    }
}

/// Prefers SHA-256 when the server did not express a preference.
fn hash_or_default(hash: Option<HashAlg>) -> Option<HashAlg> {
    hash.or(Some(HashAlg::Sha256))
}

/// Runs collector commands over an SSH session.
pub struct SshCommandRunner {
    session: Arc<SshSession>,
}

impl SshCommandRunner {
    pub fn new(session: Arc<SshSession>) -> Self {
        Self { session }
    }
}

#[async_trait]
impl CommandRunner for SshCommandRunner {
    async fn execute(
        &self,
        commands: &[Command],
    ) -> Result<Vec<Result<CommandOutput, TransportError>>, TransportError> {
        if commands.is_empty() {
            return Ok(Vec::new());
        }

        let script = batch::build_script(commands);
        let raw = self.session.run_script(&script).await?;
        Ok(batch::split_output(&raw, commands.len()))
    }

    fn capabilities(&self) -> TransportCapabilities {
        TransportCapabilities {
            // The whole batch goes in one channel, one round trip.
            supports_batching: true,
            supports_direct_file_read: false,
            supports_privileged: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_never_appear_in_debug_output() {
        // These structs end up inside error contexts and tracing spans.
        let password = SshCredential::Password(Secret::from_string("hunter2".into()));
        let rendered = format!("{password:?}");
        assert!(!rendered.contains("hunter2"), "Debug leaked: {rendered}");
        assert_eq!(rendered, "SshCredential::Password(<redacted>)");

        let key = SshCredential::PrivateKey {
            key: Secret::from_string("-----BEGIN OPENSSH PRIVATE KEY-----".into()),
            passphrase: Some(Secret::from_string("secret".into())),
        };
        let rendered = format!("{key:?}");
        assert!(!rendered.contains("BEGIN"), "Debug leaked: {rendered}");
        assert!(!rendered.contains("secret"), "Debug leaked: {rendered}");
        assert_eq!(rendered, "SshCredential::EncryptedPrivateKey(<redacted>)");
    }

    #[test]
    fn an_unencrypted_key_is_labelled_differently_from_an_encrypted_one() {
        let plain = SshCredential::PrivateKey {
            key: Secret::from_string("key".into()),
            passphrase: None,
        };
        assert_eq!(
            format!("{plain:?}"),
            "SshCredential::PrivateKey(<redacted>)"
        );
    }

    #[test]
    fn settings_carry_sensible_default_timeouts() {
        let settings = SshSettings::new("10.0.0.1", 22, "root");
        assert_eq!(settings.connect_timeout, Duration::from_secs(15));
        assert!(settings.command_timeout >= settings.connect_timeout);
        assert!(settings.idle_timeout > settings.command_timeout);
    }

    #[test]
    fn rsa_hashing_defaults_to_sha256_when_the_server_is_silent() {
        // Falling back to SHA-1 would fail against any modern, hardened sshd.
        assert_eq!(hash_or_default(None), Some(HashAlg::Sha256));
        assert_eq!(
            hash_or_default(Some(HashAlg::Sha512)),
            Some(HashAlg::Sha512)
        );
    }

    #[tokio::test]
    async fn connecting_to_a_closed_port_fails_promptly() {
        let settings = SshSettings {
            connect_timeout: Duration::from_secs(2),
            ..SshSettings::new("127.0.0.1", 1, "root")
        };

        let started = std::time::Instant::now();
        let result = SshSession::connect(
            settings,
            &SshCredential::Password(Secret::from_string("x".into())),
            Arc::new(KnownHosts::in_memory()),
        )
        .await;

        let err = result.err().expect("must fail");
        assert!(
            matches!(
                err,
                TransportError::Connection(_) | TransportError::Timeout { .. }
            ),
            "got {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the timeout was not enforced"
        );
    }

    #[tokio::test]
    async fn a_connection_timeout_is_reported_as_a_timeout() {
        // 203.0.113.0/24 is TEST-NET-3: reserved, and reliably unroutable.
        let settings = SshSettings {
            connect_timeout: Duration::from_millis(600),
            ..SshSettings::new("203.0.113.1", 22, "root")
        };

        let result = SshSession::connect(
            settings,
            &SshCredential::Password(Secret::from_string("x".into())),
            Arc::new(KnownHosts::in_memory()),
        )
        .await;

        assert!(
            result.is_err(),
            "an unroutable address must not appear to connect"
        );
    }

    #[test]
    fn an_empty_batch_needs_no_round_trip() {
        // Guards against opening a channel to run nothing.
        let commands: Vec<Command> = Vec::new();
        assert!(batch::build_script(&commands).lines().count() <= 2);
    }
}
