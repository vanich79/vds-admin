//! The analytics provider port.
//!
//! Every provider — Yandex.Metrica today, Google Analytics or Plausible tomorrow —
//! implements exactly this trait and nothing wider. See
//! `docs/adr/003-analytics-provider-architecture.md`.

use crate::analytics::{
    AnalyticsAccount, AnalyticsCapabilities, AnalyticsCounter, AnalyticsInterval, AnalyticsMetric,
    AnalyticsSnapshot, AnalyticsTimeSeries, DateRange, Referrer, TopPage,
};
use crate::ids::{CredentialRef, ProviderId, WebsiteId};
use async_trait::async_trait;
use chrono::Duration;

/// Everything a provider needs to answer one question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsQuery {
    pub website_id: WebsiteId,
    /// The provider's own identifier for the tracked entity (a Metrica counter ID, a
    /// GA4 property ID, …).
    pub external_id: String,
    /// Handle to the token or key; the provider resolves it through the
    /// [`super::SecretStore`]. The secret is never passed around in the clear.
    pub credential_ref: CredentialRef,
    pub range: DateRange,
}

/// Why a provider call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("authentication failed: {0}")]
    Authentication(String),
    #[error("access denied: {0}")]
    Forbidden(String),
    #[error("{0} not found")]
    NotFound(String),
    /// The provider asked us to slow down. `retry_after` is honoured by the scheduler.
    #[error("rate limited{}", match .retry_after_secs {
        Some(s) => format!(", retry after {s}s"),
        None => String::new(),
    })]
    RateLimited { retry_after_secs: Option<u64> },
    /// The provider rejected the request itself — a bad parameter, an unknown metric.
    ///
    /// Distinct from [`ProviderError::Upstream`] because retrying will produce exactly
    /// the same rejection: the fault is ours, and only new code or new configuration
    /// fixes it.
    #[error("the provider rejected the request: {0}")]
    Rejected(String),
    #[error("provider returned an error: {0}")]
    Upstream(String),
    #[error("network failure: {0}")]
    Network(String),
    #[error("timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("could not interpret the provider's response: {0}")]
    Malformed(String),
    #[error("this provider does not support {0}")]
    Unsupported(&'static str),
    #[error("credential unavailable: {0}")]
    MissingCredential(String),
}

impl ProviderError {
    /// Whether the scheduler should retry with backoff.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::Network(_)
                | ProviderError::Timeout { .. }
                | ProviderError::Upstream(_)
        )
    }

    /// How long the provider asked us to wait, when it said so.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ProviderError::RateLimited {
                retry_after_secs: Some(secs),
            } => Some(Duration::seconds(*secs as i64)),
            _ => None,
        }
    }
}

/// Result of a provider health check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderHealth {
    Ok,
    /// Reachable but not usable, e.g. an expired token.
    Degraded(String),
    Unavailable(String),
}

/// A source of website analytics.
///
/// The default implementations of the optional reports return
/// [`ProviderError::Unsupported`], so a minimal provider only has to implement
/// `overview`, and the UI hides what `capabilities()` says is missing rather than
/// calling a method that will fail.
#[async_trait]
pub trait AnalyticsProvider: Send + Sync {
    fn id(&self) -> ProviderId;

    /// Display name for the provider picker.
    fn display_name(&self) -> &'static str;

    fn capabilities(&self) -> AnalyticsCapabilities;

    /// Verifies that the stored credential works.
    async fn validate_connection(
        &self,
        credential_ref: CredentialRef,
    ) -> Result<ProviderHealth, ProviderError>;

    /// Accounts the credential can see. Used by the "add integration" flow.
    async fn accounts(
        &self,
        _credential_ref: CredentialRef,
    ) -> Result<Vec<AnalyticsAccount>, ProviderError> {
        Err(ProviderError::Unsupported("account listing"))
    }

    /// Counters/properties/sites the credential can see, so the user can pick one
    /// instead of typing an ID.
    async fn counters(
        &self,
        _credential_ref: CredentialRef,
    ) -> Result<Vec<AnalyticsCounter>, ProviderError> {
        Err(ProviderError::Unsupported("counter listing"))
    }

    /// Aggregate figures for the query's range.
    ///
    /// The only required report. Metrics the provider cannot serve must be reported as
    /// [`crate::metrics::MetricValue::NotAvailable`], never as zero.
    async fn overview(&self, query: &AnalyticsQuery) -> Result<AnalyticsSnapshot, ProviderError>;

    async fn time_series(
        &self,
        _query: &AnalyticsQuery,
        _metric: AnalyticsMetric,
        _interval: AnalyticsInterval,
    ) -> Result<AnalyticsTimeSeries, ProviderError> {
        Err(ProviderError::Unsupported("time series"))
    }

    async fn top_pages(
        &self,
        _query: &AnalyticsQuery,
        _limit: u32,
    ) -> Result<Vec<TopPage>, ProviderError> {
        Err(ProviderError::Unsupported("top pages"))
    }

    async fn referrers(
        &self,
        _query: &AnalyticsQuery,
        _limit: u32,
    ) -> Result<Vec<Referrer>, ProviderError> {
        Err(ProviderError::Unsupported("referrers"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limits_and_network_faults_are_retryable() {
        assert!(
            ProviderError::RateLimited {
                retry_after_secs: Some(30)
            }
            .is_retryable()
        );
        assert!(ProviderError::Network("dns".into()).is_retryable());
        assert!(ProviderError::Timeout { seconds: 10 }.is_retryable());
    }

    #[test]
    fn credential_problems_are_not_retryable() {
        // Retrying a bad token just burns quota and can trip lockouts.
        assert!(!ProviderError::Authentication("bad token".into()).is_retryable());
        assert!(!ProviderError::Forbidden("no access".into()).is_retryable());
        assert!(!ProviderError::Unsupported("top pages").is_retryable());
    }

    #[test]
    fn a_rejected_request_is_not_retryable() {
        // Sending the same bad request again produces the same rejection, forever.
        assert!(!ProviderError::Rejected("unknown metric ym:s:banana".into()).is_retryable());
    }

    #[test]
    fn retry_after_is_surfaced_when_the_provider_supplies_it() {
        let limited = ProviderError::RateLimited {
            retry_after_secs: Some(45),
        };
        assert_eq!(limited.retry_after(), Some(Duration::seconds(45)));
        assert!(limited.to_string().contains("retry after 45s"));

        let vague = ProviderError::RateLimited {
            retry_after_secs: None,
        };
        assert_eq!(vague.retry_after(), None);
    }
}
