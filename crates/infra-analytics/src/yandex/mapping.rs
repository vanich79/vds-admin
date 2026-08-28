//! Translation between the domain's metric vocabulary and Metrica's.
//!
//! Kept separate from the HTTP client and completely free of I/O, so the mapping — the
//! part most likely to be wrong, and the part a reviewer most needs to check — is
//! testable on its own.
//!
//! The equivalences are documented in `docs/ARCHITECTURE.md` §10. Two of them are
//! deliberate approximations and are commented as such; the rest are exact.

use vds_domain::analytics::{AnalyticsInterval, AnalyticsMetric};

/// A Metrica metric expression, e.g. `ym:s:users`.
pub type Expression = &'static str;

/// The Metrica expression for a domain metric, where one exists.
///
/// `None` means Metrica has no equivalent, and the provider reports the metric as
/// [`MetricValue::NotAvailable`](vds_domain::metrics::MetricValue::NotAvailable) rather
/// than inventing one.
pub fn expression_for(metric: AnalyticsMetric) -> Option<Expression> {
    match metric {
        AnalyticsMetric::Visitors => Some("ym:s:users"),
        AnalyticsMetric::Visits => Some("ym:s:visits"),
        AnalyticsMetric::PageViews => Some("ym:s:pageviews"),
        AnalyticsMetric::NewVisitors => Some("ym:s:newUsers"),
        AnalyticsMetric::BounceRate => Some("ym:s:bounceRate"),
        AnalyticsMetric::AverageSessionDuration => Some("ym:s:avgVisitDurationSeconds"),
        AnalyticsMetric::PagesPerSession => Some("ym:s:pageDepth"),

        // Metrica has no separate notion of a "session": a visit *is* the session, and
        // the two numbers would always be identical. Mapping them to the same expression
        // is honest; reporting one of them as unavailable would be misleading.
        AnalyticsMetric::Sessions => Some("ym:s:visits"),
        // Likewise, `ym:s:users` already counts unique users.
        AnalyticsMetric::UniqueVisitors => Some("ym:s:users"),

        // Metrica exposes new users but not returning ones. The provider derives it as
        // `users - newUsers`, so there is no direct expression to request.
        AnalyticsMetric::ReturningVisitors => None,
    }
}

/// Metrics the provider requests directly.
///
/// Deduplicated: `Visits` and `Sessions` share an expression, and requesting it twice
/// would waste a column in every response.
pub fn requested_expressions() -> Vec<Expression> {
    let mut expressions: Vec<Expression> = AnalyticsMetric::ALL
        .iter()
        .filter_map(|m| expression_for(*m))
        .collect();
    expressions.dedup_by(|a, b| a == b);
    // `dedup_by` only removes *adjacent* duplicates, so sort first.
    expressions.sort_unstable();
    expressions.dedup();
    expressions
}

/// The Metrica `group` parameter for a time-series interval.
pub fn group_for(interval: AnalyticsInterval) -> &'static str {
    match interval {
        AnalyticsInterval::Hour => "hour",
        AnalyticsInterval::Day => "day",
        AnalyticsInterval::Week => "week",
        AnalyticsInterval::Month => "month",
    }
}

/// Whether the metric is derived rather than requested.
pub fn is_derived(metric: AnalyticsMetric) -> bool {
    expression_for(metric).is_none()
}

/// Everything Metrica can serve.
pub fn supported_metrics() -> Vec<AnalyticsMetric> {
    // ReturningVisitors is included even though it has no expression: the provider does
    // compute it, so from the UI's point of view it is supported.
    AnalyticsMetric::ALL.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_expression_is_a_valid_metrica_metric_name() {
        for metric in AnalyticsMetric::ALL {
            let Some(expression) = expression_for(*metric) else {
                continue;
            };
            assert!(
                expression.starts_with("ym:s:"),
                "{metric} maps to {expression:?}, which is not a session metric"
            );
        }
    }

    #[test]
    fn the_core_traffic_metrics_map_to_the_documented_expressions() {
        // These four are what the dashboard leads with; getting one wrong would show
        // plausible but incorrect traffic.
        assert_eq!(
            expression_for(AnalyticsMetric::Visitors),
            Some("ym:s:users")
        );
        assert_eq!(expression_for(AnalyticsMetric::Visits), Some("ym:s:visits"));
        assert_eq!(
            expression_for(AnalyticsMetric::PageViews),
            Some("ym:s:pageviews")
        );
        assert_eq!(
            expression_for(AnalyticsMetric::BounceRate),
            Some("ym:s:bounceRate")
        );
    }

    #[test]
    fn sessions_and_visits_are_deliberately_the_same_metric() {
        // Metrica has no separate session concept. Documented in ARCHITECTURE.md §10.
        assert_eq!(
            expression_for(AnalyticsMetric::Sessions),
            expression_for(AnalyticsMetric::Visits)
        );
        assert_eq!(
            expression_for(AnalyticsMetric::UniqueVisitors),
            expression_for(AnalyticsMetric::Visitors)
        );
    }

    #[test]
    fn returning_visitors_has_no_expression_because_it_is_derived() {
        assert_eq!(expression_for(AnalyticsMetric::ReturningVisitors), None);
        assert!(is_derived(AnalyticsMetric::ReturningVisitors));
        assert!(!is_derived(AnalyticsMetric::Visitors));
    }

    #[test]
    fn requested_expressions_contain_no_duplicates() {
        // Requesting the same column twice wastes response size on every single call.
        let expressions = requested_expressions();
        let mut sorted = expressions.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), expressions.len());
    }

    #[test]
    fn requested_expressions_cover_every_non_derived_metric() {
        let expressions = requested_expressions();
        for metric in AnalyticsMetric::ALL {
            let Some(expected) = expression_for(*metric) else {
                continue;
            };
            assert!(
                expressions.contains(&expected),
                "{metric} would never be requested"
            );
        }
    }

    #[test]
    fn intervals_map_to_metrica_groups() {
        assert_eq!(group_for(AnalyticsInterval::Hour), "hour");
        assert_eq!(group_for(AnalyticsInterval::Day), "day");
        assert_eq!(group_for(AnalyticsInterval::Week), "week");
        assert_eq!(group_for(AnalyticsInterval::Month), "month");
    }

    #[test]
    fn every_domain_metric_is_accounted_for() {
        // A new metric added to the domain must be either mapped or explicitly derived;
        // this test fails until someone decides which.
        for metric in AnalyticsMetric::ALL {
            let mapped = expression_for(*metric).is_some();
            let derived = is_derived(*metric);
            assert!(mapped || derived, "{metric} is neither mapped nor derived");
        }
        assert_eq!(supported_metrics().len(), AnalyticsMetric::ALL.len());
    }
}
