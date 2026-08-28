//! The agentless [`ServerProbe`]: pooled SSH sessions driving the shared collectors.

use crate::known_hosts::KnownHosts;
use crate::session::{SshCredential, SshSession, SshSettings};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use vds_domain::ids::ServerId;
use vds_domain::ports::{SecretKind, SecretStore, ServerProbe, TransportError};
use vds_domain::server::{ConnectionSettings, Server, ServerSnapshot, SshAuthKind};
use vds_infra_collectors::CollectorRegistry;

/// Collects server metrics over SSH.
///
/// Sessions are pooled per server: SSH's handshake is expensive — a TCP round trip, a key
/// exchange and an authentication exchange — and repeating it every fifteen seconds for
/// two hundred servers is most of a monitoring cycle's cost.
pub struct SshServerProbe {
    registry: CollectorRegistry,
    secrets: Arc<dyn SecretStore>,
    known_hosts: Arc<KnownHosts>,
    sessions: Mutex<HashMap<ServerId, Arc<SshSession>>>,
}

impl SshServerProbe {
    pub fn new(
        registry: CollectorRegistry,
        secrets: Arc<dyn SecretStore>,
        known_hosts: Arc<KnownHosts>,
    ) -> Self {
        Self {
            registry,
            secrets,
            known_hosts,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Number of pooled sessions, for the debug panel.
    pub fn pooled_sessions(&self) -> usize {
        self.sessions.lock().len()
    }

    /// Returns a usable session, connecting or reconnecting as required.
    async fn session(&self, server: &Server) -> Result<Arc<SshSession>, TransportError> {
        // A stale session is dropped rather than used: reusing one the server has already
        // timed out produces a confusing "channel open failed" instead of a clean
        // reconnect.
        {
            let mut sessions = self.sessions.lock();
            if let Some(existing) = sessions.get(&server.id) {
                if !existing.is_stale() {
                    return Ok(Arc::clone(existing));
                }
                sessions.remove(&server.id);
            }
        }

        let session = Arc::new(self.connect(server).await?);
        self.sessions.lock().insert(server.id, Arc::clone(&session));
        Ok(session)
    }

    async fn connect(&self, server: &Server) -> Result<SshSession, TransportError> {
        let ConnectionSettings::Ssh(ssh) = &server.connection else {
            return Err(TransportError::Protocol(
                "this server is not configured for SSH".to_owned(),
            ));
        };

        let credential = self.credential(ssh.auth_kind, ssh.credential_ref).await?;

        let settings = SshSettings {
            host: server.host.clone(),
            port: server.port,
            username: ssh.username.clone(),
            connect_timeout: Duration::from_secs(u64::from(server.timeout_secs.max(1))),
            command_timeout: Duration::from_secs(u64::from(server.timeout_secs.max(1))),
            idle_timeout: Duration::from_secs(300),
        };

        SshSession::connect(settings, &credential, Arc::clone(&self.known_hosts)).await
    }

    /// Fetches the secret material for one connection attempt.
    ///
    /// Resolved per attempt and dropped immediately afterwards, so nothing long-lived
    /// ever holds a password or a private key.
    async fn credential(
        &self,
        kind: SshAuthKind,
        reference: vds_domain::ids::CredentialRef,
    ) -> Result<SshCredential, TransportError> {
        let missing = |what: &str| {
            let what = what.to_owned();
            move |e: vds_domain::ports::SecretStoreError| {
                TransportError::MissingCredential(format!("{what}: {e}"))
            }
        };

        match kind {
            SshAuthKind::Password => {
                let secret = self
                    .secrets
                    .retrieve(reference, SecretKind::SshPassword)
                    .await
                    .map_err(missing("password"))?;
                Ok(SshCredential::Password(secret))
            }
            SshAuthKind::PrivateKey => {
                let key = self
                    .secrets
                    .retrieve(reference, SecretKind::SshPrivateKey)
                    .await
                    .map_err(missing("private key"))?;
                Ok(SshCredential::PrivateKey {
                    key,
                    passphrase: None,
                })
            }
            SshAuthKind::EncryptedPrivateKey => {
                let key = self
                    .secrets
                    .retrieve(reference, SecretKind::SshPrivateKey)
                    .await
                    .map_err(missing("private key"))?;
                let passphrase = self
                    .secrets
                    .retrieve(reference, SecretKind::SshKeyPassphrase)
                    .await
                    .map_err(missing("key passphrase"))?;
                Ok(SshCredential::PrivateKey {
                    key,
                    passphrase: Some(passphrase),
                })
            }
        }
    }

    /// Drops a pooled session, so the next probe reconnects.
    fn evict(&self, server_id: ServerId) {
        self.sessions.lock().remove(&server_id);
    }
}

#[async_trait]
impl ServerProbe for SshServerProbe {
    async fn probe(
        &self,
        server: &Server,
        at: DateTime<Utc>,
    ) -> Result<ServerSnapshot, TransportError> {
        let session = self.session(server).await?;
        let runner = crate::session::SshCommandRunner::new(Arc::clone(&session));

        match self.registry.collect(&runner, server.id, at).await {
            Ok(snapshot) => Ok(snapshot),
            Err(err) => {
                // A transport failure means this session is no good any more. Keeping it
                // pooled would make every subsequent cycle fail the same way.
                if err.is_retryable() {
                    self.evict(server.id);
                }
                Err(err)
            }
        }
    }

    async fn ping(&self, server: &Server) -> Result<(), TransportError> {
        let session = self.session(server).await?;
        // Cheapest possible proof of life: a shell that runs and exits.
        session.run_script("exit 0\n").await.map(|_| ())
    }

    async fn disconnect(&self, server_id: ServerId) {
        let session = self.sessions.lock().remove(&server_id);
        if let Some(session) = session {
            session.disconnect().await;
        }
    }
}

impl std::fmt::Debug for SshServerProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshServerProbe")
            .field("collectors", &self.registry.len())
            .field("pooled_sessions", &self.pooled_sessions())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::ids::CredentialRef;
    use vds_domain::ports::{Secret, SecretStoreError};
    use vds_domain::server::{AgentSettings, SshSettings as DomainSshSettings};

    /// A store holding whatever the test put in it.
    #[derive(Default)]
    struct StubSecrets {
        entries: Mutex<HashMap<(CredentialRef, &'static str), String>>,
    }

    impl StubSecrets {
        fn with(reference: CredentialRef, kind: SecretKind, value: &str) -> Arc<Self> {
            let store = Arc::new(Self::default());
            store
                .entries
                .lock()
                .insert((reference, kind.as_str()), value.to_owned());
            store
        }

        fn add(&self, reference: CredentialRef, kind: SecretKind, value: &str) {
            self.entries
                .lock()
                .insert((reference, kind.as_str()), value.to_owned());
        }
    }

    #[async_trait]
    impl SecretStore for StubSecrets {
        async fn store(
            &self,
            _reference: CredentialRef,
            _kind: SecretKind,
            _secret: Secret,
        ) -> Result<(), SecretStoreError> {
            Ok(())
        }

        async fn retrieve(
            &self,
            reference: CredentialRef,
            kind: SecretKind,
        ) -> Result<Secret, SecretStoreError> {
            self.entries
                .lock()
                .get(&(reference, kind.as_str()))
                .cloned()
                .map(Secret::from_string)
                .ok_or_else(|| SecretStoreError::NotFound(kind.as_str().to_owned()))
        }

        async fn delete(
            &self,
            _reference: CredentialRef,
            _kind: SecretKind,
        ) -> Result<(), SecretStoreError> {
            Ok(())
        }

        async fn contains(
            &self,
            reference: CredentialRef,
            kind: SecretKind,
        ) -> Result<bool, SecretStoreError> {
            Ok(self
                .entries
                .lock()
                .contains_key(&(reference, kind.as_str())))
        }

        async fn delete_all(&self, _reference: CredentialRef) -> Result<(), SecretStoreError> {
            Ok(())
        }

        fn backend_description(&self) -> String {
            "stub".to_owned()
        }
    }

    fn probe_with(secrets: Arc<dyn SecretStore>) -> SshServerProbe {
        SshServerProbe::new(
            CollectorRegistry::linux(),
            secrets,
            Arc::new(KnownHosts::in_memory()),
        )
    }

    fn ssh_server(kind: SshAuthKind, reference: CredentialRef) -> Server {
        let mut server = Server::new(
            "prod-01",
            "127.0.0.1",
            ConnectionSettings::Ssh(DomainSshSettings {
                username: "root".into(),
                auth_kind: kind,
                credential_ref: reference,
            }),
            Utc::now(),
        );
        // Port 1 is reliably closed, so nothing in these tests can accidentally reach a
        // real SSH daemon.
        server.port = 1;
        server.timeout_secs = 2;
        server
    }

    #[tokio::test]
    async fn a_password_credential_is_resolved_from_the_secret_store() {
        let reference = CredentialRef::new();
        let secrets = StubSecrets::with(reference, SecretKind::SshPassword, "hunter2");
        let probe = probe_with(secrets);

        let credential = probe
            .credential(SshAuthKind::Password, reference)
            .await
            .expect("resolved");
        match credential {
            SshCredential::Password(secret) => assert_eq!(secret.expose(), b"hunter2"),
            other => panic!("expected a password, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_encrypted_key_resolves_both_the_key_and_its_passphrase() {
        let reference = CredentialRef::new();
        let secrets = StubSecrets::with(reference, SecretKind::SshPrivateKey, "KEYDATA");
        secrets.add(reference, SecretKind::SshKeyPassphrase, "PASS");
        let probe = probe_with(secrets);

        let credential = probe
            .credential(SshAuthKind::EncryptedPrivateKey, reference)
            .await
            .expect("resolved");
        match credential {
            SshCredential::PrivateKey { key, passphrase } => {
                assert_eq!(key.expose(), b"KEYDATA");
                assert_eq!(passphrase.expect("present").expose(), b"PASS");
            }
            other => panic!("expected a key, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_credential_names_what_is_missing() {
        // "authentication failed" would send the user hunting in the wrong place.
        let reference = CredentialRef::new();
        let probe = probe_with(Arc::new(StubSecrets::default()));

        let err = probe
            .credential(SshAuthKind::Password, reference)
            .await
            .expect_err("must fail");
        assert!(
            matches!(err, TransportError::MissingCredential(_)),
            "got {err:?}"
        );
        assert!(err.to_string().contains("password"), "message was: {err}");
    }

    #[tokio::test]
    async fn a_missing_passphrase_for_an_encrypted_key_is_reported_specifically() {
        let reference = CredentialRef::new();
        let secrets = StubSecrets::with(reference, SecretKind::SshPrivateKey, "KEYDATA");
        let probe = probe_with(secrets);

        let err = probe
            .credential(SshAuthKind::EncryptedPrivateKey, reference)
            .await
            .expect_err("must fail");
        assert!(err.to_string().contains("passphrase"), "message was: {err}");
    }

    #[tokio::test]
    async fn probing_a_server_configured_for_the_agent_is_rejected_clearly() {
        let probe = probe_with(Arc::new(StubSecrets::default()));
        let server = Server::new(
            "agent-01",
            "127.0.0.1",
            ConnectionSettings::Agent(AgentSettings {
                port: 9443,
                credential_ref: CredentialRef::new(),
                certificate_fingerprint: None,
            }),
            Utc::now(),
        );

        let err = probe
            .probe(&server, Utc::now())
            .await
            .expect_err("must fail");
        assert!(matches!(err, TransportError::Protocol(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn an_unreachable_server_fails_without_pooling_a_broken_session() {
        let reference = CredentialRef::new();
        let secrets = StubSecrets::with(reference, SecretKind::SshPassword, "hunter2");
        let probe = probe_with(secrets);
        let server = ssh_server(SshAuthKind::Password, reference);

        assert!(probe.probe(&server, Utc::now()).await.is_err());
        assert_eq!(
            probe.pooled_sessions(),
            0,
            "a failed connection must not be pooled"
        );
    }

    #[tokio::test]
    async fn a_missing_credential_fails_before_any_connection_is_attempted() {
        let probe = probe_with(Arc::new(StubSecrets::default()));
        let server = ssh_server(SshAuthKind::Password, CredentialRef::new());

        let started = std::time::Instant::now();
        let err = probe
            .probe(&server, Utc::now())
            .await
            .expect_err("must fail");

        assert!(matches!(err, TransportError::MissingCredential(_)));
        // No network round trip happened.
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[tokio::test]
    async fn disconnecting_an_unknown_server_is_harmless() {
        let probe = probe_with(Arc::new(StubSecrets::default()));
        probe.disconnect(ServerId::new()).await;
        assert_eq!(probe.pooled_sessions(), 0);
    }

    #[test]
    fn the_debug_output_shows_pool_state_and_no_secrets() {
        let probe = probe_with(Arc::new(StubSecrets::default()));
        let rendered = format!("{probe:?}");
        assert!(rendered.contains("pooled_sessions"));
        assert!(rendered.contains("collectors"));
    }
}
