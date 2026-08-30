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
use vds_domain::analytics::AnalyticsIntegration;
use vds_domain::ids::ProviderId;
use vds_domain::ids::{CredentialRef, ServerId, WebsiteId};
use vds_domain::ports::{
    AnalyticsRepository, Clock, RepositoryError, Secret, SecretKind, SecretStore, SecretStoreError,
    ServerRepository, WebsiteRepository,
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
    #[error("enter the counter number")]
    EmptyCounter,
    #[error("a counter number is digits only")]
    MalformedCounter,
    #[error("save the analytics token before connecting a counter")]
    MissingAnalyticsToken,
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

/// What the "edit server" form collected.
///
/// Separate from [`NewServer`] because of one field: the secret is optional. Changing a
/// polling interval must not require re-pasting a seven-line private key, so `None`
/// means "keep what is stored". That distinction does not exist when adding, and folding
/// the two together would make it possible to create a server with no credential at all.
#[derive(Clone)]
pub struct ServerEdit {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub connection: ConnectionEdit,
    pub poll_interval_secs: u32,
    pub enabled: bool,
    pub tags: Vec<String>,
}

/// The mode-specific half of an edit. `None` secrets keep the stored ones.
#[derive(Clone)]
pub enum ConnectionEdit {
    Ssh {
        username: String,
        auth_kind: SshAuthKind,
        /// The password or private key. `None` keeps the stored one.
        secret: Option<Secret>,
        passphrase: Option<Secret>,
    },
    Agent {
        port: u16,
        /// `None` keeps the stored token.
        token: Option<Secret>,
    },
}

/// Prints the shape without the material; see [`NewConnection`]'s implementation.
impl std::fmt::Debug for ServerEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerEdit")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("connection", &self.connection)
            .field("poll_interval_secs", &self.poll_interval_secs)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl std::fmt::Debug for ConnectionEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionEdit::Ssh {
                username,
                auth_kind,
                secret,
                ..
            } => f
                .debug_struct("Ssh")
                .field("username", username)
                .field("auth_kind", auth_kind)
                .field(
                    "secret",
                    &if secret.is_some() {
                        "<replaced>"
                    } else {
                        "<kept>"
                    },
                )
                .finish(),
            ConnectionEdit::Agent { port, token } => f
                .debug_struct("Agent")
                .field("port", port)
                .field(
                    "token",
                    &if token.is_some() {
                        "<replaced>"
                    } else {
                        "<kept>"
                    },
                )
                .finish(),
        }
    }
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

/// The credential that holds the analytics OAuth token.
///
/// Fixed rather than random, and this is the whole design decision: one Yandex account's
/// token authorises every counter that account can see, so every website's integration
/// points at the *same* secret. A stable reference is what lets the token be entered once,
/// in settings, before any website has been connected — and what stops a second website
/// from needing it again.
///
/// It is a well-known key in the OS keystore, the same way an application names its own
/// keychain entry. Version 4 UUID shape so it cannot collide with a generated one.
const ANALYTICS_CREDENTIAL: &str = "a1a1a1a1-0000-4000-8000-000000000001";

/// Creates and removes servers, websites and analytics integrations.
pub struct ProvisioningService {
    servers: Arc<dyn ServerRepository>,
    websites: Arc<dyn WebsiteRepository>,
    analytics: Arc<dyn AnalyticsRepository>,
    secrets: Arc<dyn SecretStore>,
    clock: Arc<dyn Clock>,
}

impl ProvisioningService {
    pub fn new(
        servers: Arc<dyn ServerRepository>,
        websites: Arc<dyn WebsiteRepository>,
        analytics: Arc<dyn AnalyticsRepository>,
        secrets: Arc<dyn SecretStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            servers,
            websites,
            analytics,
            secrets,
            clock,
        }
    }

    /// The handle under which the analytics token is kept.
    fn analytics_credential() -> CredentialRef {
        // The constant is a literal this crate controls, so a parse failure is a
        // programming error rather than anything a user can cause. Falling back to a
        // fresh handle keeps it from being a panic; the token would then simply need
        // entering again, which is recoverable.
        CredentialRef::parse(ANALYTICS_CREDENTIAL).unwrap_or_else(|_| CredentialRef::new())
    }

    /// Saves the analytics OAuth token.
    ///
    /// Entered once and shared by every counter, so this is deliberately separate from
    /// connecting an individual website.
    pub async fn save_analytics_token(&self, token: Secret) -> Result<(), ProvisioningError> {
        // `expose` yields bytes, and an all-whitespace token is as useless as an empty
        // one: it is what a stray paste of a blank line produces.
        if token.expose().iter().all(|byte| byte.is_ascii_whitespace()) {
            return Err(ProvisioningError::MissingCredential);
        }

        self.secrets
            .store(
                Self::analytics_credential(),
                SecretKind::AnalyticsToken,
                token,
            )
            .await
            .map_err(ProvisioningError::Secrets)
    }

    /// Whether a token has been saved.
    ///
    /// Used by the interface to say "enter the token first" rather than letting someone
    /// connect a counter that cannot be read.
    pub async fn has_analytics_token(&self) -> bool {
        self.secrets
            .contains(Self::analytics_credential(), SecretKind::AnalyticsToken)
            .await
            .unwrap_or(false)
    }

    /// Points a website at a provider's counter.
    ///
    /// Replaces any existing integration for the same website and provider rather than
    /// adding a second one: a website has one counter per provider, and silently
    /// accumulating duplicates would double every figure on the dashboard.
    pub async fn connect_analytics(
        &self,
        website_id: WebsiteId,
        provider: ProviderId,
        counter: &str,
    ) -> Result<AnalyticsIntegration, ProvisioningError> {
        let counter = counter.trim();
        if counter.is_empty() {
            return Err(ProvisioningError::EmptyCounter);
        }
        // Every provider this targets identifies a counter numerically. Catching a pasted
        // URL or a stray space here gives a clear message instead of an authentication
        // failure hours later.
        if !counter.chars().all(|c| c.is_ascii_digit()) {
            return Err(ProvisioningError::MalformedCounter);
        }

        if !self.has_analytics_token().await {
            return Err(ProvisioningError::MissingAnalyticsToken);
        }

        // Confirms the website exists before writing anything that references it.
        self.websites
            .get(website_id)
            .await
            .map_err(ProvisioningError::Repository)?;

        let existing = self
            .analytics
            .list_integrations_for_website(website_id)
            .await
            .map_err(ProvisioningError::Repository)?
            .into_iter()
            .find(|i| i.provider == provider);

        let integration = match existing {
            Some(mut integration) => {
                integration.external_id = counter.to_owned();
                integration.credential_ref = Self::analytics_credential();
                integration.enabled = true;
                integration
            }
            None => AnalyticsIntegration::new(
                website_id,
                provider,
                counter,
                Self::analytics_credential(),
                self.clock.now(),
            ),
        };

        self.analytics
            .save_integration(&integration)
            .await
            .map_err(ProvisioningError::Repository)?;

        Ok(integration)
    }

    /// Removes a website's integration with a provider.
    ///
    /// The shared token is left alone: other websites are still using it.
    pub async fn disconnect_analytics(
        &self,
        website_id: WebsiteId,
        provider: &ProviderId,
    ) -> Result<(), ProvisioningError> {
        let integrations = self
            .analytics
            .list_integrations_for_website(website_id)
            .await
            .map_err(ProvisioningError::Repository)?;

        for integration in integrations.iter().filter(|i| i.provider == *provider) {
            self.analytics
                .delete_integration(integration.id)
                .await
                .map_err(ProvisioningError::Repository)?;
        }
        Ok(())
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
    /// Changes a server in place.
    ///
    /// The identifier is preserved, which is the entire point: it is what the metric
    /// history, the incidents and the events are keyed by. Deleting and re-adding — the
    /// only option before this existed — silently threw all of that away.
    pub async fn update_server(
        &self,
        id: ServerId,
        edit: ServerEdit,
    ) -> Result<Server, ProvisioningError> {
        let existing = self.servers.get(id).await?;
        let reference = existing.connection.credential_ref();

        // A credential is only replaced when one was typed. Keeping the handle rather
        // than issuing a new one means an unchanged secret is never rewritten, and a
        // failure below cannot leave the server pointing at a handle with nothing behind
        // it.
        let mut server = existing.clone();
        server.name = edit.name.trim().to_owned();
        server.host = edit.host.trim().to_owned();
        server.port = edit.port;
        server.poll_interval_secs = edit.poll_interval_secs;
        server.enabled = edit.enabled;
        server.tags = edit.tags;
        server.connection = connection_from_edit(&edit.connection, reference);

        server.validate()?;

        // Changing the authentication method without supplying a new secret would leave
        // the stored one under the wrong kind — a password read as a private key. Caught
        // here rather than at the next collection.
        if let (
            ConnectionEdit::Ssh {
                auth_kind, secret, ..
            },
            ConnectionSettings::Ssh(previous),
        ) = (&edit.connection, &existing.connection)
            && *auth_kind != previous.auth_kind
            && secret.is_none()
        {
            return Err(ProvisioningError::MissingCredential);
        }

        // Likewise for a mode change: an agent token cannot be read as an SSH key.
        if edit.connection.mode() != existing.connection.mode() && !edit.connection.carries_secret()
        {
            return Err(ProvisioningError::MissingCredential);
        }

        self.store_edited_credential(reference, &edit.connection, &existing)
            .await?;

        self.servers.save(&server).await?;

        tracing::info!(server = %server.id, name = %server.name, "server updated");
        Ok(server)
    }

    /// Writes whichever secrets the edit actually supplied.
    async fn store_edited_credential(
        &self,
        reference: CredentialRef,
        edit: &ConnectionEdit,
        existing: &Server,
    ) -> Result<(), ProvisioningError> {
        match edit {
            ConnectionEdit::Ssh {
                auth_kind,
                secret,
                passphrase,
                ..
            } => {
                if let Some(secret) = secret.clone() {
                    let kind = match auth_kind {
                        SshAuthKind::Password => SecretKind::SshPassword,
                        SshAuthKind::PrivateKey | SshAuthKind::EncryptedPrivateKey => {
                            SecretKind::SshPrivateKey
                        }
                    };
                    self.secrets.store(reference, kind, secret).await?;
                }
                if let Some(passphrase) = passphrase.clone() {
                    self.secrets
                        .store(reference, SecretKind::SshKeyPassphrase, passphrase)
                        .await?;
                }
                // Moving away from an encrypted key leaves a passphrase behind that
                // nothing will ever read. Removed so the keystore does not accumulate
                // secrets whose owner has forgotten them.
                if *auth_kind != SshAuthKind::EncryptedPrivateKey
                    && let Err(err) = self
                        .secrets
                        .delete(reference, SecretKind::SshKeyPassphrase)
                        .await
                {
                    tracing::debug!(error = %err, "no passphrase to remove");
                }
                let _ = existing;
                Ok(())
            }
            ConnectionEdit::Agent { token, .. } => {
                if let Some(token) = token.clone() {
                    self.secrets
                        .store(reference, SecretKind::AgentToken, token)
                        .await?;
                }
                Ok(())
            }
        }
    }

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

    /// Changes a website in place, keeping its identifier.
    ///
    /// That identifier is what the availability history, the analytics integration and
    /// the screenshot are keyed by — so fixing a typo in a URL no longer costs all three.
    pub async fn update_website(
        &self,
        id: WebsiteId,
        edit: NewWebsite,
    ) -> Result<Website, ProvisioningError> {
        let existing = self.websites.get(id).await?;

        // Built through the constructor so the URL goes through the same normalisation
        // and validation as a new one — a scheme-less address gains https:// here too.
        let mut website = Website::new(
            edit.name.trim(),
            // The same normalisation creation uses, so a scheme-less address gains
            // https:// whichever way it was entered.
            normalise_url(&edit.url),
            existing.created_at,
        );
        website.id = existing.id;
        website.server_id = edit.server_id;
        website.poll_interval_secs = edit.poll_interval_secs;
        website.enabled = existing.enabled;
        website.expectation.status = edit.expected_status;
        website.expectation.body_contains = edit
            .expected_text
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());

        website.validate()?;
        self.websites.save(&website).await?;

        tracing::info!(website = %website.id, name = %website.name, "website updated");
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

/// The same, for an edit: the credential handle is the one the server already has.
fn connection_from_edit(edit: &ConnectionEdit, reference: CredentialRef) -> ConnectionSettings {
    match edit {
        ConnectionEdit::Ssh {
            username,
            auth_kind,
            ..
        } => ConnectionSettings::Ssh(SshSettings {
            username: username.trim().to_owned(),
            auth_kind: *auth_kind,
            credential_ref: reference,
        }),
        ConnectionEdit::Agent { port, .. } => ConnectionSettings::Agent(AgentSettings {
            port: *port,
            credential_ref: reference,
            // Cleared deliberately: the address or the port may now point at a different
            // machine, and silently keeping the old pin would defeat the check.
            certificate_fingerprint: None,
        }),
    }
}

impl ConnectionEdit {
    fn mode(&self) -> vds_domain::server::ConnectionMode {
        match self {
            ConnectionEdit::Ssh { .. } => vds_domain::server::ConnectionMode::Ssh,
            ConnectionEdit::Agent { .. } => vds_domain::server::ConnectionMode::Agent,
        }
    }

    /// Whether this edit supplies a secret rather than keeping the stored one.
    fn carries_secret(&self) -> bool {
        match self {
            ConnectionEdit::Ssh { secret, .. } => secret.is_some(),
            ConnectionEdit::Agent { token, .. } => token.is_some(),
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
    use crate::testing::{
        FakeAnalyticsRepository, FakeSecretStore, FakeServerRepository, FakeWebsiteRepository,
    };
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
        analytics: Arc<FakeAnalyticsRepository>,
    }

    fn harness() -> Harness {
        let servers = Arc::new(FakeServerRepository::new());
        let websites = Arc::new(FakeWebsiteRepository::new());
        let secrets = Arc::new(FakeSecretStore::new());
        let analytics = Arc::new(FakeAnalyticsRepository::new());

        let service = ProvisioningService::new(
            Arc::clone(&servers) as Arc<dyn ServerRepository>,
            Arc::clone(&websites) as Arc<dyn WebsiteRepository>,
            Arc::clone(&analytics) as Arc<dyn AnalyticsRepository>,
            Arc::clone(&secrets) as Arc<dyn SecretStore>,
            Arc::new(FixedClock::new(at(1_000))),
        );

        Harness {
            service,
            servers,
            websites,
            secrets,
            analytics,
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

    // --- analytics -------------------------------------------------------------------

    fn yandex() -> ProviderId {
        ProviderId::new("yandex_metrica")
    }

    async fn a_website(service: &ProvisioningService) -> WebsiteId {
        service
            .create_website(NewWebsite {
                name: "Example".into(),
                url: "https://example.com/".into(),
                poll_interval_secs: 60,
                expected_status: 200,
                expected_text: None,
                server_id: None,
            })
            .await
            .expect("website created")
            .id
    }

    #[tokio::test]
    async fn a_counter_cannot_be_connected_before_the_token_is_saved() {
        // Otherwise the integration exists, the scheduler starts polling it, and every
        // refresh fails authentication — which reads to the user as "Metrica is broken".
        let h = harness();
        let website = a_website(&h.service).await;

        let err = h
            .service
            .connect_analytics(website, yandex(), "54028423")
            .await
            .expect_err("must refuse");
        assert!(matches!(err, ProvisioningError::MissingAnalyticsToken));
    }

    #[tokio::test]
    async fn saving_the_token_then_connecting_a_counter_works() {
        let h = harness();
        let website = a_website(&h.service).await;

        h.service
            .save_analytics_token(Secret::from_string("oauth-token".to_owned()))
            .await
            .expect("token saved");
        assert!(h.service.has_analytics_token().await);

        let integration = h
            .service
            .connect_analytics(website, yandex(), "54028423")
            .await
            .expect("connected");

        assert_eq!(integration.external_id, "54028423");
        assert_eq!(integration.website_id, website);
        assert!(integration.enabled);
    }

    #[tokio::test]
    async fn every_website_shares_one_token_and_keeps_its_own_counter() {
        // The point of the whole design: a Yandex account's token covers every counter it
        // can see, so it is entered once and each site only needs its number.
        let h = harness();
        h.service
            .save_analytics_token(Secret::from_string("oauth-token".to_owned()))
            .await
            .expect("token saved");

        let first = a_website(&h.service).await;
        let second = a_website(&h.service).await;

        let a = h
            .service
            .connect_analytics(first, yandex(), "11111111")
            .await
            .expect("connected");
        let b = h
            .service
            .connect_analytics(second, yandex(), "22222222")
            .await
            .expect("connected");

        assert_ne!(a.external_id, b.external_id, "counters are per website");
        assert_eq!(a.credential_ref, b.credential_ref, "the token is shared");
    }

    #[tokio::test]
    async fn reconnecting_replaces_the_counter_rather_than_adding_a_second() {
        // Two integrations for one website would double every figure on the dashboard.
        let h = harness();
        h.service
            .save_analytics_token(Secret::from_string("oauth-token".to_owned()))
            .await
            .expect("token saved");
        let website = a_website(&h.service).await;

        h.service
            .connect_analytics(website, yandex(), "11111111")
            .await
            .expect("connected");
        h.service
            .connect_analytics(website, yandex(), "22222222")
            .await
            .expect("reconnected");

        let integrations = h
            .analytics
            .list_integrations_for_website(website)
            .await
            .expect("listed");
        assert_eq!(integrations.len(), 1, "a second integration was created");
        assert_eq!(integrations[0].external_id, "22222222");
    }

    #[tokio::test]
    async fn a_counter_that_is_not_a_number_is_refused_with_a_clear_reason() {
        // Pasting the counter's URL is the mistake this catches. Without it the failure
        // surfaces hours later as an authentication error.
        let h = harness();
        h.service
            .save_analytics_token(Secret::from_string("oauth-token".to_owned()))
            .await
            .expect("token saved");
        let website = a_website(&h.service).await;

        for bad in [
            "https://metrika.yandex.ru/dashboard?id=54028423",
            "540 284",
            "abc",
        ] {
            let err = h
                .service
                .connect_analytics(website, yandex(), bad)
                .await
                .expect_err("must refuse");
            assert!(
                matches!(err, ProvisioningError::MalformedCounter),
                "accepted {bad}"
            );
        }
    }

    #[tokio::test]
    async fn an_empty_counter_is_refused_separately_from_a_malformed_one() {
        let h = harness();
        h.service
            .save_analytics_token(Secret::from_string("oauth-token".to_owned()))
            .await
            .expect("token saved");
        let website = a_website(&h.service).await;

        let err = h
            .service
            .connect_analytics(website, yandex(), "   ")
            .await
            .expect_err("must refuse");
        assert!(matches!(err, ProvisioningError::EmptyCounter));
    }

    #[tokio::test]
    async fn surrounding_whitespace_on_a_counter_is_forgiven() {
        // Copying a number out of the Metrica interface brings a space with it.
        let h = harness();
        h.service
            .save_analytics_token(Secret::from_string("oauth-token".to_owned()))
            .await
            .expect("token saved");
        let website = a_website(&h.service).await;

        let integration = h
            .service
            .connect_analytics(website, yandex(), "  54028423 ")
            .await
            .expect("connected");
        assert_eq!(integration.external_id, "54028423");
    }

    #[tokio::test]
    async fn an_empty_token_is_refused() {
        let h = harness();
        let err = h
            .service
            .save_analytics_token(Secret::from_string("   ".to_owned()))
            .await
            .expect_err("must refuse");
        assert!(matches!(err, ProvisioningError::MissingCredential));
        assert!(!h.service.has_analytics_token().await);
    }

    #[tokio::test]
    async fn disconnecting_removes_the_integration_but_keeps_the_shared_token() {
        // Other websites are still using it.
        let h = harness();
        h.service
            .save_analytics_token(Secret::from_string("oauth-token".to_owned()))
            .await
            .expect("token saved");
        let website = a_website(&h.service).await;
        h.service
            .connect_analytics(website, yandex(), "54028423")
            .await
            .expect("connected");

        h.service
            .disconnect_analytics(website, &yandex())
            .await
            .expect("disconnected");

        assert!(
            h.analytics
                .list_integrations_for_website(website)
                .await
                .expect("listed")
                .is_empty()
        );
        assert!(
            h.service.has_analytics_token().await,
            "the token was removed too"
        );
    }

    #[tokio::test]
    async fn connecting_a_website_that_does_not_exist_is_refused() {
        let h = harness();
        h.service
            .save_analytics_token(Secret::from_string("oauth-token".to_owned()))
            .await
            .expect("token saved");

        assert!(
            h.service
                .connect_analytics(WebsiteId::new(), yandex(), "54028423")
                .await
                .is_err()
        );
    }

    // --- editing ---------------------------------------------------------------------

    fn ssh_edit(name: &str) -> ServerEdit {
        ServerEdit {
            name: name.to_owned(),
            host: "10.0.0.1".to_owned(),
            port: 22,
            connection: ConnectionEdit::Ssh {
                // Matches what  creates: an edit that changes the method
                // without supplying a secret is refused, which is its own test below.
                username: "vds-monitor".to_owned(),
                auth_kind: SshAuthKind::Password,
                secret: None,
                passphrase: None,
            },
            poll_interval_secs: 30,
            enabled: true,
            tags: Vec::new(),
        }
    }

    #[tokio::test]
    async fn editing_keeps_the_identifier_and_therefore_the_history() {
        // The whole reason this exists. Delete-and-re-add — the only option before —
        // threw away every metric, incident and event keyed by this id.
        let h = harness();
        let created = h
            .service
            .create_server(ssh_server("web-01"))
            .await
            .expect("created");

        let mut edit = ssh_edit("web-01-renamed");
        edit.poll_interval_secs = 120;
        let updated = h
            .service
            .update_server(created.id, edit)
            .await
            .expect("updated");

        assert_eq!(updated.id, created.id, "the identifier changed");
        assert_eq!(updated.name, "web-01-renamed");
        assert_eq!(updated.poll_interval_secs, 120);
    }

    #[tokio::test]
    async fn an_edit_without_a_new_secret_keeps_the_stored_one() {
        // Changing a polling interval must not require re-pasting a private key.
        let h = harness();
        let created = h
            .service
            .create_server(ssh_server("web-01"))
            .await
            .expect("created");
        let reference = created.connection.credential_ref();
        let before = h
            .secrets
            .retrieve(reference, SecretKind::SshPassword)
            .await
            .expect("stored");

        h.service
            .update_server(created.id, ssh_edit("web-01"))
            .await
            .expect("updated");

        let after = h
            .secrets
            .retrieve(reference, SecretKind::SshPassword)
            .await
            .expect("still stored");
        assert_eq!(before.expose(), after.expose(), "the key was disturbed");
    }

    #[tokio::test]
    async fn supplying_a_new_secret_replaces_the_stored_one() {
        let h = harness();
        let created = h
            .service
            .create_server(ssh_server("web-01"))
            .await
            .expect("created");
        let reference = created.connection.credential_ref();

        let mut edit = ssh_edit("web-01");
        edit.connection = ConnectionEdit::Ssh {
            username: "vds-monitor".to_owned(),
            auth_kind: SshAuthKind::Password,
            secret: Some(Secret::from_string("a-new-key".to_owned())),
            passphrase: None,
        };
        h.service
            .update_server(created.id, edit)
            .await
            .expect("updated");

        let stored = h
            .secrets
            .retrieve(reference, SecretKind::SshPassword)
            .await
            .expect("stored");
        assert_eq!(stored.expose(), b"a-new-key");
    }

    #[tokio::test]
    async fn changing_the_authentication_method_requires_a_new_secret() {
        // Otherwise the stored password would be read back as a private key, and the
        // failure would surface at the next collection as an unexplained auth error.
        let h = harness();
        let created = h
            .service
            .create_server(ssh_server("web-01"))
            .await
            .expect("created");

        let mut edit = ssh_edit("web-01");
        edit.connection = ConnectionEdit::Ssh {
            username: "vds-monitor".to_owned(),
            auth_kind: SshAuthKind::PrivateKey,
            secret: None,
            passphrase: None,
        };

        let err = h
            .service
            .update_server(created.id, edit)
            .await
            .expect_err("must refuse");
        assert!(matches!(err, ProvisioningError::MissingCredential));
    }

    #[tokio::test]
    async fn switching_to_agent_mode_requires_a_token() {
        let h = harness();
        let created = h
            .service
            .create_server(ssh_server("web-01"))
            .await
            .expect("created");

        let mut edit = ssh_edit("web-01");
        edit.connection = ConnectionEdit::Agent {
            port: 9443,
            token: None,
        };

        assert!(matches!(
            h.service.update_server(created.id, edit).await,
            Err(ProvisioningError::MissingCredential)
        ));
    }

    #[tokio::test]
    async fn an_invalid_edit_is_refused_and_changes_nothing() {
        let h = harness();
        let created = h
            .service
            .create_server(ssh_server("web-01"))
            .await
            .expect("created");

        let mut edit = ssh_edit("");
        edit.host = String::new();
        assert!(h.service.update_server(created.id, edit).await.is_err());

        let unchanged = h.servers.get(created.id).await.expect("still there");
        assert_eq!(
            unchanged.name, "web-01",
            "a rejected edit was partly applied"
        );
    }

    #[tokio::test]
    async fn editing_a_server_that_does_not_exist_is_refused() {
        let h = harness();
        assert!(
            h.service
                .update_server(ServerId::new(), ssh_edit("ghost"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn editing_a_website_keeps_its_identifier() {
        // Which is what the availability history, the counter and the screenshot hang on.
        let h = harness();
        let created = h
            .service
            .create_website(NewWebsite {
                name: "Example".into(),
                url: "https://example.com/".into(),
                poll_interval_secs: 60,
                expected_status: 200,
                expected_text: None,
                server_id: None,
            })
            .await
            .expect("created");

        let updated = h
            .service
            .update_website(
                created.id,
                NewWebsite {
                    name: "Example renamed".into(),
                    url: "example.org".into(),
                    poll_interval_secs: 120,
                    expected_status: 301,
                    expected_text: Some("  hello  ".into()),
                    server_id: None,
                },
            )
            .await
            .expect("updated");

        assert_eq!(updated.id, created.id);
        assert_eq!(updated.name, "Example renamed");
        // The scheme is added by the same path that adds it for a new website.
        assert_eq!(updated.url, "https://example.org");
        assert_eq!(updated.poll_interval_secs, 120);
        assert_eq!(updated.expectation.status, 301);
        assert_eq!(updated.expectation.body_contains.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn clearing_the_expected_text_removes_it_rather_than_matching_everything() {
        // An empty expectation would match any response, so the check would pass while
        // testing nothing.
        let h = harness();
        let created = h
            .service
            .create_website(NewWebsite {
                name: "Example".into(),
                url: "https://example.com/".into(),
                poll_interval_secs: 60,
                expected_status: 200,
                expected_text: Some("welcome".into()),
                server_id: None,
            })
            .await
            .expect("created");

        let updated = h
            .service
            .update_website(
                created.id,
                NewWebsite {
                    name: "Example".into(),
                    url: "https://example.com/".into(),
                    poll_interval_secs: 60,
                    expected_status: 200,
                    expected_text: Some("   ".into()),
                    server_id: None,
                },
            )
            .await
            .expect("updated");

        assert_eq!(updated.expectation.body_contains, None);
    }
}
