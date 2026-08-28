//! The website checker: DNS → TCP → TLS → HTTP, timed at each stage.
//!
//! Stages are performed and reported separately so a failure says *what* broke. "Site
//! down" is not an actionable message; "NXDOMAIN", "connection refused" and "expected
//! 200, got 503" each point at a different fix.

use crate::dns::{DnsError, DnsResolver};
use crate::tls::CertificateInspector;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vds_application::monitoring::website::WebsiteChecker;
use vds_domain::Status;
use vds_domain::website::{CheckStage, Website, WebsiteCheck};

/// Longest body the checker will read when a website has a content expectation.
///
/// Bounded because a monitoring tool must not be talked into buffering a gigabyte by a
/// misbehaving endpoint.
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Checks websites over HTTP(S).
pub struct HttpWebsiteChecker {
    client: reqwest::Client,
    /// A second client that does not follow redirects, used when the website is
    /// configured to check the immediate response rather than the final one.
    strict_client: reqwest::Client,
    resolver: Arc<DnsResolver>,
    inspector: Option<Arc<CertificateInspector>>,
    user_agent: String,
}

/// Why the checker could not be built.
#[derive(Debug, thiserror::Error)]
pub enum CheckerError {
    #[error("could not build the HTTP client: {0}")]
    Client(String),
    #[error("could not build the DNS resolver: {0}")]
    Resolver(String),
}

impl HttpWebsiteChecker {
    /// Builds a checker with sensible defaults.
    pub fn new(user_agent: impl Into<String>) -> Result<Self, CheckerError> {
        install_crypto_provider();
        let user_agent = user_agent.into();

        let build = |follow: bool| {
            let policy = if follow {
                // A handful of hops is normal (http → https → www); dozens is a loop.
                reqwest::redirect::Policy::limited(10)
            } else {
                reqwest::redirect::Policy::none()
            };
            reqwest::Client::builder()
                .user_agent(user_agent.clone())
                .redirect(policy)
                .build()
                .map_err(|e| CheckerError::Client(e.to_string()))
        };

        Ok(Self {
            client: build(true)?,
            strict_client: build(false)?,
            resolver: Arc::new(
                DnsResolver::from_system().map_err(|e| CheckerError::Resolver(e.to_string()))?,
            ),
            // A machine without a working crypto provider still checks plain HTTP.
            inspector: CertificateInspector::new().ok().map(Arc::new),
            user_agent,
        })
    }

    /// Replaces the resolver, for tests.
    pub fn with_resolver(mut self, resolver: Arc<DnsResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Runs the full check.
    pub async fn perform(&self, website: &Website, now: DateTime<Utc>) -> WebsiteCheck {
        let timeout = Duration::from_secs(u64::from(website.timeout_secs.max(1)));

        let Some(host) = website.host() else {
            return WebsiteCheck::failed(
                website.id,
                now,
                CheckStage::HttpRequest,
                "the URL has no host",
            );
        };
        let port = website
            .port()
            .unwrap_or(if website.is_https() { 443 } else { 80 });

        // --- DNS ---
        let resolution = match self.resolver.resolve(&host, timeout).await {
            Ok(resolution) => resolution,
            Err(err) => {
                let stage = match err {
                    DnsError::Timeout { .. } => CheckStage::DnsResolution,
                    _ => CheckStage::DnsResolution,
                };
                return WebsiteCheck::failed(website.id, now, stage, err.to_string());
            }
        };
        let addresses: Vec<String> = resolution
            .addresses
            .iter()
            .map(ToString::to_string)
            .collect();
        let dns_ms = duration_ms(resolution.elapsed);

        // --- TCP ---
        let connect_started = Instant::now();
        let connect = tokio::time::timeout(
            timeout,
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await;

        let connect_ms = match connect {
            Ok(Ok(stream)) => {
                drop(stream);
                duration_ms(connect_started.elapsed())
            }
            Ok(Err(err)) => {
                let mut check = WebsiteCheck::failed(
                    website.id,
                    now,
                    CheckStage::TcpConnection,
                    err.to_string(),
                );
                check.resolved_addresses = addresses;
                check.dns_ms = dns_ms;
                return check;
            }
            Err(_) => {
                let mut check = WebsiteCheck::failed(
                    website.id,
                    now,
                    CheckStage::TcpConnection,
                    format!("timed out after {}s", timeout.as_secs()),
                );
                check.resolved_addresses = addresses;
                check.dns_ms = dns_ms;
                return check;
            }
        };

        // --- TLS certificate (inspection only; see `crate::tls`) ---
        // Deliberately performed before the HTTP request and never allowed to fail the
        // check: an expired certificate must be *reported*, and a site that is otherwise
        // fine should not be marked down because we could not read its certificate.
        let ssl = if website.is_https() {
            match &self.inspector {
                Some(inspector) => match inspector.inspect(&host, port, timeout).await {
                    Ok(info) => Some(info),
                    Err(err) => {
                        tracing::debug!(host = %host, error = %err, "certificate inspection failed");
                        None
                    }
                },
                None => None,
            }
        } else {
            None
        };

        // --- HTTP ---
        let client = if website.follow_redirects {
            &self.client
        } else {
            &self.strict_client
        };
        let request_started = Instant::now();
        let response = client.get(&website.url).timeout(timeout).send().await;

        let response = match response {
            Ok(response) => response,
            Err(err) => {
                let stage = if err.is_timeout() {
                    CheckStage::HttpRequest
                } else if err.is_connect() {
                    CheckStage::TlsHandshake
                } else {
                    CheckStage::HttpRequest
                };
                let mut check = WebsiteCheck::failed(website.id, now, stage, describe(&err));
                check.resolved_addresses = addresses;
                check.dns_ms = dns_ms;
                check.connect_ms = connect_ms;
                check.ssl = ssl;
                return check;
            }
        };

        let http_status = response.status().as_u16();
        let final_url = response.url().to_string();

        // The body is only read when something actually needs it. Downloading every
        // monitored page on every cycle would be gratuitous traffic.
        let body = if website.expectation.body_contains.is_some() {
            match read_body(response).await {
                Ok(body) => Some(body),
                Err(err) => {
                    let mut check = WebsiteCheck::failed(
                        website.id,
                        now,
                        CheckStage::HttpRequest,
                        format!("could not read the response body: {err}"),
                    );
                    check.resolved_addresses = addresses;
                    check.dns_ms = dns_ms;
                    check.connect_ms = connect_ms;
                    check.http_status = Some(http_status);
                    check.ssl = ssl;
                    return check;
                }
            }
        } else {
            None
        };

        let response_ms = duration_ms(request_started.elapsed());
        let satisfied = website
            .expectation
            .is_satisfied_by(http_status, body.as_deref());

        let mut check = WebsiteCheck {
            website_id: website.id,
            checked_at: now,
            status: Status::Healthy,
            resolved_addresses: addresses,
            dns_ms,
            connect_ms,
            response_ms,
            http_status: Some(http_status),
            final_url: (final_url != website.url).then_some(final_url),
            ssl,
            failure: None,
        };

        if !satisfied {
            check.failure = Some(vds_domain::website::CheckFailure {
                stage: CheckStage::Expectation,
                message: describe_mismatch(website, http_status, body.as_deref()),
            });
        }

        check.status = vds_domain::website::evaluate_check(website, &check, now);
        check
    }
}

#[async_trait]
impl WebsiteChecker for HttpWebsiteChecker {
    async fn check(&self, website: &Website, at: DateTime<Utc>) -> WebsiteCheck {
        self.perform(website, at).await
    }
}

/// Installs `ring` as the process-wide rustls provider.
///
/// reqwest is built with `rustls-no-provider` so that aws-lc-rs — which needs cmake and
/// nasm at build time — stays out of the dependency tree; `ring` cross-compiles to ARM
/// and Android far more easily. Installing is idempotent: a second call, or one racing
/// another thread, returns an error that is correctly ignored.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Reads a bounded amount of the response body.
async fn read_body(response: reqwest::Response) -> Result<String, reqwest::Error> {
    let bytes = response.bytes().await?;
    let capped = &bytes[..bytes.len().min(MAX_BODY_BYTES)];
    Ok(String::from_utf8_lossy(capped).into_owned())
}

fn duration_ms(elapsed: Duration) -> Option<u32> {
    u32::try_from(elapsed.as_millis()).ok()
}

/// A human-readable reason for a request failure.
fn describe(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        "the request timed out".to_owned()
    } else if err.is_connect() {
        format!("could not establish a connection: {err}")
    } else if err.is_redirect() {
        "too many redirects".to_owned()
    } else {
        err.to_string()
    }
}

/// Explains exactly which part of the expectation was not met.
fn describe_mismatch(website: &Website, status: u16, body: Option<&str>) -> String {
    if status != website.expectation.status {
        return format!("expected HTTP {}, got {status}", website.expectation.status);
    }
    match (&website.expectation.body_contains, body) {
        (Some(needle), Some(_)) => {
            format!("the response did not contain {needle:?}")
        }
        (Some(needle), None) => {
            format!("could not check for {needle:?}: the body was not read")
        }
        (None, _) => "the response did not match expectations".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn checker() -> HttpWebsiteChecker {
        HttpWebsiteChecker::new("vds-admin-test/0.1").expect("builds")
    }

    fn website_for(url: String) -> Website {
        let mut website = Website::new("Test", url, at(0));
        website.timeout_secs = 5;
        website
    }

    #[tokio::test]
    async fn a_healthy_endpoint_passes_with_timings() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_string("all good"))
            .mount(&server)
            .await;

        let website = website_for(format!("{}/health", server.uri()));
        let check = checker().perform(&website, at(1_000)).await;

        assert!(check.is_success(), "failure was {:?}", check.failure);
        assert_eq!(check.http_status, Some(200));
        assert_eq!(check.status, Status::Healthy);
        assert!(check.response_ms.is_some());
        assert!(!check.resolved_addresses.is_empty());
        // Plain HTTP, so there is nothing to report about a certificate.
        assert!(check.ssl.is_none());
    }

    #[tokio::test]
    async fn an_unexpected_status_fails_at_the_expectation_stage() {
        // Not at the transport stage: the server answered, so it is not down.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let website = website_for(format!("{}/health", server.uri()));
        let check = checker().perform(&website, at(1_000)).await;

        assert!(!check.is_success());
        let failure = check.failure.as_ref().expect("failure present");
        assert_eq!(failure.stage, CheckStage::Expectation);
        assert!(
            failure.message.contains("503"),
            "message was {}",
            failure.message
        );
        assert_eq!(check.http_status, Some(503));
        assert_eq!(check.status, Status::Critical);
    }

    #[tokio::test]
    async fn a_non_default_expected_status_is_honoured() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/redirect-me"))
            .respond_with(ResponseTemplate::new(301).insert_header("location", "/elsewhere"))
            .mount(&server)
            .await;

        let mut website = website_for(format!("{}/redirect-me", server.uri()));
        website.follow_redirects = false;
        website.expectation.status = 301;

        let check = checker().perform(&website, at(1_000)).await;
        assert!(check.is_success(), "failure was {:?}", check.failure);
        assert_eq!(check.http_status, Some(301));
    }

    #[tokio::test]
    async fn a_body_expectation_is_checked() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(ResponseTemplate::new(200).set_body_string("status: healthy"))
            .mount(&server)
            .await;

        let mut website = website_for(format!("{}/health", server.uri()));
        website.expectation.body_contains = Some("healthy".into());
        assert!(checker().perform(&website, at(1_000)).await.is_success());

        website.expectation.body_contains = Some("perfect".into());
        let check = checker().perform(&website, at(1_000)).await;
        assert!(!check.is_success());
        assert!(
            check
                .failure
                .as_ref()
                .expect("failure")
                .message
                .contains("perfect"),
            "message did not name the missing text"
        );
    }

    #[tokio::test]
    async fn the_body_is_not_downloaded_unless_it_is_needed() {
        // Downloading every monitored page on every cycle would be gratuitous traffic.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(1_000)))
            .mount(&server)
            .await;

        let website = website_for(format!("{}/big", server.uri()));
        assert!(website.expectation.body_contains.is_none());
        assert!(checker().perform(&website, at(1_000)).await.is_success());
    }

    #[tokio::test]
    async fn redirects_are_followed_and_the_final_url_is_reported() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/end"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/end"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let website = website_for(format!("{}/start", server.uri()));
        let check = checker().perform(&website, at(1_000)).await;

        assert!(check.is_success(), "failure was {:?}", check.failure);
        assert!(
            check
                .final_url
                .as_deref()
                .is_some_and(|u| u.ends_with("/end"))
        );
    }

    #[tokio::test]
    async fn redirects_can_be_refused() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/end"))
            .mount(&server)
            .await;

        let mut website = website_for(format!("{}/start", server.uri()));
        website.follow_redirects = false;

        let check = checker().perform(&website, at(1_000)).await;
        assert_eq!(check.http_status, Some(302));
        assert!(!check.is_success(), "302 is not the expected 200");
    }

    #[tokio::test]
    async fn a_closed_port_fails_at_the_tcp_stage_with_the_dns_result_kept() {
        // The DNS timing is still useful information even though the check failed later.
        let mut website = website_for("http://127.0.0.1:1/".to_owned());
        website.timeout_secs = 2;

        let check = checker().perform(&website, at(1_000)).await;
        let failure = check.failure.as_ref().expect("failure present");
        assert_eq!(failure.stage, CheckStage::TcpConnection);
        assert_eq!(check.status, Status::Offline);
        assert_eq!(check.resolved_addresses, vec!["127.0.0.1".to_owned()]);
    }

    #[tokio::test]
    async fn an_unresolvable_host_fails_at_the_dns_stage() {
        // ".invalid" is reserved by RFC 2606 and must never resolve.
        let mut website = website_for("http://this-host-does-not-exist.invalid/".to_owned());
        website.timeout_secs = 5;

        let check = checker().perform(&website, at(1_000)).await;
        let failure = check.failure.as_ref().expect("failure present");
        assert_eq!(failure.stage, CheckStage::DnsResolution);
        assert!(check.resolved_addresses.is_empty());
        assert_eq!(check.status, Status::Offline);
    }

    #[tokio::test]
    async fn a_slow_response_is_a_warning_rather_than_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1_200)))
            .mount(&server)
            .await;

        let mut website = website_for(format!("{}/slow", server.uri()));
        website.response_time_threshold = vds_domain::Threshold::above(1_000.0, 3_000.0);

        let check = checker().perform(&website, at(1_000)).await;
        assert!(check.is_success());
        assert_eq!(check.status, Status::Warning);
        assert!(check.response_ms.is_some_and(|ms| ms >= 1_000));
    }

    #[tokio::test]
    async fn a_response_slower_than_the_timeout_fails_rather_than_hanging() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/hang"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
            .mount(&server)
            .await;

        let mut website = website_for(format!("{}/hang", server.uri()));
        website.timeout_secs = 1;

        let started = Instant::now();
        let check = checker().perform(&website, at(1_000)).await;

        assert!(!check.is_success());
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the timeout was not enforced"
        );
    }

    #[tokio::test]
    async fn a_url_without_a_host_fails_cleanly() {
        let website = website_for("http:///nowhere".to_owned());
        let check = checker().perform(&website, at(1_000)).await;
        assert!(!check.is_success());
    }

    #[tokio::test]
    async fn the_user_agent_identifies_the_monitor() {
        // Site owners should be able to tell what is polling them.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ua"))
            .and(wiremock::matchers::header(
                "user-agent",
                "vds-admin-test/0.1",
            ))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let website = website_for(format!("{}/ua", server.uri()));
        let check = checker().perform(&website, at(1_000)).await;
        assert!(check.is_success(), "the user agent header was not sent");
        assert_eq!(checker().user_agent(), "vds-admin-test/0.1");
    }

    #[test]
    fn a_mismatch_message_names_the_specific_problem() {
        let mut website = website_for("http://example.com/".to_owned());
        assert!(describe_mismatch(&website, 503, None).contains("expected HTTP 200, got 503"));

        website.expectation.body_contains = Some("ok".into());
        let message = describe_mismatch(&website, 200, Some("nope"));
        assert!(message.contains("\"ok\""), "message was {message}");
    }
}
