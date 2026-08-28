//! Turning domain values into the strings the interface shows.
//!
//! All of it lives here, in Rust, rather than in `.slint` expressions: formatting has
//! edge cases — an unmeasured metric, a machine up for 143 days, a certificate that
//! expired yesterday — and edge cases deserve tests.

use crate::i18n;
use chrono::{DateTime, Duration, Utc};
use vds_domain::Status;
use vds_domain::metrics::{MetricUnit, MetricValue};

/// Substitutes the single `{}` in a catalogue string.
///
/// A hand-rolled substitution rather than `format!`, because the pattern is not known at
/// compile time — it comes from the catalogue, and which language that is depends on the
/// user. Anything without a placeholder is returned unchanged, so a mistranslation that
/// drops the `{}` loses the number rather than corrupting the line.
fn fill(pattern: &str, value: &str) -> String {
    match pattern.find("{}") {
        Some(at) => format!("{}{}{}", &pattern[..at], value, &pattern[at + 2..]),
        None => pattern.to_owned(),
    }
}

/// Substitutes two placeholders, left to right.
fn fill2(pattern: &str, first: &str, second: &str) -> String {
    fill(&fill(pattern, first), second)
}

/// What an unavailable value looks like.
///
/// An em dash, never "0" and never "N/A": the point is that the number does not exist,
/// and it should be visually obvious at a glance down a column.
pub const UNAVAILABLE: &str = "—";

/// Formats a metric value in its own unit.
pub fn metric(value: MetricValue, unit: MetricUnit) -> String {
    let Some(number) = value.value() else {
        return UNAVAILABLE.to_owned();
    };

    match unit {
        MetricUnit::Percent => format!("{number:.0}%"),
        MetricUnit::Bytes => bytes(number),
        MetricUnit::BytesPerSecond => format!("{}/s", bytes(number)),
        MetricUnit::Seconds => duration_secs(number as i64),
        MetricUnit::Milliseconds => format!("{number:.0} ms"),
        MetricUnit::Count => count(number),
        MetricUnit::Ratio => format!("{number:.2}"),
        MetricUnit::Celsius => format!("{number:.0}°C"),
    }
}

/// Formats a percentage with one decimal, for figures where precision matters.
pub fn percent(value: MetricValue) -> String {
    value
        .value()
        .map_or_else(|| UNAVAILABLE.to_owned(), |number| format!("{number:.1}%"))
}

/// Human-readable byte size.
///
/// Binary units, because that is what `df` and `free` report and a mismatch between the
/// app and the terminal is exactly the sort of thing that wastes an hour.
pub fn bytes(value: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if !value.is_finite() || value < 0.0 {
        return UNAVAILABLE.to_owned();
    }

    let mut size = value;
    let mut unit = 0;
    while size >= 1_024.0 && unit < UNITS.len() - 1 {
        size /= 1_024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{size:.0} {}", UNITS[unit])
    } else if size < 10.0 {
        format!("{size:.1} {}", UNITS[unit])
    } else {
        format!("{size:.0} {}", UNITS[unit])
    }
}

/// A count with thousands separators.
pub fn count(value: f64) -> String {
    if !value.is_finite() {
        return UNAVAILABLE.to_owned();
    }
    let rounded = value.round() as i64;
    let negative = rounded < 0;
    let digits = rounded.unsigned_abs().to_string();

    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(' ');
        }
        grouped.push(digit);
    }

    if negative {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// Uptime, in the largest sensible unit.
pub fn duration_secs(seconds: i64) -> String {
    if seconds < 0 {
        return UNAVAILABLE.to_owned();
    }

    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;

    let strings = i18n::strings();
    if days > 0 {
        fill2(
            strings.dur_days_hours,
            &days.to_string(),
            &hours.to_string(),
        )
    } else if hours > 0 {
        fill2(
            strings.dur_hours_mins,
            &hours.to_string(),
            &minutes.to_string(),
        )
    } else if minutes > 0 {
        fill(strings.dur_mins, &minutes.to_string())
    } else {
        fill(strings.dur_secs, &seconds.to_string())
    }
}

/// How long ago something happened.
pub fn relative_time(then: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    let strings = i18n::strings();
    let Some(then) = then else {
        return strings.time_never.to_owned();
    };

    let elapsed = now - then;
    if elapsed < Duration::zero() {
        // A clock that has gone backwards; "in the future" is more honest than a
        // negative age.
        return strings.time_just_now.to_owned();
    }

    let seconds = elapsed.num_seconds();
    if seconds < 10 {
        strings.time_just_now.to_owned()
    } else if seconds < 60 {
        fill(strings.time_secs_ago, &seconds.to_string())
    } else if elapsed.num_minutes() < 60 {
        fill(strings.time_mins_ago, &elapsed.num_minutes().to_string())
    } else if elapsed.num_hours() < 24 {
        fill(strings.time_hours_ago, &elapsed.num_hours().to_string())
    } else {
        fill(strings.time_days_ago, &elapsed.num_days().to_string())
    }
}

/// Days until a certificate expires, phrased for a human.
pub fn ssl_expiry(days: Option<i64>) -> String {
    let strings = i18n::strings();
    match days {
        None => UNAVAILABLE.to_owned(),
        Some(days) if days < 0 => fill(strings.ssl_expired_days_ago, &(-days).to_string()),
        Some(0) => strings.ssl_expires_today.to_owned(),
        Some(1) => strings.ssl_one_day.to_owned(),
        Some(days) => fill(strings.ssl_days, &days.to_string()),
    }
}

/// Availability percentage.
pub fn uptime(percent: Option<f64>) -> String {
    match percent {
        // No checks is not 100%.
        None => UNAVAILABLE.to_owned(),
        Some(value) if value >= 99.995 => "100%".to_owned(),
        Some(value) => format!("{value:.2}%"),
    }
}

/// A status as a short label.
pub fn status_label(status: Status) -> &'static str {
    let strings = i18n::strings();
    match status {
        Status::Healthy => strings.status_online,
        Status::Warning => strings.status_warning,
        Status::Critical => strings.status_critical,
        Status::Offline => strings.status_offline,
        Status::Unknown => strings.status_unknown,
    }
}

/// A time range, as the switcher shows it.
pub fn range_label(range: vds_domain::metrics::TimeRange) -> &'static str {
    use vds_domain::metrics::TimeRange as R;
    let strings = i18n::strings();
    match range {
        R::LastHour => strings.range_1h,
        R::LastSixHours => strings.range_6h,
        R::LastDay => strings.range_24h,
        R::LastWeek => strings.range_7d,
        R::LastMonth => strings.range_30d,
        R::LastQuarter => strings.range_90d,
        R::LastYear => strings.range_1y,
    }
}

/// An analytics period, as the switcher shows it.
pub fn period_label(period: vds_domain::analytics::AnalyticsPeriod) -> &'static str {
    use vds_domain::analytics::AnalyticsPeriod as P;
    let strings = i18n::strings();
    match period {
        P::Today => strings.period_today,
        P::Yesterday => strings.period_yesterday,
        P::LastSevenDays => strings.period_7d,
        P::LastThirtyDays => strings.period_30d,
        P::LastNinetyDays => strings.period_90d,
        // A custom range names itself; there is nothing to translate.
        P::Custom { .. } => strings.period_90d,
    }
}

/// An analytics metric, by name.
pub fn analytics_metric_label(metric: vds_domain::analytics::AnalyticsMetric) -> &'static str {
    use vds_domain::analytics::AnalyticsMetric as M;
    let strings = i18n::strings();
    match metric {
        M::Visitors => strings.am_visitors,
        M::Visits => strings.am_visits,
        M::PageViews => strings.am_page_views,
        M::Sessions => strings.am_sessions,
        M::UniqueVisitors => strings.am_unique_visitors,
        M::NewVisitors => strings.am_new_visitors,
        M::ReturningVisitors => strings.am_returning_visitors,
        M::BounceRate => strings.am_bounce_rate,
        M::AverageSessionDuration => strings.am_session_duration,
        M::PagesPerSession => strings.am_pages_per_session,
    }
}

/// A server metric, by name.
pub fn metric_kind_label(kind: vds_domain::metrics::MetricKind) -> &'static str {
    use vds_domain::metrics::MetricKind as K;
    let strings = i18n::strings();
    match kind {
        K::CpuUsage => strings.mk_cpu,
        K::MemoryUsage => strings.mk_ram,
        K::MemoryUsedBytes => strings.mk_ram_used,
        K::SwapUsage => strings.mk_swap,
        K::DiskUsage => strings.mk_disk,
        K::DiskUsedBytes => strings.mk_disk_used,
        K::NetworkRxBytesPerSec => strings.mk_network_in,
        K::NetworkTxBytesPerSec => strings.mk_network_out,
        K::LoadAverage1 => strings.mk_load_1m,
        K::LoadAverage5 => strings.mk_load_5m,
        K::LoadAverage15 => strings.mk_load_15m,
        K::UptimeSeconds => strings.mk_uptime,
        K::ProcessCount => strings.mk_processes,
        K::TemperatureCelsius => strings.mk_temperature,
        K::ResponseTimeMs => strings.mk_response_time,
        K::SslDaysRemaining => strings.mk_ssl_expiry,
    }
}

/// A screenshot refresh policy, as the picker shows it.
pub fn policy_label(policy: vds_domain::screenshot::ScreenshotRefreshPolicy) -> &'static str {
    use vds_domain::screenshot::ScreenshotRefreshPolicy as P;
    let strings = i18n::strings();
    match policy {
        P::Hourly => strings.policy_hourly,
        P::EverySixHours => strings.policy_six_hours,
        P::Daily => strings.policy_daily,
        P::Manual => strings.policy_manual,
    }
}

/// Period-over-period change, with its sign.
pub fn change(percent: Option<f64>) -> String {
    match percent {
        None => UNAVAILABLE.to_owned(),
        Some(value) if !value.is_finite() => UNAVAILABLE.to_owned(),
        Some(value) if value > 0.0 => format!("+{value:.1}%"),
        Some(value) => format!("{value:.1}%"),
    }
}

/// An HTTP status code, or a dash when there was no response at all.
pub fn http_status(code: Option<u16>) -> String {
    code.map_or_else(|| UNAVAILABLE.to_owned(), |code| code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    #[test]
    fn an_unavailable_metric_shows_a_dash_never_a_zero() {
        // The single most important formatting rule in the application.
        for unit in [
            MetricUnit::Percent,
            MetricUnit::Bytes,
            MetricUnit::BytesPerSecond,
            MetricUnit::Seconds,
            MetricUnit::Milliseconds,
            MetricUnit::Count,
            MetricUnit::Ratio,
            MetricUnit::Celsius,
        ] {
            assert_eq!(metric(MetricValue::NotAvailable, unit), UNAVAILABLE);
        }
        assert_eq!(percent(MetricValue::NotAvailable), UNAVAILABLE);
    }

    #[test]
    fn metrics_are_formatted_in_their_own_units() {
        assert_eq!(
            metric(MetricValue::Available(42.4), MetricUnit::Percent),
            "42%"
        );
        assert_eq!(
            metric(MetricValue::Available(1_536.0), MetricUnit::Bytes),
            "1.5 KiB"
        );
        assert_eq!(
            metric(MetricValue::Available(142.0), MetricUnit::Milliseconds),
            "142 ms"
        );
        assert_eq!(
            metric(MetricValue::Available(48.0), MetricUnit::Celsius),
            "48°C"
        );
        assert_eq!(
            metric(MetricValue::Available(1.25), MetricUnit::Ratio),
            "1.25"
        );
    }

    #[test]
    fn byte_sizes_use_binary_units_to_match_df_and_free() {
        assert_eq!(bytes(0.0), "0 B");
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(1_024.0), "1.0 KiB");
        assert_eq!(bytes(1_536.0), "1.5 KiB");
        assert_eq!(bytes(16.0 * 1_024.0 * 1_024.0 * 1_024.0), "16 GiB");
        assert_eq!(bytes(1_024.0_f64.powi(4)), "1.0 TiB");
    }

    #[test]
    fn an_absurd_byte_value_does_not_run_off_the_end_of_the_unit_table() {
        let huge = bytes(1_024.0_f64.powi(8));
        assert!(huge.ends_with("PiB"), "formatted as {huge}");
    }

    #[test]
    fn negative_and_non_finite_sizes_are_unavailable_rather_than_nonsense() {
        assert_eq!(bytes(-1.0), UNAVAILABLE);
        assert_eq!(bytes(f64::NAN), UNAVAILABLE);
        assert_eq!(count(f64::INFINITY), UNAVAILABLE);
    }

    #[test]
    fn large_counts_are_grouped_for_readability() {
        assert_eq!(count(0.0), "0");
        assert_eq!(count(999.0), "999");
        assert_eq!(count(1_000.0), "1 000");
        assert_eq!(count(24_821.0), "24 821");
        assert_eq!(count(1_234_567.0), "1 234 567");
        assert_eq!(count(-1_500.0), "-1 500");
    }

    #[test]
    fn uptime_uses_the_largest_sensible_unit() {
        // The example from the brief: a machine up for 143 days.
        assert_eq!(duration_secs(143 * 86_400 + 3_600 * 5), "143d 5h");
        assert_eq!(duration_secs(3_600 * 5 + 60 * 30), "5h 30m");
        assert_eq!(duration_secs(90), "1m");
        assert_eq!(duration_secs(30), "30s");
        assert_eq!(duration_secs(0), "0s");
    }

    #[test]
    fn a_negative_uptime_is_unavailable_rather_than_a_negative_day_count() {
        assert_eq!(duration_secs(-5), UNAVAILABLE);
    }

    #[test]
    fn relative_times_read_naturally() {
        let now = at(100_000);
        assert_eq!(relative_time(None, now), "never");
        assert_eq!(relative_time(Some(now), now), "just now");
        assert_eq!(
            relative_time(Some(now - Duration::seconds(30)), now),
            "30s ago"
        );
        assert_eq!(
            relative_time(Some(now - Duration::minutes(8)), now),
            "8m ago"
        );
        assert_eq!(relative_time(Some(now - Duration::hours(4)), now), "4h ago");
        assert_eq!(relative_time(Some(now - Duration::days(3)), now), "3d ago");
    }

    #[test]
    fn a_timestamp_from_the_future_does_not_render_as_a_negative_age() {
        // Happens when a server's clock is ahead, and "-4h ago" looks like a bug.
        let now = at(100_000);
        assert_eq!(
            relative_time(Some(now + Duration::hours(4)), now),
            "just now"
        );
    }

    #[test]
    fn ssl_expiry_is_phrased_differently_once_it_has_passed() {
        assert_eq!(ssl_expiry(Some(42)), "42 days");
        assert_eq!(ssl_expiry(Some(1)), "1 day");
        assert_eq!(ssl_expiry(Some(0)), "expires today");
        assert_eq!(ssl_expiry(Some(-3)), "expired 3 days ago");
        assert_eq!(ssl_expiry(None), UNAVAILABLE);
    }

    #[test]
    fn uptime_with_no_checks_is_a_dash_not_one_hundred_percent() {
        // Showing 100% for a site that has never been checked would be a lie.
        assert_eq!(uptime(None), UNAVAILABLE);
        assert_eq!(uptime(Some(99.98)), "99.98%");
        assert_eq!(uptime(Some(100.0)), "100%");
        assert_eq!(uptime(Some(75.0)), "75.00%");
    }

    #[test]
    fn a_near_perfect_uptime_does_not_round_up_to_a_bare_hundred_percent() {
        // 99.99% must not read as flawless.
        assert_eq!(uptime(Some(99.99)), "99.99%");
    }

    #[test]
    fn changes_carry_their_sign() {
        assert_eq!(change(Some(18.4)), "+18.4%");
        assert_eq!(change(Some(-35.0)), "-35.0%");
        assert_eq!(change(Some(0.0)), "0.0%");
        assert_eq!(change(None), UNAVAILABLE);
        assert_eq!(change(Some(f64::INFINITY)), UNAVAILABLE);
    }

    #[test]
    fn every_status_has_a_label() {
        for status in [
            Status::Healthy,
            Status::Warning,
            Status::Critical,
            Status::Offline,
            Status::Unknown,
        ] {
            assert!(!status_label(status).is_empty());
        }
        // "Online" reads better than "Healthy" on a server list.
        assert_eq!(status_label(Status::Healthy), "Online");
    }

    #[test]
    fn a_missing_http_status_is_a_dash() {
        assert_eq!(http_status(None), UNAVAILABLE);
        assert_eq!(http_status(Some(200)), "200");
        assert_eq!(http_status(Some(503)), "503");
    }
}
