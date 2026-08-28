//! The website aggregate: what to check, what a check produced, and how a check result
//! becomes a [`Status`].

use crate::ids::{ServerId, WebsiteId};
use crate::status::{Status, Threshold};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// A monitored URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Website {
    pub id: WebsiteId,
    pub name: String,
    /// Absolute URL including scheme.
    pub url: String,
    /// The server this site is hosted on, when the user has linked them. Optional:
    /// endpoints can be monitored without owning the machine behind them.
    pub server_id: Option<ServerId>,
    pub enabled: bool,
    pub poll_interval_secs: u32,
    pub timeout_secs: u32,
    pub expectation: HttpExpectation,
    /// Consecutive failures before the site is declared [`Status::Offline`].
    pub offline_after_failures: u32,
    /// Thresholds on response time, in milliseconds.
    pub response_time_threshold: Threshold,
    /// Thresholds on remaining certificate lifetime, in days.
    pub ssl_expiry_threshold: Threshold,
    /// Whether to follow 3xx responses before evaluating the expectation.
    pub follow_redirects: bool,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
}

pub const DEFAULT_WEBSITE_POLL_INTERVAL_SECS: u32 = 60;
pub const DEFAULT_WEBSITE_TIMEOUT_SECS: u32 = 15;
pub const DEFAULT_WEBSITE_OFFLINE_AFTER_FAILURES: u32 = 2;

impl Website {
    pub fn new(name: impl Into<String>, url: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            id: WebsiteId::new(),
            name: name.into(),
            url: url.into(),
            server_id: None,
            enabled: true,
            poll_interval_secs: DEFAULT_WEBSITE_POLL_INTERVAL_SECS,
            timeout_secs: DEFAULT_WEBSITE_TIMEOUT_SECS,
            expectation: HttpExpectation::default(),
            offline_after_failures: DEFAULT_WEBSITE_OFFLINE_AFTER_FAILURES,
            response_time_threshold: Threshold::above(1_000.0, 3_000.0),
            ssl_expiry_threshold: Threshold::below(14.0, 3.0),
            follow_redirects: true,
            tags: Vec::new(),
            created_at: now,
        }
    }

    pub fn poll_interval(&self) -> Duration {
        Duration::seconds(i64::from(self.poll_interval_secs.max(1)))
    }

    pub fn timeout(&self) -> Duration {
        Duration::seconds(i64::from(self.timeout_secs.max(1)))
    }

    /// Whether the URL uses TLS, and therefore whether certificate checks apply.
    pub fn is_https(&self) -> bool {
        self.url
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("https://")
    }

    /// The host portion of the URL, used for DNS and TCP checks.
    pub fn host(&self) -> Option<String> {
        url::Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
    }

    /// The effective port, defaulting by scheme.
    pub fn port(&self) -> Option<u16> {
        let parsed = url::Url::parse(&self.url).ok()?;
        parsed.port().or_else(|| match parsed.scheme() {
            "https" => Some(443),
            "http" => Some(80),
            _ => None,
        })
    }

    pub fn validate(&self) -> Result<(), WebsiteValidationError> {
        if self.name.trim().is_empty() {
            return Err(WebsiteValidationError::EmptyName);
        }
        let parsed =
            url::Url::parse(&self.url).map_err(|_| WebsiteValidationError::MalformedUrl)?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(WebsiteValidationError::UnsupportedScheme(
                parsed.scheme().to_owned(),
            ));
        }
        if parsed.host_str().is_none() {
            return Err(WebsiteValidationError::MissingHost);
        }
        if self.poll_interval_secs == 0 {
            return Err(WebsiteValidationError::InvalidPollInterval);
        }
        if self.timeout_secs == 0 {
            return Err(WebsiteValidationError::InvalidTimeout);
        }
        if self.offline_after_failures == 0 {
            return Err(WebsiteValidationError::InvalidFailureThreshold);
        }
        if !self.expectation.is_valid() {
            return Err(WebsiteValidationError::InvalidExpectedStatus);
        }
        Ok(())
    }
}

/// Why a website configuration was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebsiteValidationError {
    #[error("website name must not be empty")]
    EmptyName,
    #[error("URL is not well-formed")]
    MalformedUrl,
    #[error("unsupported URL scheme {0:?}; only http and https are monitored")]
    UnsupportedScheme(String),
    #[error("URL has no host")]
    MissingHost,
    #[error("poll interval must be at least 1 second")]
    InvalidPollInterval,
    #[error("timeout must be at least 1 second")]
    InvalidTimeout,
    #[error("offline threshold must be at least 1 failed check")]
    InvalidFailureThreshold,
    #[error("expected HTTP status must be in the range 100..=599")]
    InvalidExpectedStatus,
}

/// What a healthy response looks like for this website.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpExpectation {
    /// Status code that means "healthy".
    pub status: u16,
    /// Optional substring that must appear in the body.
    pub body_contains: Option<String>,
}

impl Default for HttpExpectation {
    fn default() -> Self {
        Self {
            status: 200,
            body_contains: None,
        }
    }
}

impl HttpExpectation {
    pub fn is_valid(&self) -> bool {
        (100..=599).contains(&self.status)
    }

    /// Whether a response satisfies the expectation.
    ///
    /// `body` is `None` when the body was not captured (for example a HEAD request); in
    /// that case a configured substring cannot be confirmed, and the check does not pass.
    pub fn is_satisfied_by(&self, status: u16, body: Option<&str>) -> bool {
        if status != self.status {
            return false;
        }
        match &self.body_contains {
            None => true,
            Some(needle) => body.is_some_and(|b| b.contains(needle.as_str())),
        }
    }
}

/// TLS certificate facts extracted from a completed handshake.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SslInfo {
    pub subject: String,
    pub issuer: String,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    /// SHA-256 fingerprint of the leaf certificate, hex encoded.
    pub fingerprint: String,
    /// Subject alternative names.
    pub san: Vec<String>,
}

impl SslInfo {
    /// Whole days remaining until expiry. Negative once expired.
    pub fn days_remaining(&self, now: DateTime<Utc>) -> i64 {
        (self.not_after - now).num_days()
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.not_after
    }

    /// True before the certificate's validity period begins.
    pub fn is_not_yet_valid(&self, now: DateTime<Utc>) -> bool {
        now < self.not_before
    }
}

/// Which stage of a check failed.
///
/// Ordered as the check proceeds, so the first failure is the informative one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStage {
    DnsResolution,
    TcpConnection,
    TlsHandshake,
    HttpRequest,
    /// The response arrived but did not match the expectation.
    Expectation,
}

impl CheckStage {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStage::DnsResolution => "dns_resolution",
            CheckStage::TcpConnection => "tcp_connection",
            CheckStage::TlsHandshake => "tls_handshake",
            CheckStage::HttpRequest => "http_request",
            CheckStage::Expectation => "expectation",
        }
    }
}

/// Why a check did not succeed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckFailure {
    pub stage: CheckStage,
    pub message: String,
}

/// The outcome of one website check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebsiteCheck {
    pub website_id: WebsiteId,
    pub checked_at: DateTime<Utc>,
    pub status: Status,
    /// Resolved addresses, empty when DNS failed.
    pub resolved_addresses: Vec<String>,
    /// Time to resolve DNS, in milliseconds.
    pub dns_ms: Option<u32>,
    /// Time to establish the TCP connection, in milliseconds.
    pub connect_ms: Option<u32>,
    /// Total round-trip time, in milliseconds.
    pub response_ms: Option<u32>,
    pub http_status: Option<u16>,
    /// Final URL after redirects, when it differs from the configured one.
    pub final_url: Option<String>,
    pub ssl: Option<SslInfo>,
    pub failure: Option<CheckFailure>,
}

impl WebsiteCheck {
    /// A check that never got off the ground.
    pub fn failed(
        website_id: WebsiteId,
        at: DateTime<Utc>,
        stage: CheckStage,
        message: impl Into<String>,
    ) -> Self {
        Self {
            website_id,
            checked_at: at,
            status: Status::Offline,
            resolved_addresses: Vec::new(),
            dns_ms: None,
            connect_ms: None,
            response_ms: None,
            http_status: None,
            final_url: None,
            ssl: None,
            failure: Some(CheckFailure {
                stage,
                message: message.into(),
            }),
        }
    }

    pub fn is_success(&self) -> bool {
        self.failure.is_none()
    }
}

/// Turns a completed check into a [`Status`], applying the website's thresholds.
///
/// Kept as a free function rather than a method on `WebsiteCheck` because it is policy,
/// not data: it depends on configuration, and the check itself should stay a pure record
/// of what happened.
pub fn evaluate_check(website: &Website, check: &WebsiteCheck, now: DateTime<Utc>) -> Status {
    // A transport-level failure means the site is not serving, full stop.
    if let Some(failure) = &check.failure {
        return match failure.stage {
            CheckStage::DnsResolution
            | CheckStage::TcpConnection
            | CheckStage::TlsHandshake
            | CheckStage::HttpRequest => Status::Offline,
            // The server answered, it just answered wrongly. That is a fault in the
            // application, not an outage of the host.
            CheckStage::Expectation => Status::Critical,
        };
    }

    let mut statuses = vec![Status::Healthy];

    if let Some(ms) = check.response_ms {
        statuses.push(website.response_time_threshold.classify(f64::from(ms)));
    }

    if let Some(ssl) = &check.ssl {
        if ssl.is_expired(now) || ssl.is_not_yet_valid(now) {
            statuses.push(Status::Critical);
        } else {
            statuses.push(
                website
                    .ssl_expiry_threshold
                    .classify(ssl.days_remaining(now) as f64),
            );
        }
    }

    Status::worst_of(statuses)
}

/// Rolling availability figure for a website over a window.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UptimeSummary {
    pub total_checks: u32,
    pub successful_checks: u32,
}

impl UptimeSummary {
    /// Availability as a percentage, or `None` when nothing was measured.
    ///
    /// Returning `None` rather than 100% matters: "no data" and "perfect uptime" must not
    /// look the same on a dashboard.
    pub fn percent(&self) -> Option<f64> {
        if self.total_checks == 0 {
            return None;
        }
        Some(f64::from(self.successful_checks) / f64::from(self.total_checks) * 100.0)
    }
}

/// The derived, persisted state of a website between checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebsiteRuntimeState {
    pub website_id: WebsiteId,
    pub status: Status,
    pub last_check: Option<DateTime<Utc>>,
    pub last_success: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub response_ms: Option<u32>,
    pub http_status: Option<u16>,
    pub ssl_days_remaining: Option<i64>,
    pub last_error: Option<String>,
}

impl WebsiteRuntimeState {
    pub fn unknown(website_id: WebsiteId) -> Self {
        Self {
            website_id,
            status: Status::Unknown,
            last_check: None,
            last_success: None,
            consecutive_failures: 0,
            response_ms: None,
            http_status: None,
            ssl_days_remaining: None,
            last_error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn site() -> Website {
        Website::new("Example", "https://example.com/health", at(0))
    }

    fn ok_check(response_ms: u32) -> WebsiteCheck {
        WebsiteCheck {
            website_id: WebsiteId::new(),
            checked_at: at(100),
            status: Status::Healthy,
            resolved_addresses: vec!["93.184.216.34".into()],
            dns_ms: Some(5),
            connect_ms: Some(20),
            response_ms: Some(response_ms),
            http_status: Some(200),
            final_url: None,
            ssl: None,
            failure: None,
        }
    }

    #[test]
    fn a_default_website_is_valid() {
        assert_eq!(site().validate(), Ok(()));
    }

    #[test]
    fn validation_rejects_non_http_schemes() {
        let mut website = site();
        website.url = "ftp://example.com".into();
        assert_eq!(
            website.validate(),
            Err(WebsiteValidationError::UnsupportedScheme("ftp".into()))
        );
    }

    #[test]
    fn validation_rejects_malformed_urls() {
        let mut website = site();
        website.url = "example.com".into();
        assert_eq!(
            website.validate(),
            Err(WebsiteValidationError::MalformedUrl)
        );
    }

    #[test]
    fn url_parts_are_derived_from_the_url() {
        let website = site();
        assert!(website.is_https());
        assert_eq!(website.host().as_deref(), Some("example.com"));
        assert_eq!(website.port(), Some(443));

        let plain = Website::new("Plain", "http://example.com:8080/", at(0));
        assert!(!plain.is_https());
        assert_eq!(plain.port(), Some(8080));
    }

    #[test]
    fn expectation_matches_status_and_body() {
        let expectation = HttpExpectation {
            status: 200,
            body_contains: Some("healthy".into()),
        };
        assert!(expectation.is_satisfied_by(200, Some("service is healthy")));
        assert!(!expectation.is_satisfied_by(200, Some("service is on fire")));
        assert!(!expectation.is_satisfied_by(500, Some("service is healthy")));
    }

    #[test]
    fn a_body_expectation_cannot_pass_without_a_body() {
        let expectation = HttpExpectation {
            status: 200,
            body_contains: Some("healthy".into()),
        };
        assert!(!expectation.is_satisfied_by(200, None));
    }

    #[test]
    fn expectation_without_a_body_rule_only_checks_status() {
        let expectation = HttpExpectation::default();
        assert!(expectation.is_satisfied_by(200, None));
    }

    #[test]
    fn ssl_days_remaining_goes_negative_after_expiry() {
        let ssl = SslInfo {
            subject: "CN=example.com".into(),
            issuer: "CN=Test CA".into(),
            not_before: at(0),
            not_after: at(86_400 * 10),
            fingerprint: "ab".into(),
            san: vec!["example.com".into()],
        };
        assert_eq!(ssl.days_remaining(at(0)), 10);
        assert!(!ssl.is_expired(at(0)));
        assert_eq!(ssl.days_remaining(at(86_400 * 12)), -2);
        assert!(ssl.is_expired(at(86_400 * 12)));
    }

    #[test]
    fn transport_failures_mean_offline() {
        let website = site();
        let check = WebsiteCheck::failed(
            website.id,
            at(0),
            CheckStage::TcpConnection,
            "connection refused",
        );
        assert_eq!(evaluate_check(&website, &check, at(0)), Status::Offline);
    }

    #[test]
    fn a_wrong_response_is_critical_not_offline() {
        // The distinction matters: the host is up, the application is broken.
        let website = site();
        let check = WebsiteCheck::failed(
            website.id,
            at(0),
            CheckStage::Expectation,
            "expected 200, got 503",
        );
        assert_eq!(evaluate_check(&website, &check, at(0)), Status::Critical);
    }

    #[test]
    fn a_fast_healthy_response_is_healthy() {
        let website = site();
        assert_eq!(
            evaluate_check(&website, &ok_check(120), at(0)),
            Status::Healthy
        );
    }

    #[test]
    fn a_slow_response_degrades_the_status() {
        let website = site();
        assert_eq!(
            evaluate_check(&website, &ok_check(1_500), at(0)),
            Status::Warning
        );
        assert_eq!(
            evaluate_check(&website, &ok_check(4_000), at(0)),
            Status::Critical
        );
    }

    #[test]
    fn an_expiring_certificate_degrades_an_otherwise_healthy_site() {
        let website = site();
        let mut check = ok_check(100);
        check.ssl = Some(SslInfo {
            subject: "CN=example.com".into(),
            issuer: "CN=Test CA".into(),
            not_before: at(0),
            not_after: at(86_400 * 5),
            fingerprint: "ab".into(),
            san: vec![],
        });
        // Five days left is below the 14-day warning threshold but above the 3-day
        // critical one.
        assert_eq!(evaluate_check(&website, &check, at(0)), Status::Warning);
    }

    #[test]
    fn an_expired_certificate_is_critical_even_when_the_site_responds() {
        let website = site();
        let mut check = ok_check(100);
        check.ssl = Some(SslInfo {
            subject: "CN=example.com".into(),
            issuer: "CN=Test CA".into(),
            not_before: at(0),
            not_after: at(86_400),
            fingerprint: "ab".into(),
            san: vec![],
        });
        assert_eq!(
            evaluate_check(&website, &check, at(86_400 * 2)),
            Status::Critical
        );
    }

    #[test]
    fn uptime_with_no_checks_is_unknown_not_perfect() {
        assert_eq!(
            UptimeSummary {
                total_checks: 0,
                successful_checks: 0
            }
            .percent(),
            None
        );
        assert_eq!(
            UptimeSummary {
                total_checks: 4,
                successful_checks: 3
            }
            .percent(),
            Some(75.0)
        );
    }
}
