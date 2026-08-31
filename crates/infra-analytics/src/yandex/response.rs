//! Parsing Metrica's Reporting API responses.
//!
//! Pure functions over JSON, so the shape of a real API response can be pinned in a
//! fixture and asserted against without a network.
//!
//! The response shape is:
//!
//! ```json
//! { "query": { "metrics": ["ym:s:users", "ym:s:visits"] },
//!   "totals": [24821, 31442],
//!   "data": [ { "dimensions": [{"name": "2026-08-26"}], "metrics": [1000, 1200] } ] }
//! ```
//!
//! Note that `totals` and each row's `metrics` are positional: they line up with
//! `query.metrics`. Getting that alignment wrong silently attributes one metric's value
//! to another, which is why it is parsed by name rather than by assumed order.

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use vds_domain::ports::ProviderError;

/// A decoded report.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Totals keyed by metric expression.
    pub totals: HashMap<String, f64>,
    /// One row per dimension value, in the order the API returned them.
    pub rows: Vec<ReportRow>,
}

/// One row of a grouped report.
#[derive(Debug, Clone, PartialEq)]
pub struct ReportRow {
    /// The first dimension's raw value, e.g. `"2026-08-26"` or `"/pricing"`.
    pub dimension: String,
    /// Human-readable name, when the API supplies one distinct from the value.
    pub label: Option<String>,
    pub metrics: HashMap<String, f64>,
}

impl ReportRow {
    pub fn metric(&self, expression: &str) -> Option<f64> {
        self.metrics.get(expression).copied()
    }
}

/// The raw response shape.
#[derive(Debug, Deserialize)]
struct RawResponse {
    query: RawQuery,
    #[serde(default)]
    totals: Vec<Option<f64>>,
    #[serde(default)]
    data: Vec<RawRow>,
}

#[derive(Debug, Deserialize)]
struct RawQuery {
    #[serde(default)]
    metrics: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RawRow {
    #[serde(default)]
    dimensions: Vec<RawDimension>,
    #[serde(default)]
    metrics: Vec<Option<f64>>,
}

#[derive(Debug, Deserialize)]
struct RawDimension {
    #[serde(default)]
    name: Option<String>,
    /// Metrica returns the machine value here for some dimensions (page URLs, dates).
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

impl RawDimension {
    /// The value to key on, preferring the machine-readable form.
    fn value(&self) -> Option<String> {
        self.url
            .clone()
            .or_else(|| self.id.clone())
            .or_else(|| self.name.clone())
    }
}

/// Metrica's error body.
#[derive(Debug, Deserialize)]
struct RawError {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    errors: Vec<RawErrorDetail>,
}

#[derive(Debug, Deserialize)]
struct RawErrorDetail {
    #[serde(default)]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Parses a successful report body.
pub fn parse_report(body: &str) -> Result<Report, ProviderError> {
    let raw: RawResponse = serde_json::from_str(body)
        .map_err(|e| ProviderError::Malformed(format!("could not decode the report: {e}")))?;

    if raw.query.metrics.is_empty() {
        return Err(ProviderError::Malformed(
            "the response did not say which metrics it contains".to_owned(),
        ));
    }

    // Positional alignment is the one thing that must not be assumed: a shorter `totals`
    // array than `query.metrics` would otherwise attribute values to the wrong metric.
    let totals = zip_metrics(&raw.query.metrics, &raw.totals);

    let rows = raw
        .data
        .into_iter()
        .map(|row| ReportRow {
            dimension: row
                .dimensions
                .first()
                .and_then(RawDimension::value)
                .unwrap_or_default(),
            label: row.dimensions.first().and_then(|d| d.name.clone()),
            metrics: zip_metrics(&raw.query.metrics, &row.metrics),
        })
        .collect();

    Ok(Report { totals, rows })
}

/// Pairs metric names with values, dropping nulls and any surplus on either side.
///
/// A `null` in Metrica's output means "no data for this metric in this period", which
/// must stay absent rather than becoming zero.
fn zip_metrics(names: &[String], values: &[Option<f64>]) -> HashMap<String, f64> {
    names
        .iter()
        .zip(values.iter())
        .filter_map(|(name, value)| value.filter(|v| v.is_finite()).map(|v| (name.clone(), v)))
        .collect()
}

/// Turns an HTTP status and body into the right [`ProviderError`].
///
/// The distinction matters to the scheduler: a 429 is retried with backoff, a 403 is
/// not, and retrying a revoked token forever would burn quota and never recover.
pub fn parse_error(status: u16, body: &str, retry_after: Option<u64>) -> ProviderError {
    let detail = serde_json::from_str::<RawError>(body).ok().and_then(|raw| {
        raw.errors
            .first()
            .and_then(|e| e.message.clone().or_else(|| e.error_type.clone()))
            .or(raw.message)
            .or_else(|| raw.code.map(|c| format!("error code {c}")))
    });
    let message = detail.unwrap_or_else(|| format!("HTTP {status}"));

    match status {
        401 => ProviderError::Authentication(message),
        // Metrica answers 403 to two different problems and says which in the body: a token it
        // will not accept, and a token it accepts for an account that cannot see this
        // counter. Collapsing them sends the user to check the wrong thing — as it did
        // for an afternoon, while the field held an application ID rather than a token.
        403 if mentions_the_token(&message) => ProviderError::Authentication(message),
        403 => ProviderError::Forbidden(message),
        404 => ProviderError::NotFound(message),
        429 => ProviderError::RateLimited {
            retry_after_secs: retry_after,
        },
        // Metrica uses 400 for quota exhaustion as well as for genuinely bad requests,
        // so the body is what distinguishes them.
        400 if message.to_lowercase().contains("quota") => ProviderError::RateLimited {
            retry_after_secs: retry_after,
        },
        500..=599 => ProviderError::Upstream(message),
        // Every other 4xx is our request being wrong. Retrying it on a schedule would
        // repeat the same rejection until someone changed the code or the settings.
        400..=499 => ProviderError::Rejected(format!("HTTP {status}: {message}")),
        _ => ProviderError::Upstream(format!("HTTP {status}: {message}")),
    }
}

/// Whether a rejection is about the token itself rather than about what it may see.
///
/// Metrica's wording for the first is `Invalid oauth_token`; for the second it names the
/// counter or says access is denied. Matching on the token's name is narrow on purpose —
/// anything unrecognised stays a plain refusal, which is the safer of the two to be wrong
/// about.
fn mentions_the_token(message: &str) -> bool {
    let lowered = message.to_lowercase();
    lowered.contains("oauth_token")
        || lowered.contains("oauth token")
        || lowered.contains("invalid token")
}

/// Parses a date dimension into an instant.
///
/// Metrica returns `"2026-08-26"` for daily grouping and `"2026-08-26 14:00:00"` for
/// hourly. Both are in the counter's timezone; they are interpreted as UTC, which is
/// what the chart axis expects.
pub fn parse_dimension_date(raw: &str) -> Option<DateTime<Utc>> {
    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return date
            .and_hms_opt(0, 0, 0)
            .and_then(|dt| Utc.from_utc_datetime(&dt).into());
    }
    NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|dt| Utc.from_utc_datetime(&dt))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OVERVIEW: &str = r#"{
        "query": { "metrics": ["ym:s:users", "ym:s:visits", "ym:s:pageviews", "ym:s:bounceRate"] },
        "totals": [24821, 31442, 89104, 42.5],
        "data": []
    }"#;

    const DAILY: &str = r#"{
        "query": { "metrics": ["ym:s:users"] },
        "totals": [3000],
        "data": [
            { "dimensions": [{"name": "2026-08-24"}], "metrics": [1000] },
            { "dimensions": [{"name": "2026-08-25"}], "metrics": [1200] },
            { "dimensions": [{"name": "2026-08-26"}], "metrics": [800] }
        ]
    }"#;

    #[test]
    fn totals_are_keyed_by_metric_name_not_by_position() {
        let report = parse_report(OVERVIEW).expect("parses");
        assert_eq!(report.totals.get("ym:s:users"), Some(&24_821.0));
        assert_eq!(report.totals.get("ym:s:visits"), Some(&31_442.0));
        assert_eq!(report.totals.get("ym:s:pageviews"), Some(&89_104.0));
        assert_eq!(report.totals.get("ym:s:bounceRate"), Some(&42.5));
    }

    #[test]
    fn a_truncated_totals_array_does_not_shift_values_onto_the_wrong_metric() {
        // The dangerous failure: silently reporting page views as visits.
        let truncated = r#"{
            "query": { "metrics": ["ym:s:users", "ym:s:visits", "ym:s:pageviews"] },
            "totals": [100],
            "data": []
        }"#;
        let report = parse_report(truncated).expect("parses");
        assert_eq!(report.totals.get("ym:s:users"), Some(&100.0));
        assert_eq!(report.totals.get("ym:s:visits"), None);
        assert_eq!(report.totals.get("ym:s:pageviews"), None);
    }

    #[test]
    fn a_null_metric_stays_absent_rather_than_becoming_zero() {
        let with_null = r#"{
            "query": { "metrics": ["ym:s:users", "ym:s:bounceRate"] },
            "totals": [100, null],
            "data": []
        }"#;
        let report = parse_report(with_null).expect("parses");
        assert_eq!(report.totals.get("ym:s:users"), Some(&100.0));
        assert_eq!(report.totals.get("ym:s:bounceRate"), None);
    }

    #[test]
    fn grouped_rows_are_parsed_in_order() {
        let report = parse_report(DAILY).expect("parses");
        assert_eq!(report.rows.len(), 3);
        assert_eq!(report.rows[0].dimension, "2026-08-24");
        assert_eq!(report.rows[0].metric("ym:s:users"), Some(1_000.0));
        assert_eq!(report.rows[2].metric("ym:s:users"), Some(800.0));
    }

    #[test]
    fn a_report_with_no_rows_is_valid() {
        let report = parse_report(OVERVIEW).expect("parses");
        assert!(report.rows.is_empty());
    }

    #[test]
    fn a_response_without_a_query_block_is_rejected() {
        // Without it there is no way to know what the numbers mean.
        assert!(parse_report(r#"{"totals": [1,2,3]}"#).is_err());
        assert!(parse_report(r#"{"query": {"metrics": []}, "totals": [1]}"#).is_err());
    }

    #[test]
    fn garbage_is_rejected_as_malformed() {
        let err = parse_report("not json").expect_err("must fail");
        assert!(matches!(err, ProviderError::Malformed(_)));
    }

    #[test]
    fn page_dimensions_prefer_the_url_over_the_display_name() {
        let pages = r#"{
            "query": { "metrics": ["ym:s:pageviews"] },
            "totals": [500],
            "data": [
                { "dimensions": [{"name": "Pricing", "url": "https://example.com/pricing"}],
                  "metrics": [500] }
            ]
        }"#;
        let report = parse_report(pages).expect("parses");
        assert_eq!(report.rows[0].dimension, "https://example.com/pricing");
        assert_eq!(report.rows[0].label.as_deref(), Some("Pricing"));
    }

    #[test]
    fn an_unauthorised_response_is_an_authentication_error_not_a_retry() {
        let body = r#"{"errors":[{"error_type":"invalid_token","message":"Invalid oauth_token"}],
                       "code":401,"message":"Invalid oauth_token"}"#;
        let err = parse_error(401, body, None);
        assert!(matches!(err, ProviderError::Authentication(_)));
        assert!(
            !err.is_retryable(),
            "retrying a bad token burns quota forever"
        );
        assert!(err.to_string().contains("Invalid oauth_token"));
    }

    #[test]
    fn a_forbidden_response_names_the_problem() {
        let body =
            r#"{"errors":[{"error_type":"access_denied","message":"No access to counter"}]}"#;
        let err = parse_error(403, body, None);
        assert!(matches!(err, ProviderError::Forbidden(_)));
        assert!(err.to_string().contains("No access to counter"));
    }

    #[test]
    fn a_rate_limit_carries_the_retry_delay() {
        let err = parse_error(429, "{}", Some(120));
        assert!(err.is_retryable());
        assert_eq!(err.retry_after(), Some(chrono::Duration::seconds(120)));
    }

    #[test]
    fn a_quota_error_disguised_as_a_bad_request_is_still_a_rate_limit() {
        // Metrica reports quota exhaustion as HTTP 400. Treating it as a permanent
        // failure would stop analytics until the app was restarted.
        let body = r#"{"errors":[{"error_type":"quota","message":"Quota exceeded"}]}"#;
        let err = parse_error(400, body, Some(60));
        assert!(
            matches!(err, ProviderError::RateLimited { .. }),
            "got {err:?}"
        );
        assert!(err.is_retryable());
    }

    #[test]
    fn a_genuinely_bad_request_is_not_retried() {
        // A wrong metric name would otherwise be retried on every scheduler tick for as
        // long as the application ran.
        let body = r#"{"errors":[{"error_type":"wrong_parameter","message":"Bad metric"}]}"#;
        let err = parse_error(400, body, None);
        assert!(matches!(err, ProviderError::Rejected(_)), "got {err:?}");
        assert!(!err.is_retryable());
        assert!(err.to_string().contains("Bad metric"));
    }

    #[test]
    fn an_unexpected_client_error_is_also_permanent() {
        for status in [405, 409, 413, 422] {
            let err = parse_error(status, "{}", None);
            assert!(
                !err.is_retryable(),
                "HTTP {status} was classified as retryable"
            );
        }
    }

    #[test]
    fn a_server_error_is_retried() {
        assert!(parse_error(503, "", None).is_retryable());
        assert!(parse_error(500, "{}", None).is_retryable());
    }

    #[test]
    fn an_unparsable_error_body_still_produces_a_useful_message() {
        let err = parse_error(418, "<html>teapot</html>", None);
        assert!(err.to_string().contains("418"));
    }

    #[test]
    fn date_dimensions_parse_for_both_groupings() {
        let daily = parse_dimension_date("2026-08-26").expect("parses");
        assert_eq!(
            daily.date_naive(),
            NaiveDate::from_ymd_opt(2026, 8, 26).expect("valid")
        );

        let hourly = parse_dimension_date("2026-08-26 14:00:00").expect("parses");
        assert_eq!(hourly.time().to_string(), "14:00:00");

        assert_eq!(parse_dimension_date("last tuesday"), None);
    }

    #[test]
    fn a_rejected_token_is_an_authentication_problem_even_at_403() {
        // Metrica answers 403 both for "this token is not valid" and for "this token
        // cannot see that counter". Telling them apart is the difference between "go and
        // fetch a new token" and "check which account owns the counter" — and sending
        // someone to the second when it is the first costs an afternoon.
        let body = r#"{"errors":[{"error_type":"invalid_token","message":"Invalid oauth_token"}],"code":403}"#;
        assert_eq!(parse_error(403, body, None).kind(), "authentication");
    }

    #[test]
    fn a_counter_the_account_cannot_see_stays_a_refusal() {
        let body = r#"{"errors":[{"error_type":"access_denied","message":"Access is denied to counter 12345"}],"code":403}"#;
        assert_eq!(parse_error(403, body, None).kind(), "forbidden");
    }

    #[test]
    fn an_unrecognised_403_stays_a_refusal() {
        // The safer of the two to be wrong about: it does not send anyone to replace a
        // token that is working.
        assert_eq!(parse_error(403, "{}", None).kind(), "forbidden");
    }
}
