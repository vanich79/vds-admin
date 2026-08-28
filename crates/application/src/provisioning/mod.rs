//! Creating and removing the things being monitored.
//!
//! Adding a server is the one operation where a secret enters the system, so it is worth
//! having in one place rather than spread through a dialog handler. The order matters:
//!
//! 1. validate everything first, so a bad form never reaches storage;
//! 2. store the secret, and get back a handle;
//! 3. store the entity, which carries only the handle;
//! 4. if step 3 fails, remove the secret again.
//!
//! Step 4 is the part that is easy to leave out. Without it, a failed save leaves an
//! orphaned password in the OS keystore that nothing will ever read or delete — and the
//! user, seeing an error, will try again and leave another.
//!
//! Deleting reverses it: the entity goes first, then the secrets, then the cached state.
//! A secret whose owner is already gone is unreachable, which is the safe way round.

use std::sync::Arc;
use vds_domain::ids::{CredentialRef, ServerId, WebsiteId};
use vds_domain::ports::{
    Clock, RepositoryError, Secret, SecretKind, SecretStore, SecretStoreError, ServerRepository,
    WebsiteRepository,
};
use vds_domain::server::{
    AgentSettings, ConnectionSettings, Server, ServerValidationError, SshAuthKind, SshSettings,
};
use vds_domain::website::{HttpExpectation, Website, WebsiteValidationError};

/// Why a server or website could not be created.
#[derive(Debug, thiserror::Error)]
pub enum ProvisioningError {
    #[error(transparent)]
    InvalidServer(#[from] ServerValidationError),
    #[error(transparent)]
    InvalidWebsite(#[from] WebsiteValidationError),
    #[error("a credential is required for this connection mode")]
    MissingCredential,
    #[error("could not store the credential: {0}")]
    Secrets(#[from] SecretStoreError),
    #[error("could not save: {0}")]
    Repository(#[from] RepositoryError),
}

impl ProvisioningError {
    /// A single line to put in front of the user.
    ///
    /// The `Display` of the underlying error is already written for a person; this exists
    /// so callers do not have to decide whether to unwrap the chain.
    pub fn message(&self) -> String {
        self.to_string()
    }
}

/// What the "add server" form collected.
///
/// The secret is [`Secret`], not `String`, so it cannot be logged or serialised on its
/// way through this struct — the same guarantee applies here as everywhere else.
#[derive(Clone)]
pub struct NewServer {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub connection: NewConnection,
    pub poll_interval_secs: u32,
    pub tags: Vec<String>,
}

/// The mode-specific half of a new server.
#[derive(Clone)]
pub enum NewConnection {
    Ssh {
        username: String,
        auth_kind: SshAuthKind,
        /// The password, or the private key.
        secret: Secret,
        /// Passphrase, for an encrypted private key.
        passphrase: Option<Secret>,
    },
    Agent {
        port: u16,
        token: Secret,
    },
}

/// What the "add website" form collected.
#[derive(Debug, Clone)]
pub struct NewWebsite {
    pub name: String,
    pub url: String,
    pub server_id: Option<ServerId>,
    pub poll_interval_secs: u32,
    pub expected_status: u16,
    pub expected_text: Option<String>,
}

/// Prints the form without its credential.
///
/// Hand-written for the same reason as everywhere else in this codebase: a derived
/// `Debug` would start printing a new field the day someone adds one, and this struct
/// travels through a message queue that is logged at trace level.
impl std::fmt::Debug for NewServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewServer")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("connection", &self.connection)
            .field("poll_interval_secs", &self.poll_interval_secs)
            .field("tags", &self.tags)
            .finish()
    }
}

impl std::fmt::Debug for NewConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NewConnection::Ssh {
                username,
                auth_kind,
                ..
            } => f
                .debug_struct("Ssh")
                .field("username", username)
                .field("auth_kind", auth_kind)
                .field("secret", &"<redacted>")
                .finish(),
            NewConnection::Agent { port, .. } => f
                .debug_struct("Agent")
                .field("port", port)
                .field("token", &"<redacted>")
                .finish(),
        }
    }
}

/// Creates and removes servers and websites.
pub struct ProvisioningService {
    servers: Arc<dyn ServerRepository>,
    websites: Arc<dyn WebsiteRepository>,
    secrets: Arc<dyn SecretStore>,
    clock: Arc<dyn Clock>,
}

impl ProvisioningService {
    pub fn new(
        servers: Arc<dyn ServerRepository>,
        websites: Arc<dyn WebsiteRepository>,
        secrets: Arc<dyn SecretStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            servers,
            websites,
            secrets,
            clock,
        }
    }

    /// Adds a server, storing its credential in the secret store.
    pub async fn create_server(&self, new: NewServer) -> Result<Server, ProvisioningError> {
        let reference = CredentialRef::new();
        let mut server = Server::new(
            new.name.trim(),
            new.host.trim(),
            connection_settings(&new.connection, reference),
            self.clock.now(),
        );
        server.port = new.port;
        server.poll_interval_secs = new.poll_interval_secs;
        server.tags = new.tags;

        // Validated before anything is stored, so an empty name cannot leave a password
        // behind in the keystore.
        server.validate()?;

        self.store_credential(reference, &new.connection).await?;

        if let Err(err) = self.servers.save(&server).await {
            // The secret has no owner now. Leaving it would be an unreachable password in
            // the user's keystore that nothing will ever clean up.
            if let Err(cleanup) = self.secrets.delete_all(reference).await {
                tracing::warn!(error = %cleanup, "could not roll back a stored credential");
            }
            return Err(err.into());
        }

        tracing::info!(server = %server.id, name = %server.name, "server added");
        Ok(server)
    }

    /// Removes a server and everything stored for it.
    ///
    /// The entity goes first: a credential whose owner is already gone is unreachable,
    /// whereas a server whose credential has gone is a server that fails every poll with
    /// a confusing error.
    pub async fn delete_server(&self, id: ServerId) -> Result<(), ProvisioningError> {
        let reference = self
            .servers
            .get(id)
            .await
            .ok()
            .map(|s| s.connection.credential_ref());

        self.servers.delete(id).await?;

        if let Some(reference) = reference
            && let Err(err) = self.secrets.delete_all(reference).await
        {
            // Worth reporting, not worth failing: the server is already gone, and
            // returning an error here would make the deletion look unsuccessful.
            tracing::warn!(error = %err, "could not remove the stored credential");
        }

        tracing::info!(server = %id, "server removed");
        Ok(())
    }

    /// Adds a website.
    pub async fn create_website(&self, new: NewWebsite) -> Result<Website, ProvisioningError> {
        let mut website = Website::new(new.name.trim(), normalise_url(&new.url), self.clock.now());
        website.server_id = new.server_id;
        website.poll_interval_secs = new.poll_interval_secs;
        website.expectation = HttpExpectation {
            status: new.expected_status,
            body_contains: new
                .expected_text
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty()),
        };

        website.validate()?;
        self.websites.save(&website).await?;

        tracing::info!(website = %website.id, name = %website.name, "website added");
        Ok(website)
    }

    pub async fn delete_website(&self, id: WebsiteId) -> Result<(), ProvisioningError> {
        self.websites.delete(id).await?;
        tracing::info!(website = %id, "website removed");
        Ok(())
    }

    /// Writes the credential material for a new connection.
    async fn store_credential(
        &self,
        reference: CredentialRef,
        connection: &NewConnection,
    ) -> Result<(), ProvisioningError> {
        match connection {
            NewConnection::Ssh {
                auth_kind,
                secret,
                passphrase,
                ..
            } => {
                if secret.expose().is_empty() {
                    return Err(ProvisioningError::MissingCredential);
                }

                let kind = match auth_kind {
                    SshAuthKind::Password => SecretKind::SshPassword,
                    SshAuthKind::PrivateKey | SshAuthKind::EncryptedPrivateKey => {
                        SecretKind::SshPrivateKey
                    }
                };
                self.secrets.store(reference, kind, secret.clone()).await?;

                if *auth_kind == SshAuthKind::EncryptedPrivateKey {
                    let passphrase = passphrase
                        .as_ref()
                        .filter(|p| !p.expose().is_empty())
                        .ok_or(ProvisioningError::MissingCredential)?;
                    self.secrets
                        .store(reference, SecretKind::SshKeyPassphrase, passphrase.clone())
                        .await?;
                }
                Ok(())
            }
            NewConnection::Agent { token, .. } => {
                if token.expose().is_empty() {
                    return Err(ProvisioningError::MissingCredential);
                }
                self.secrets
                    .store(reference, SecretKind::AgentToken, token.clone())
                    .await?;
                Ok(())
            }
        }
    }
}

fn connection_settings(connection: &NewConnection, reference: CredentialRef) -> ConnectionSettings {
    match connection {
        NewConnection::Ssh {
            username,
            auth_kind,
            ..
        } => ConnectionSettings::Ssh(SshSettings {
            username: username.trim().to_owned(),
            auth_kind: *auth_kind,
            credential_ref: reference,
        }),
        NewConnection::Agent { port, .. } => ConnectionSettings::Agent(AgentSettings {
            port: *port,
            credential_ref: reference,
            // Pinned on the first successful connection, not here: pinning a fingerprint
            // nobody has seen would defeat the point of pinning it.
            certificate_fingerprint: None,
        }),
    }
}

/// Adds a scheme when the user did not type one.
///
/// "example.com" is what people type, and rejecting it as a malformed URL is a poor
/// welcome. `https` rather than `http`, because defaulting to the insecure one in a tool
/// that reports on certificates would be an odd choice.
///
/// A string that already carries *any* scheme is left exactly as it is, even one this
/// application cannot monitor. Prepending to `ftp://files.example` would produce
/// `https://ftp://files.example`, which parses — the host becomes `ftp` — and would be
/// accepted as a website that can never resolve. Left alone, validation rejects it with
/// "only http and https are monitored", which is the truth and is actionable.
fn normalise_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains("://") {
        return trimmed.to_owned();
    }
    format!("https://{trimmed}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeSecretStore, FakeServerRepository, FakeWebsiteRepository};
    use chrono::{DateTime, Utc};
    use vds_domain::ports::FixedClock;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).unwrap_or_default()
    }

    struct Harness {
        service: ProvisioningService,
        servers: Arc<FakeServerRepository>,
        websites: Arc<FakeWebsiteRepository>,
        secrets: Arc<FakeSecretStore>,
    }

    fn harness() -> Harness {
        let servers = Arc::new(FakeServerRepository::new());
        let websites = Arc::new(FakeWebsiteRepository::new());
        let secrets = Arc::new(FakeSecretStore::new());

        let service = ProvisioningService::new(
            Arc::clone(&servers) as Arc<dyn ServerRepository>,
            Arc::clone(&websites) as Arc<dyn WebsiteRepository>,
            Arc::clone(&secrets) as Arc<dyn SecretStore>,
            Arc::new(FixedClock::new(at(1_000))),
        );

        Harness {
            service,
            servers,
            websites,
            secrets,
        }
    }

    fn ssh_server(name: &str) -> NewServer {
        NewServer {
            name: name.to_owned(),
            host: "10.0.0.5".to_owned(),
            port: 22,
            connection: NewConnection::Ssh {
                username: "vds-monitor".to_owned(),
                auth_kind: SshAuthKind::Password,
                secret: Secret::from_string("hunter2".to_owned()),
                passphrase: None,
            },
            poll_interval_secs: 30,
            tags: Vec::new(),
        }
    }

    #[tokio::test]
    async fn adding_a_server_stores_the_password_in_the_secret_store_and_a_handle_in_the_row() {
        let h = harness();
        let server = h
            .service
            .create_server(ssh_server("prod-01"))
            .await
            .expect("created");

        assert_eq!(server.name, "prod-01");
        assert!(h.servers.get(server.id).await.is_ok());

        // The password is in the store, under the handle the server carries.
        let reference = server.connection.credential_ref();
        assert!(
            h.secrets
                .contains(reference, SecretKind::SshPassword)
                .await
                .expect("checked")
        );

        // And nothing resembling the password is in the entity itself.
        let rendered = format!("{server:?}");
        assert!(
            !rendered.contains("hunter2"),
            "the server carried the password: {rendered}"
        );
    }

    #[tokio::test]
    async fn a_private_key_is_stored_under_its_own_kind() {
        let h = harness();
        let new = NewServer {
            connection: NewConnection::Ssh {
                username: "root".to_owned(),
                auth_kind: SshAuthKind::PrivateKey,
                secret: Secret::from_string("-----BEGIN OPENSSH PRIVATE KEY-----".to_owned()),
                passphrase: None,
            },
            ..ssh_server("prod-02")
        };

        let server = h.service.create_server(new).await.expect("created");
        let reference = server.connection.credential_ref();
        assert!(
            h.secrets
                .contains(reference, SecretKind::SshPrivateKey)
                .await
                .expect("checked")
        );
        assert!(
            !h.secrets
                .contains(reference, SecretKind::SshPassword)
                .await
                .expect("checked")
        );
    }

    #[tokio::test]
    async fn an_encrypted_key_stores_the_passphrase_beside_it() {
        let h = harness();
        let new = NewServer {
            connection: NewConnection::Ssh {
                username: "root".to_owned(),
                auth_kind: SshAuthKind::EncryptedPrivateKey,
                secret: Secret::from_string("-----BEGIN OPENSSH PRIVATE KEY-----".to_owned()),
                passphrase: Some(Secret::from_string("s3cret".to_owned())),
            },
            ..ssh_server("prod-03")
        };

        let server = h.service.create_server(new).await.expect("created");
        let reference = server.connection.credential_ref();
        assert!(
            h.secrets
                .contains(reference, SecretKind::SshPrivateKey)
                .await
                .expect("checked")
        );
        assert!(
            h.secrets
                .contains(reference, SecretKind::SshKeyPassphrase)
                .await
                .expect("checked")
        );
    }

    #[tokio::test]
    async fn an_encrypted_key_without_a_passphrase_is_refused() {
        let h = harness();
        let new = NewServer {
            connection: NewConnection::Ssh {
                username: "root".to_owned(),
                auth_kind: SshAuthKind::EncryptedPrivateKey,
                secret: Secret::from_string("key".to_owned()),
                passphrase: None,
            },
            ..ssh_server("prod-04")
        };

        assert!(matches!(
            h.service.create_server(new).await,
            Err(ProvisioningError::MissingCredential)
        ));
    }

    #[tokio::test]
    async fn an_agent_server_stores_a_token_and_pins_nothing_yet() {
        // Pinning a fingerprint nobody has seen would defeat the point of pinning it.
        let h = harness();
        let new = NewServer {
            connection: NewConnection::Agent {
                port: 9443,
                token: Secret::from_string("0123456789abcdef0123456789abcdef".to_owned()),
            },
            ..ssh_server("prod-05")
        };

        let server = h.service.create_server(new).await.expect("created");
        match &server.connection {
            ConnectionSettings::Agent(settings) => {
                assert_eq!(settings.port, 9443);
                assert_eq!(settings.certificate_fingerprint, None);
            }
            other => panic!("expected agent settings, got {other:?}"),
        }
        assert!(
            h.secrets
                .contains(server.connection.credential_ref(), SecretKind::AgentToken)
                .await
                .expect("checked")
        );
    }

    #[tokio::test]
    async fn an_empty_credential_is_refused_before_anything_is_stored() {
        let h = harness();
        let new = NewServer {
            connection: NewConnection::Ssh {
                username: "root".to_owned(),
                auth_kind: SshAuthKind::Password,
                secret: Secret::from_string(String::new()),
                passphrase: None,
            },
            ..ssh_server("prod-06")
        };

        assert!(matches!(
            h.service.create_server(new).await,
            Err(ProvisioningError::MissingCredential)
        ));
        assert_eq!(h.servers.count(), 0);
        assert_eq!(h.secrets.len(), 0);
    }

    #[tokio::test]
    async fn an_invalid_server_is_refused_before_the_secret_is_written() {
        // Otherwise a mistyped form leaves an orphaned password in the user's keystore.
        let h = harness();
        let new = NewServer {
            name: "   ".to_owned(),
            ..ssh_server("ignored")
        };

        assert!(matches!(
            h.service.create_server(new).await,
            Err(ProvisioningError::InvalidServer(
                ServerValidationError::EmptyName
            ))
        ));
        assert_eq!(
            h.secrets.len(),
            0,
            "a rejected form must leave no secret behind"
        );
        assert_eq!(h.servers.count(), 0);
    }

    #[tokio::test]
    async fn a_failed_save_rolls_the_credential_back() {
        // The case that is easy to leave out: the secret is written, then the row fails.
        let h = harness();
        h.servers.fail_next_save();

        let result = h.service.create_server(ssh_server("prod-07")).await;
        assert!(result.is_err());
        assert_eq!(
            h.secrets.len(),
            0,
            "an unreachable credential was left in the keystore"
        );
    }

    #[tokio::test]
    async fn deleting_a_server_removes_its_credential_too() {
        let h = harness();
        let server = h
            .service
            .create_server(ssh_server("prod-08"))
            .await
            .expect("created");
        let reference = server.connection.credential_ref();
        assert_eq!(h.secrets.len(), 1);

        h.service.delete_server(server.id).await.expect("deleted");

        assert!(h.servers.get(server.id).await.is_err());
        assert!(
            !h.secrets
                .contains(reference, SecretKind::SshPassword)
                .await
                .expect("checked")
        );
    }

    #[tokio::test]
    async fn a_website_gets_a_scheme_when_the_user_did_not_type_one() {
        // "example.com" is what people type; rejecting it is a poor welcome.
        let h = harness();
        let website = h
            .service
            .create_website(NewWebsite {
                name: "Example".to_owned(),
                url: "example.com".to_owned(),
                server_id: None,
                poll_interval_secs: 60,
                expected_status: 200,
                expected_text: None,
            })
            .await
            .expect("created");

        assert_eq!(website.url, "https://example.com");
        assert!(website.is_https());
    }

    #[tokio::test]
    async fn an_explicit_scheme_is_left_alone() {
        let h = harness();
        let website = h
            .service
            .create_website(NewWebsite {
                name: "Insecure".to_owned(),
                url: "http://legacy.example/health".to_owned(),
                server_id: None,
                poll_interval_secs: 60,
                expected_status: 200,
                expected_text: None,
            })
            .await
            .expect("created");

        assert_eq!(website.url, "http://legacy.example/health");
    }

    #[tokio::test]
    async fn an_empty_expected_text_becomes_no_expectation_rather_than_an_empty_match() {
        // An empty `body_contains` would match every response, which looks like a check
        // that passes and is really a check that does nothing.
        let h = harness();
        let website = h
            .service
            .create_website(NewWebsite {
                name: "Example".to_owned(),
                url: "https://example.com".to_owned(),
                server_id: None,
                poll_interval_secs: 60,
                expected_status: 200,
                expected_text: Some("   ".to_owned()),
            })
            .await
            .expect("created");

        assert_eq!(website.expectation.body_contains, None);
    }

    #[tokio::test]
    async fn a_malformed_url_is_refused() {
        let h = harness();
        let result = h
            .service
            .create_website(NewWebsite {
                name: "Broken".to_owned(),
                url: "https://".to_owned(),
                server_id: None,
                poll_interval_secs: 60,
                expected_status: 200,
                expected_text: None,
            })
            .await;

        assert!(matches!(result, Err(ProvisioningError::InvalidWebsite(_))));
        assert_eq!(h.websites.count(), 0);
    }

    #[tokio::test]
    async fn an_unmonitorable_scheme_is_named_rather_than_mangled() {
        // Prepending https:// here would produce `https://ftp://files.example`, which
        // parses — the host becomes `ftp` — and would be accepted as a site that can
        // never resolve.
        let h = harness();
        let err = h
            .service
            .create_website(NewWebsite {
                name: "Files".to_owned(),
                url: "ftp://files.example".to_owned(),
                server_id: None,
                poll_interval_secs: 60,
                expected_status: 200,
                expected_text: None,
            })
            .await
            .expect_err("must refuse");

        let message = err.message();
        assert!(
            message.contains("ftp"),
            "did not name the scheme: {message}"
        );
        assert_eq!(h.websites.count(), 0);
    }

    #[tokio::test]
    async fn an_out_of_range_expected_status_is_refused() {
        let h = harness();
        let result = h
            .service
            .create_website(NewWebsite {
                name: "Example".to_owned(),
                url: "https://example.com".to_owned(),
                server_id: None,
                poll_interval_secs: 60,
                expected_status: 999,
                expected_text: None,
            })
            .await;

        assert!(matches!(result, Err(ProvisioningError::InvalidWebsite(_))));
    }

    #[tokio::test]
    async fn the_error_message_is_a_sentence_a_user_can_act_on() {
        let h = harness();
        let err = h
            .service
            .create_website(NewWebsite {
                name: String::new(),
                url: "https://example.com".to_owned(),
                server_id: None,
                poll_interval_secs: 60,
                expected_status: 200,
                expected_text: None,
            })
            .await
            .expect_err("must refuse");

        let message = err.message();
        assert!(message.contains("name"), "unhelpful message: {message}");
        assert!(!message.contains("Err("), "raw debug leaked: {message}");
    }

    #[test]
    fn the_debug_rendering_of_a_form_never_contains_the_credential() {
        // These travel through the intent queue, which is logged at trace level.
        let new = ssh_server("prod-09");
        let rendered = format!("{new:?}");

        assert!(
            !rendered.contains("hunter2"),
            "the password leaked: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        // Still useful for diagnosing anything else.
        assert!(rendered.contains("prod-09"));
        assert!(rendered.contains("vds-monitor"));
    }

    #[test]
    fn the_same_holds_for_an_agent_token() {
        let new = NewServer {
            connection: NewConnection::Agent {
                port: 9443,
                token: Secret::from_string("0123456789abcdef0123456789abcdef".to_owned()),
            },
            ..ssh_server("prod-10")
        };

        let rendered = format!("{new:?}");
        assert!(
            !rendered.contains("0123456789abcdef"),
            "the token leaked: {rendered}"
        );
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("9443"), "the port is not a secret");
    }
}
