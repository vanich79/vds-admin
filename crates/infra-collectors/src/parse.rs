//! Parsing helpers shared by the collectors.
//!
//! Everything here is deliberately forgiving in one specific way: unexpected input
//! yields `None`, never a panic and never a fabricated number. A collector that meets an
//! unfamiliar distribution should degrade to "not available", not take the monitoring
//! loop down with it.

/// Reads a `key: value` style line, as used by `/proc/meminfo` and `/etc/os-release`.
pub fn key_value(line: &str, separator: char) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(separator)?;
    Some((key.trim(), value.trim()))
}

/// Strips one layer of matching quotes, as found in `/etc/os-release`.
pub fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = value.chars().next();
        let last = value.chars().last();
        if (first == Some('"') && last == Some('"')) || (first == Some('\'') && last == Some('\''))
        {
            // Safe because we just confirmed both ends are single-byte ASCII quotes.
            return &value[1..value.len() - 1];
        }
    }
    value
}

/// Parses a whitespace-separated field by index.
pub fn field(line: &str, index: usize) -> Option<&str> {
    line.split_whitespace().nth(index)
}

/// Parses a `u64` field by index.
pub fn u64_field(line: &str, index: usize) -> Option<u64> {
    field(line, index)?.parse().ok()
}

/// Parses an `f64` field by index.
pub fn f64_field(line: &str, index: usize) -> Option<f64> {
    field(line, index)?.parse().ok()
}

/// Splits a line into `count` leading whitespace-separated fields plus the untouched
/// remainder.
///
/// Needed wherever the last column may itself contain spaces — `df`'s mount point,
/// `ps`'s command line, `systemctl`'s description.
pub fn split_n(line: &str, count: usize) -> Option<(Vec<&str>, &str)> {
    let mut fields = Vec::with_capacity(count);
    let mut rest = line.trim_start();
    for index in 0..count {
        if rest.is_empty() {
            return None;
        }
        match rest.find(char::is_whitespace) {
            Some(end) => {
                let (head, tail) = rest.split_at(end);
                fields.push(head);
                rest = tail.trim_start();
            }
            // The line ends here. That is fine if this was the last field we wanted —
            // `systemctl` rows with an empty description look exactly like this — but
            // not if fields are still outstanding.
            None if index + 1 == count => {
                fields.push(rest);
                rest = "";
            }
            None => return None,
        }
    }
    Some((fields, rest))
}

/// Parses a number that may carry a percent sign, e.g. docker's `"1.53%"`.
pub fn percent(value: &str) -> Option<f64> {
    value.trim().trim_end_matches('%').trim().parse().ok()
}

/// Parses a human-readable byte size as printed by Docker: `"1.5GiB"`, `"512MB"`, `"0B"`.
///
/// Both binary (`KiB`) and decimal (`kB`) suffixes appear in Docker output depending on
/// the field, so both are handled and distinguished.
pub fn human_bytes(value: &str) -> Option<u64> {
    let value = value.trim();
    let split = value
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(split);
    let number: f64 = number.trim().parse().ok()?;
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    let multiplier: f64 = match suffix.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kib" => 1_024.0,
        "mib" => 1_024.0 * 1_024.0,
        "gib" => 1_024.0 * 1_024.0 * 1_024.0,
        "tib" => 1_024.0 * 1_024.0 * 1_024.0 * 1_024.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        _ => return None,
    };
    let bytes = number * multiplier;
    if bytes > u64::MAX as f64 {
        None
    } else {
        Some(bytes as u64)
    }
}

/// Splits a Docker `"used / limit"` pair.
pub fn slash_pair(value: &str) -> Option<(&str, &str)> {
    let (left, right) = value.split_once('/')?;
    Some((left.trim(), right.trim()))
}

/// Clamps a percentage into `0..=100`, discarding values that are not finite.
///
/// Kernel counters occasionally produce a slightly-over-100 figure across a sampling
/// boundary; clamping is right, but a NaN means the calculation was wrong and must not
/// be presented as a measurement.
pub fn clamp_percent(value: f64) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }
    Some(value.clamp(0.0, 100.0))
}

/// Skips a header line when the first line looks like one.
///
/// `predicate` receives the lowercased first line.
pub fn skip_header<'a>(
    lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
    predicate: impl Fn(&str) -> bool,
) {
    if let Some(first) = lines.peek()
        && predicate(&first.to_ascii_lowercase())
    {
        lines.next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_value_trims_both_sides() {
        assert_eq!(
            key_value("MemTotal:   16316456 kB", ':'),
            Some(("MemTotal", "16316456 kB"))
        );
        assert_eq!(key_value("no separator", ':'), None);
    }

    #[test]
    fn unquote_removes_only_matching_pairs() {
        assert_eq!(unquote("\"Ubuntu\""), "Ubuntu");
        assert_eq!(unquote("'Ubuntu'"), "Ubuntu");
        assert_eq!(unquote("Ubuntu"), "Ubuntu");
        assert_eq!(unquote("\"Ubuntu"), "\"Ubuntu");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn split_n_keeps_the_remainder_intact() {
        let line = "1234 root  0.5  1.2 45678 /usr/bin/my program --flag";
        let (fields, rest) = split_n(line, 5).expect("five fields present");
        assert_eq!(fields, vec!["1234", "root", "0.5", "1.2", "45678"]);
        assert_eq!(rest, "/usr/bin/my program --flag");
    }

    #[test]
    fn split_n_fails_when_there_are_too_few_fields() {
        assert_eq!(split_n("only two", 5), None);
        assert_eq!(split_n("", 1), None);
    }

    #[test]
    fn split_n_accepts_a_line_that_ends_exactly_at_the_last_field() {
        let (fields, rest) = split_n("a b c d", 4).expect("four fields present");
        assert_eq!(fields, vec!["a", "b", "c", "d"]);
        assert_eq!(rest, "");
    }

    #[test]
    fn percent_tolerates_the_sign() {
        assert_eq!(percent("1.53%"), Some(1.53));
        assert_eq!(percent(" 0.00% "), Some(0.0));
        assert_eq!(percent("--"), None);
    }

    #[test]
    fn human_bytes_distinguishes_binary_from_decimal_suffixes() {
        assert_eq!(human_bytes("1KiB"), Some(1_024));
        assert_eq!(human_bytes("1kB"), Some(1_000));
        assert_eq!(human_bytes("1.5GiB"), Some(1_610_612_736));
        assert_eq!(human_bytes("0B"), Some(0));
        assert_eq!(human_bytes("512"), Some(512));
    }

    #[test]
    fn human_bytes_rejects_nonsense_rather_than_guessing() {
        assert_eq!(human_bytes("banana"), None);
        assert_eq!(human_bytes("12ZZ"), None);
        assert_eq!(human_bytes("-5MiB"), None);
        assert_eq!(human_bytes(""), None);
    }

    #[test]
    fn slash_pair_splits_docker_usage_strings() {
        assert_eq!(slash_pair("1.5GiB / 4GiB"), Some(("1.5GiB", "4GiB")));
        assert_eq!(slash_pair("no slash"), None);
    }

    #[test]
    fn clamp_percent_bounds_but_does_not_invent() {
        assert_eq!(clamp_percent(100.4), Some(100.0));
        assert_eq!(clamp_percent(-0.2), Some(0.0));
        assert_eq!(clamp_percent(42.0), Some(42.0));
        assert_eq!(clamp_percent(f64::NAN), None);
    }

    #[test]
    fn skip_header_only_removes_a_real_header() {
        let text = "Filesystem 1024-blocks\n/dev/sda1 100";
        let mut lines = text.lines().peekable();
        skip_header(&mut lines, |l| l.starts_with("filesystem"));
        assert_eq!(lines.next(), Some("/dev/sda1 100"));

        let text = "/dev/sda1 100";
        let mut lines = text.lines().peekable();
        skip_header(&mut lines, |l| l.starts_with("filesystem"));
        assert_eq!(lines.next(), Some("/dev/sda1 100"));
    }
}
