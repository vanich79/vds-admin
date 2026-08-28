//! The Yandex.Metrica provider.
//!
//! Implements [`AnalyticsProvider`] against the [Reporting API]. Nothing above this
//! module knows Metrica exists — see `docs/adr/003-analytics-provider-architecture.md`.
//!
//! [Reporting API]: https://yandex.ru/dev/metrika/doc/api2/api_v1/intro.html

pub mod mapping;
pub mod response;

use async_trait::async_trait;
use chrono::Utc;
use std::sync::Arc;
use std::time::Duration;
use vds_domain::analytics::{
    AnalyticsCapabilities, AnalyticsCounter, AnalyticsInterval, AnalyticsMetric, AnalyticsPoint,
    AnalyticsSnapshot, AnalyticsTimeSeries, Referrer, TopPage,
};
use vds_domain::ids::{CredentialRef, ProviderId};
use vds_domain::metrics::MetricValue;
use vds_domain::ports::{
    AnalyticsProvider, AnalyticsQuery, ProviderError, ProviderHealth, SecretKind, SecretStore,
};

/// The provider's stable identifier. Stored in the database; never change it.
pub const PROVIDER_ID: &str = "yandex_metrica";

/// Installs `ring` as the process-wide rustls provider.
///
/// reqwest is built with `rustls-no-provider` (to keep aws-lc-rs, and its cmake and nasm
/// build requirements, out of the tree), which means it *panics* on the first HTTPS
/// request if no default provider has been installed. Doing it here rather than relying
/// on the composition root means this crate cannot be broken by the order in which
/// things happen to be constructed. Installation is idempotent.
fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Default API host.
pub const DEFAULT_BASE_URL: &str = "https://api-metrika.yandex.net";

/// Per-request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Reads website analytics from Yandex.Metrica.
pub struct YandexMetricaProvider {
    client: reqwest::Client,
    secrets: Arc<dyn SecretStore>,
    base_url: String,
}

impl YandexMetricaProvider {
    /// Builds a provider.
    pub fn new(secrets: Arc<dyn SecretStore>) -> Result<Self, ProviderError> {
        Self::with_base_url(secrets, DEFAULT_BASE_URL)
    }

    /// Builds a provider pointed at a different host, for tests.
    pub fn with_base_url(
        secrets: Arc<dyn SecretStore>,
        base_url: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        install_crypto_provider();

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("vds-admin/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        Ok(Self {
            client,
            secrets,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        })
    }

    /// Resolves the OAuth token for a credential handle.
    ///
    /// The token is fetched per request and dropped immediately: it is never cached in a
    /// field, so it cannot outlive the call or be reached through a `Debug` on this
    /// struct.
    async fn token(&self, reference: CredentialRef) -> Result<String, ProviderError> {
        let secret = self
            .secrets
            .retrieve(reference, SecretKind::AnalyticsToken)
            .await
            .map_err(|e| ProviderError::MissingCredential(e.to_string()))?;

        secret
            .expose_str()
            .map(str::to_owned)
            .map_err(|_| ProviderError::MissingCredential("the token is not valid UTF-8".into()))
    }

    /// Performs a report request.
    async fn request(
        &self,
        credential_ref: CredentialRef,
        path: &str,
        parameters: &[(&str, String)],
    ) -> Result<String, ProviderError> {
        let token = self.token(credential_ref).await?;
        let url = format!("{}{path}", self.base_url);

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("OAuth {token}"))
            .query(parameters)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    ProviderError::Timeout {
                        seconds: REQUEST_TIMEOUT.as_secs(),
                    }
                } else {
                    ProviderError::Network(err.to_string())
                }
            })?;

        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());

        let body = response
            .text()
            .await
            .map_err(|e| ProviderError::Network(format!("could not read the response: {e}")))?;

        if (200..300).contains(&status) {
            Ok(body)
        } else {
            Err(response::parse_error(status, &body, retry_after))
        }
    }

    /// Parameters shared by every report request.
    fn base_parameters(query: &AnalyticsQuery) -> Vec<(&'static str, String)> {
        vec![
            ("ids", query.external_id.clone()),
            ("date1", query.range.from.format("%Y-%m-%d").to_string()),
            ("date2", query.range.to.format("%Y-%m-%d").to_string()),
            // Metrica applies a sampling ratio on large counters unless asked not to.
            // Accuracy "full" costs latency but returns real numbers, which is the whole
            // point of a monitoring tool.
            ("accuracy", "full".to_owned()),
        ]
    }
}

#[async_trait]
impl AnalyticsProvider for YandexMetricaProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn display_name(&self) -> &'static str {
        "Yandex.Metrica"
    }

    fn capabilities(&self) -> AnalyticsCapabilities {
        AnalyticsCapabilities {
            supported_metrics: mapping::supported_metrics(),
            supports_time_series: true,
            supports_top_pages: true,
            supports_referrers: true,
            // Metrica does have a realtime API, but it is a different endpoint with
            // different semantics; claiming support here would make the UI offer
            // something that does not work.
            supports_realtime: false,
            min_interval: AnalyticsInterval::Hour,
            max_history_days: None,
        }
    }

    async fn validate_connection(
        &self,
        credential_ref: CredentialRef,
    ) -> Result<ProviderHealth, ProviderError> {
        match self
            .request(credential_ref, "/management/v1/counters", &[])
            .await
        {
            Ok(_) => Ok(ProviderHealth::Ok),
            Err(ProviderError::Authentication(message)) => Ok(ProviderHealth::Degraded(message)),
            Err(ProviderError::Forbidden(message)) => Ok(ProviderHealth::Degraded(message)),
            Err(ProviderError::Network(message)) => Ok(ProviderHealth::Unavailable(message)),
            Err(other) => Err(other),
        }
    }

    async fn counters(
        &self,
        credential_ref: CredentialRef,
    ) -> Result<Vec<AnalyticsCounter>, ProviderError> {
        let body = self
            .request(credential_ref, "/management/v1/counters", &[])
            .await?;

        #[derive(serde::Deserialize)]
        struct CounterList {
            #[serde(default)]
            counters: Vec<RawCounter>,
        }
        #[derive(serde::Deserialize)]
        struct RawCounter {
            id: i64,
            #[serde(default)]
            name: String,
            #[serde(default)]
            site: Option<String>,
        }

        let list: CounterList = serde_json::from_str(&body)
            .map_err(|e| ProviderError::Malformed(format!("counter list: {e}")))?;

        Ok(list
            .counters
            .into_iter()
            .map(|counter| AnalyticsCounter {
                id: counter.id.to_string(),
                name: if counter.name.is_empty() {
                    format!("Counter {}", counter.id)
                } else {
                    counter.name
                },
                site_url: counter.site,
            })
            .collect())
    }

    async fn overview(&self, query: &AnalyticsQuery) -> Result<AnalyticsSnapshot, ProviderError> {
        let expressions = mapping::requested_expressions();
        let mut parameters = Self::base_parameters(query);
        parameters.push(("metrics", expressions.join(",")));

        let body = self
            .request(query.credential_ref, "/stat/v1/data", &parameters)
            .await?;
        let report = response::parse_report(&body)?;

        let mut snapshot = AnalyticsSnapshot::new(
            query.website_id,
            ProviderId::new(PROVIDER_ID),
            query.range,
            Utc::now(),
        );

        for metric in AnalyticsMetric::ALL {
            let value = match mapping::expression_for(*metric) {
                Some(expression) => report
                    .totals
                    .get(expression)
                    .copied()
                    .map_or(MetricValue::NotAvailable, MetricValue::available),
                None => MetricValue::NotAvailable,
            };
            snapshot.set(*metric, value);
        }

        // Returning visitors is the one derived figure: Metrica reports total and new
        // users, so the difference is the returning ones. It is only computed when both
        // inputs are present — guessing would be worse than reporting nothing.
        if let (Some(users), Some(new_users)) = (
            report.totals.get("ym:s:users").copied(),
            report.totals.get("ym:s:newUsers").copied(),
        ) {
            snapshot.set(
                AnalyticsMetric::ReturningVisitors,
                MetricValue::available((users - new_users).max(0.0)),
            );
        }

        Ok(snapshot)
    }

    async fn time_series(
        &self,
        query: &AnalyticsQuery,
        metric: AnalyticsMetric,
        interval: AnalyticsInterval,
    ) -> Result<AnalyticsTimeSeries, ProviderError> {
        let Some(expression) = mapping::expression_for(metric) else {
            // Derived metrics have no series of their own; saying so is better than
            // returning an empty chart that looks like zero traffic.
            return Err(ProviderError::Unsupported("a time series for this metric"));
        };

        let mut parameters = Self::base_parameters(query);
        parameters.push(("metrics", expression.to_owned()));
        parameters.push(("dimensions", "ym:s:date".to_owned()));
        parameters.push(("group", mapping::group_for(interval).to_owned()));
        parameters.push(("sort", "ym:s:date".to_owned()));
        parameters.push(("limit", "1000".to_owned()));

        let body = self
            .request(query.credential_ref, "/stat/v1/data", &parameters)
            .await?;
        let report = response::parse_report(&body)?;

        let points = report
            .rows
            .iter()
            .filter_map(|row| {
                let timestamp = response::parse_dimension_date(&row.dimension)?;
                let value = row.metric(expression)?;
                Some(AnalyticsPoint { timestamp, value })
            })
            .collect();

        Ok(AnalyticsTimeSeries {
            website_id: query.website_id,
            provider: ProviderId::new(PROVIDER_ID),
            metric,
            interval,
            range: query.range,
            fetched_at: Utc::now(),
            points,
        })
    }

    async fn top_pages(
        &self,
        query: &AnalyticsQuery,
        limit: u32,
    ) -> Result<Vec<TopPage>, ProviderError> {
        let mut parameters = Self::base_parameters(query);
        parameters.push(("metrics", "ym:pv:pageviews,ym:pv:users".to_owned()));
        parameters.push(("dimensions", "ym:pv:URL".to_owned()));
        parameters.push(("sort", "-ym:pv:pageviews".to_owned()));
        parameters.push(("limit", limit.clamp(1, 1_000).to_string()));

        let body = self
            .request(query.credential_ref, "/stat/v1/data", &parameters)
            .await?;
        let report = response::parse_report(&body)?;

        Ok(report
            .rows
            .iter()
            .filter_map(|row| {
                Some(TopPage {
                    url: row.dimension.clone(),
                    page_views: row.metric("ym:pv:pageviews")?,
                    visitors: row
                        .metric("ym:pv:users")
                        .map_or(MetricValue::NotAvailable, MetricValue::available),
                })
            })
            .collect())
    }

    async fn referrers(
        &self,
        query: &AnalyticsQuery,
        limit: u32,
    ) -> Result<Vec<Referrer>, ProviderError> {
        let mut parameters = Self::base_parameters(query);
        parameters.push(("metrics", "ym:s:visits".to_owned()));
        parameters.push(("dimensions", "ym:s:lastsignTrafficSource".to_owned()));
        parameters.push(("sort", "-ym:s:visits".to_owned()));
        parameters.push(("limit", limit.clamp(1, 1_000).to_string()));

        let body = self
            .request(query.credential_ref, "/stat/v1/data", &parameters)
            .await?;
        let report = response::parse_report(&body)?;

        let total: f64 = report
            .rows
            .iter()
            .filter_map(|r| r.metric("ym:s:visits"))
            .sum();

        Ok(report
            .rows
            .iter()
            .filter_map(|row| {
                let visits = row.metric("ym:s:visits")?;
                Some(Referrer {
                    source: row.label.clone().unwrap_or_else(|| row.dimension.clone()),
                    visits,
                    // A share of nothing is not zero percent, it is undefined.
                    share_percent: if total > 0.0 {
                        MetricValue::available(visits / total * 100.0)
                    } else {
                        MetricValue::NotAvailable
                    },
                })
            })
            .collect())
    }
}

impl std::fmt::Debug for YandexMetricaProvider {
    /// Hand-written so that no future field — a cached token, say — can be printed by
    /// accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YandexMetricaProvider")
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use vds_domain::analytics::DateRange;
    use vds_domain::ids::WebsiteId;
    use vds_domain::ports::{Secret, SecretStoreError};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A secret store holding one token.
    struct StubSecrets {
        token: Option<String>,
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
            _reference: CredentialRef,
            _kind: SecretKind,
        ) -> Result<Secret, SecretStoreError> {
            self.token
                .clone()
                .map(Secret::from_string)
                .ok_or_else(|| SecretStoreError::NotFound("token".into()))
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
            _reference: CredentialRef,
            _kind: SecretKind,
        ) -> Result<bool, SecretStoreError> {
            Ok(self.token.is_some())
        }

        async fn delete_all(&self, _reference: CredentialRef) -> Result<(), SecretStoreError> {
            Ok(())
        }

        fn backend_description(&self) -> String {
            "stub".to_owned()
        }
    }

    fn provider_for(server: &MockServer, token: Option<&str>) -> YandexMetricaProvider {
        YandexMetricaProvider::with_base_url(
            Arc::new(StubSecrets {
                token: token.map(str::to_owned),
            }),
            server.uri(),
        )
        .expect("builds")
    }

    fn query() -> AnalyticsQuery {
        AnalyticsQuery {
            website_id: WebsiteId::new(),
            external_id: "12345".to_owned(),
            credential_ref: CredentialRef::new(),
            range: DateRange::new(
                NaiveDate::from_ymd_opt(2026, 7, 28).expect("valid"),
                NaiveDate::from_ymd_opt(2026, 8, 26).expect("valid"),
            ),
        }
    }

    const OVERVIEW_BODY: &str = r#"{
        "query": { "metrics": ["ym:s:avgVisitDurationSeconds","ym:s:bounceRate","ym:s:newUsers",
                               "ym:s:pageDepth","ym:s:pageviews","ym:s:users","ym:s:visits"] },
        "totals": [185.5, 42.5, 8000, 3.6, 89104, 24821, 31442],
        "data": []
    }"#;

    #[tokio::test]
    async fn an_overview_maps_every_metric_the_api_returned() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OVERVIEW_BODY))
            .mount(&server)
            .await;

        let snapshot = provider_for(&server, Some("token"))
            .overview(&query())
            .await
            .expect("succeeds");

        assert_eq!(
            snapshot.get(AnalyticsMetric::Visitors),
            MetricValue::Available(24_821.0)
        );
        assert_eq!(
            snapshot.get(AnalyticsMetric::Visits),
            MetricValue::Available(31_442.0)
        );
        assert_eq!(
            snapshot.get(AnalyticsMetric::PageViews),
            MetricValue::Available(89_104.0)
        );
        assert_eq!(
            snapshot.get(AnalyticsMetric::BounceRate),
            MetricValue::Available(42.5)
        );
        assert_eq!(
            snapshot.get(AnalyticsMetric::AverageSessionDuration),
            MetricValue::Available(185.5)
        );
    }

    #[tokio::test]
    async fn returning_visitors_is_derived_from_total_and_new_users() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OVERVIEW_BODY))
            .mount(&server)
            .await;

        let snapshot = provider_for(&server, Some("token"))
            .overview(&query())
            .await
            .expect("succeeds");
        // 24821 total users - 8000 new = 16821 returning.
        assert_eq!(
            snapshot.get(AnalyticsMetric::ReturningVisitors),
            MetricValue::Available(16_821.0)
        );
    }

    #[tokio::test]
    async fn a_metric_the_api_omitted_is_unavailable_never_zero() {
        // The single most important behaviour of the whole analytics layer.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"query":{"metrics":["ym:s:users"]},"totals":[100],"data":[]}"#,
            ))
            .mount(&server)
            .await;

        let snapshot = provider_for(&server, Some("token"))
            .overview(&query())
            .await
            .expect("succeeds");

        assert_eq!(
            snapshot.get(AnalyticsMetric::Visitors),
            MetricValue::Available(100.0)
        );
        assert_eq!(
            snapshot.get(AnalyticsMetric::BounceRate),
            MetricValue::NotAvailable
        );
        assert_eq!(
            snapshot.get(AnalyticsMetric::PageViews),
            MetricValue::NotAvailable
        );
        // Not derivable without newUsers, so it stays absent rather than equalling users.
        assert_eq!(
            snapshot.get(AnalyticsMetric::ReturningVisitors),
            MetricValue::NotAvailable
        );
    }

    #[tokio::test]
    async fn the_counter_id_and_date_range_are_sent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .and(query_param("ids", "12345"))
            .and(query_param("date1", "2026-07-28"))
            .and(query_param("date2", "2026-08-26"))
            .and(query_param("accuracy", "full"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OVERVIEW_BODY))
            .mount(&server)
            .await;

        assert!(
            provider_for(&server, Some("token"))
                .overview(&query())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn the_oauth_token_is_sent_in_the_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .and(header("authorization", "OAuth s3cret-token"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OVERVIEW_BODY))
            .mount(&server)
            .await;

        assert!(
            provider_for(&server, Some("s3cret-token"))
                .overview(&query())
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn a_missing_token_fails_before_any_request_is_made() {
        let server = MockServer::start().await;
        // No mock is mounted: any request would fail the test by returning 404.
        let err = provider_for(&server, None)
            .overview(&query())
            .await
            .expect_err("must fail");
        assert!(
            matches!(err, ProviderError::MissingCredential(_)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn a_time_series_becomes_dated_points() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .and(query_param("group", "day"))
            .and(query_param("dimensions", "ym:s:date"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"query":{"metrics":["ym:s:users"]},"totals":[3000],"data":[
                    {"dimensions":[{"name":"2026-08-24"}],"metrics":[1000]},
                    {"dimensions":[{"name":"2026-08-25"}],"metrics":[1200]},
                    {"dimensions":[{"name":"2026-08-26"}],"metrics":[800]}]}"#,
            ))
            .mount(&server)
            .await;

        let series = provider_for(&server, Some("token"))
            .time_series(&query(), AnalyticsMetric::Visitors, AnalyticsInterval::Day)
            .await
            .expect("succeeds");

        assert_eq!(series.points.len(), 3);
        assert_eq!(series.total(), 3_000.0);
        assert_eq!(series.peak(), Some(1_200.0));
        assert!(
            series
                .points
                .windows(2)
                .all(|w| w[0].timestamp < w[1].timestamp)
        );
    }

    #[tokio::test]
    async fn a_derived_metric_has_no_series_and_says_so() {
        let server = MockServer::start().await;
        let err = provider_for(&server, Some("token"))
            .time_series(
                &query(),
                AnalyticsMetric::ReturningVisitors,
                AnalyticsInterval::Day,
            )
            .await
            .expect_err("must fail");
        assert!(matches!(err, ProviderError::Unsupported(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn top_pages_are_returned_in_the_order_the_api_gave_them() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .and(query_param("dimensions", "ym:pv:URL"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"query":{"metrics":["ym:pv:pageviews","ym:pv:users"]},"totals":[900,500],
                    "data":[
                      {"dimensions":[{"name":"Home","url":"https://example.com/"}],"metrics":[600,400]},
                      {"dimensions":[{"name":"Pricing","url":"https://example.com/pricing"}],"metrics":[300,150]}]}"#,
            ))
            .mount(&server)
            .await;

        let pages = provider_for(&server, Some("token"))
            .top_pages(&query(), 10)
            .await
            .expect("succeeds");

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].url, "https://example.com/");
        assert_eq!(pages[0].page_views, 600.0);
        assert_eq!(pages[0].visitors, MetricValue::Available(400.0));
    }

    #[tokio::test]
    async fn referrer_shares_sum_to_a_hundred_percent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"query":{"metrics":["ym:s:visits"]},"totals":[1000],"data":[
                    {"dimensions":[{"name":"Search engines","id":"organic"}],"metrics":[750]},
                    {"dimensions":[{"name":"Direct traffic","id":"direct"}],"metrics":[250]}]}"#,
            ))
            .mount(&server)
            .await;

        let referrers = provider_for(&server, Some("token"))
            .referrers(&query(), 10)
            .await
            .expect("succeeds");

        assert_eq!(referrers[0].source, "Search engines");
        assert_eq!(referrers[0].share_percent, MetricValue::Available(75.0));
        assert_eq!(referrers[1].share_percent, MetricValue::Available(25.0));
    }

    #[tokio::test]
    async fn a_referrer_report_with_no_traffic_reports_no_share_rather_than_zero() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"query":{"metrics":["ym:s:visits"]},"totals":[0],"data":[
                    {"dimensions":[{"name":"Direct","id":"direct"}],"metrics":[0]}]}"#,
            ))
            .mount(&server)
            .await;

        let referrers = provider_for(&server, Some("token"))
            .referrers(&query(), 10)
            .await
            .expect("succeeds");
        assert_eq!(referrers[0].share_percent, MetricValue::NotAvailable);
    }

    #[tokio::test]
    async fn counters_can_be_listed_so_the_user_need_not_type_an_id() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/management/v1/counters"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"counters":[
                    {"id":12345,"name":"example.com","site":"example.com"},
                    {"id":67890,"name":"","site":null}]}"#,
            ))
            .mount(&server)
            .await;

        let counters = provider_for(&server, Some("token"))
            .counters(CredentialRef::new())
            .await
            .expect("succeeds");

        assert_eq!(counters.len(), 2);
        assert_eq!(counters[0].id, "12345");
        assert_eq!(counters[0].site_url.as_deref(), Some("example.com"));
        // An unnamed counter still gets something usable to display.
        assert_eq!(counters[1].name, "Counter 67890");
    }

    #[tokio::test]
    async fn a_revoked_token_is_reported_as_degraded_not_as_a_crash() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/management/v1/counters"))
            .respond_with(ResponseTemplate::new(401).set_body_string(
                r#"{"errors":[{"error_type":"invalid_token","message":"Invalid oauth_token"}]}"#,
            ))
            .mount(&server)
            .await;

        let health = provider_for(&server, Some("stale"))
            .validate_connection(CredentialRef::new())
            .await
            .expect("returns a health verdict");

        match health {
            ProviderHealth::Degraded(message) => {
                assert!(message.contains("oauth_token"), "message was {message}");
            }
            other => panic!("expected Degraded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_rate_limit_is_surfaced_as_retryable_with_its_delay() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "90")
                    .set_body_string("{}"),
            )
            .mount(&server)
            .await;

        let err = provider_for(&server, Some("token"))
            .overview(&query())
            .await
            .expect_err("must fail");

        assert!(err.is_retryable());
        assert_eq!(err.retry_after(), Some(chrono::Duration::seconds(90)));
    }

    #[tokio::test]
    async fn a_server_error_is_retryable_but_a_bad_counter_is_not() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .respond_with(ResponseTemplate::new(503).set_body_string("{}"))
            .mount(&server)
            .await;
        assert!(
            provider_for(&server, Some("t"))
                .overview(&query())
                .await
                .expect_err("must fail")
                .is_retryable()
        );

        let other = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stat/v1/data"))
            .respond_with(ResponseTemplate::new(404).set_body_string(
                r#"{"errors":[{"error_type":"not_found","message":"Counter not found"}]}"#,
            ))
            .mount(&other)
            .await;
        let err = provider_for(&other, Some("t"))
            .overview(&query())
            .await
            .expect_err("must fail");
        assert!(matches!(err, ProviderError::NotFound(_)));
        assert!(!err.is_retryable());
    }

    #[test]
    fn the_provider_advertises_what_it_can_actually_do() {
        let server = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(MockServer::start());
        let capabilities = provider_for(&server, Some("t")).capabilities();

        assert!(capabilities.supports_time_series);
        assert!(capabilities.supports_top_pages);
        assert!(capabilities.supports_referrers);
        // Realtime is a different endpoint; claiming it would make the UI offer something
        // that does not work.
        assert!(!capabilities.supports_realtime);
        assert!(capabilities.supports(AnalyticsMetric::Visitors));
        assert_eq!(capabilities.min_interval, AnalyticsInterval::Hour);
    }

    #[test]
    fn the_provider_id_is_the_documented_one() {
        // It is stored in the database; changing it would orphan every integration.
        assert_eq!(PROVIDER_ID, "yandex_metrica");
    }

    #[test]
    fn the_debug_output_cannot_leak_a_token() {
        let server = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(MockServer::start());
        let rendered = format!("{:?}", provider_for(&server, Some("s3cret-token")));
        assert!(
            !rendered.contains("s3cret-token"),
            "Debug leaked the token: {rendered}"
        );
    }
}
