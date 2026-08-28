//! Structured logging with secret redaction.
//!
//! Every record carries a timestamp, a level and the component that emitted it, and logs
//! rotate daily.
//!
//! # Redaction
//!
//! The primary defence against leaking a credential is the type system: `Secret` has a
//! hand-written `Debug`, does not implement `Serialize`, and zeroes itself on drop. This
//! module is the *second* line — it rewrites anything in a formatted record that looks
//! like a secret, so that a hand-written `format!` or a third-party crate's logging
//! cannot undo the guarantee.
//!
//! Defence in depth is warranted here because a leaked SSH password in a log file that a
//! user then attaches to a bug report is unrecoverable.

use std::io::Write;
use vds_application::config::LoggingSettings;

/// Replacement text for a redacted value.
pub const REDACTED: &str = "<redacted>";

/// Patterns that mark the start of something secret.
///
/// Matched case-insensitively against `key=value` and `key: value` shapes.
const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passphrase",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "credential",
    "private_key",
    "oauth",
];

/// Keys whose value runs to the end of the line rather than to the next space.
///
/// An `Authorization` header is `scheme credential` — two whitespace-separated tokens —
/// so stopping at the first space redacts the word "Basic" and publishes the credential.
const LINE_TAIL_KEYS: &[&str] = &["authorization"];

/// Literal prefixes that are secret in their entirety.
const SENSITIVE_PREFIXES: &[&str] = &[
    "-----BEGIN OPENSSH PRIVATE KEY-----",
    "-----BEGIN RSA PRIVATE KEY-----",
    "-----BEGIN EC PRIVATE KEY-----",
    "-----BEGIN PRIVATE KEY-----",
    "Bearer ",
    "Basic ",
    "OAuth ",
    "Digest ",
];

/// Rewrites a log line, replacing anything that looks like a secret.
///
/// Deliberately conservative in one direction only: it would rather redact something
/// harmless than emit a credential.
pub fn redact(text: &str) -> String {
    // Line by line, because a record can span several lines and one bad line must not
    // take the rest of the record with it.
    let mut output = String::with_capacity(text.len());
    let mut lines = text.split('\n').peekable();

    while let Some(line) = lines.next() {
        output.push_str(&redact_line(line));
        if lines.peek().is_some() {
            output.push('\n');
        }
    }
    output
}

/// Redacts a single line.
fn redact_line(line: &str) -> String {
    // Whole-line secrets first: a private key body or an `Authorization` header value has
    // no `key=value` structure to work with.
    for prefix in SENSITIVE_PREFIXES {
        if line.contains(prefix) {
            return format!("{REDACTED} (a value matching {prefix:?} was removed)");
        }
    }

    let mut output = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut index = 0;

    while index < line.len() {
        let Some(next) = find_sensitive_key(line, index) else {
            output.push_str(&line[index..]);
            break;
        };

        // Everything up to the value is kept, so the record still says *which* field was
        // removed.
        let value_start = next.value_start;
        output.push_str(&line[index..value_start]);
        output.push_str(REDACTED);

        // Skip the value: to the next separator — or, for a key whose value is several
        // whitespace-separated tokens, to the end of the line.
        let mut end = value_start;
        let quoted = bytes.get(value_start) == Some(&b'"');
        if quoted {
            end += 1;
            while end < line.len() && bytes.get(end) != Some(&b'"') {
                end += 1;
            }
            end = (end + 1).min(line.len());
        } else if next.to_line_end {
            end = line.len();
        } else {
            while end < line.len() && !matches!(bytes.get(end), Some(b' ' | b',' | b'}' | b')')) {
                end += 1;
            }
        }
        index = end;
    }

    output
}

/// Where a sensitive value starts, and how far it runs.
struct Sensitive {
    value_start: usize,
    /// True when the value runs to the end of the line rather than the next separator.
    to_line_end: bool,
}

/// Finds the next `key=` or `key: ` whose key looks sensitive.
fn find_sensitive_key(line: &str, from: usize) -> Option<Sensitive> {
    let lower = line.to_ascii_lowercase();
    let mut best: Option<(usize, bool)> = None;

    for key in SENSITIVE_KEYS {
        let to_line_end = LINE_TAIL_KEYS.contains(key);
        let mut search = from;
        while let Some(offset) = lower[search..].find(key) {
            let position = search + offset;
            let after = position + key.len();

            // The separator may be `=`, `:` or `": "` in JSON output.
            let separator = lower[after..]
                .find(|c: char| !matches!(c, '"' | ' '))
                .map(|o| after + o);

            if let Some(separator) = separator
                && matches!(lower.as_bytes().get(separator), Some(b'=' | b':'))
            {
                // Skip the separator and any following quote or space.
                let mut value_start = separator + 1;
                while matches!(lower.as_bytes().get(value_start), Some(b' ')) {
                    value_start += 1;
                }
                if best.is_none_or(|(current, _)| value_start < current) {
                    best = Some((value_start, to_line_end));
                }
            }
            search = position + key.len();
        }
    }

    best.map(|(value_start, to_line_end)| Sensitive {
        value_start,
        to_line_end,
    })
}

/// A writer that redacts every line on its way out.
pub struct RedactingWriter<W: Write> {
    inner: W,
}

impl<W: Write> RedactingWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buffer);
        let cleaned = redact(&text);
        self.inner.write_all(cleaned.as_bytes())?;
        // The caller is told the whole buffer was consumed, which it was — redaction
        // changes the length, and reporting the rewritten length would make callers
        // re-send the tail.
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// The file writer, with or without redaction.
enum LogWriter {
    Plain(tracing_appender::non_blocking::NonBlocking),
    Redacting(RedactingWriter<tracing_appender::non_blocking::NonBlocking>),
}

impl Write for LogWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            LogWriter::Plain(writer) => writer.write(buffer),
            LogWriter::Redacting(writer) => writer.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            LogWriter::Plain(writer) => writer.flush(),
            LogWriter::Redacting(writer) => writer.flush(),
        }
    }
}

/// Installs the global logging subscriber.
///
/// Returns a guard that must be kept alive for the process's lifetime: dropping it stops
/// the background writer, and the last records never reach the file.
pub fn install(
    settings: &LoggingSettings,
    log_directory: &std::path::Path,
) -> Result<Option<tracing_appender::non_blocking::WorkerGuard>, String> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    // `RUST_LOG` wins when set, so a user can turn on debug logging without editing
    // configuration; otherwise the configured level applies.
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(&settings.level))
        .map_err(|e| format!("invalid log filter: {e}"))?;

    if !settings.file_enabled {
        let registry = tracing_subscriber::registry().with(filter);
        // The two branches are written out rather than sharing a variable: a `fmt` layer
        // is a *different type* in JSON and plain form, so a shared binding would force
        // both branches to the same one.
        if settings.json {
            registry
                .with(fmt::layer().json().with_writer(std::io::stderr))
                .try_init()
        } else {
            registry
                .with(fmt::layer().with_target(true).with_writer(std::io::stderr))
                .try_init()
        }
        .map_err(|e| format!("could not install the log subscriber: {e}"))?;
        return Ok(None);
    }

    std::fs::create_dir_all(log_directory)
        .map_err(|e| format!("could not create {log_directory:?}: {e}"))?;

    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("vds-admin")
        .filename_suffix("log")
        .max_log_files(settings.max_files.max(1) as usize)
        .build(log_directory)
        .map_err(|e| format!("could not create the log appender: {e}"))?;

    let (non_blocking, guard) = tracing_appender::non_blocking(appender);

    // Redaction wraps the *file* writer specifically: a log file is the artifact that
    // gets attached to bug reports and copied around.
    //
    // A concrete enum rather than `Box<dyn Write>`: `MakeWriter` needs a named writer
    // type, and boxing would add an allocation per record.
    let redact_secrets = settings.redact_secrets;
    let make_writer = move || -> LogWriter {
        let writer = non_blocking.clone();
        if redact_secrets {
            LogWriter::Redacting(RedactingWriter::new(writer))
        } else {
            LogWriter::Plain(writer)
        }
    };

    let registry = tracing_subscriber::registry().with(filter);
    if settings.json {
        registry
            .with(fmt::layer().json().with_writer(std::io::stderr))
            .with(
                fmt::layer()
                    .json()
                    .with_ansi(false)
                    .with_writer(make_writer),
            )
            .try_init()
    } else {
        registry
            .with(fmt::layer().with_target(true).with_writer(std::io::stderr))
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_ansi(false)
                    .with_writer(make_writer),
            )
            .try_init()
    }
    .map_err(|e| format!("could not install the log subscriber: {e}"))?;

    Ok(Some(guard))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_field_is_redacted_but_the_field_name_survives() {
        // Knowing *that* a password was involved is useful; the value is not.
        let line = "connecting user=root password=hunter2 host=10.0.0.1";
        let cleaned = redact(line);

        assert!(!cleaned.contains("hunter2"), "leaked: {cleaned}");
        assert!(
            cleaned.contains("password="),
            "the field name should survive: {cleaned}"
        );
        assert!(
            cleaned.contains("user=root"),
            "unrelated fields must be kept: {cleaned}"
        );
        assert!(
            cleaned.contains("host=10.0.0.1"),
            "unrelated fields must be kept: {cleaned}"
        );
    }

    #[test]
    fn every_sensitive_key_shape_is_caught() {
        for (line, secret) in [
            ("password=hunter2", "hunter2"),
            ("passphrase: sesame", "sesame"),
            ("api_key=abc123", "abc123"),
            ("token=ghp_xxxxxxxx", "ghp_xxxxxxxx"),
            ("oauth_token=y0_AgAAAA", "y0_AgAAAA"),
            ("authorization: Basic dXNlcjpwYXNz", "dXNlcjpwYXNz"),
            ("credential=topsecret", "topsecret"),
        ] {
            let cleaned = redact(line);
            assert!(!cleaned.contains(secret), "{line:?} leaked as {cleaned:?}");
        }
    }

    #[test]
    fn json_formatted_records_are_redacted_too() {
        // The `json` logging mode must not become a bypass.
        let line = r#"{"level":"INFO","fields":{"password":"hunter2","host":"10.0.0.1"}}"#;
        let cleaned = redact(line);
        assert!(!cleaned.contains("hunter2"), "leaked: {cleaned}");
        assert!(
            cleaned.contains("10.0.0.1"),
            "unrelated fields must survive: {cleaned}"
        );
    }

    #[test]
    fn a_private_key_body_removes_the_whole_line() {
        // There is no safe portion of a line containing key material.
        let line = "loaded key -----BEGIN OPENSSH PRIVATE KEY----- b3BlbnNzaC1rZXktdjEA";
        let cleaned = redact(line);
        assert!(
            !cleaned.contains("b3BlbnNzaC1rZXktdjEA"),
            "leaked: {cleaned}"
        );
        assert!(!cleaned.contains("BEGIN OPENSSH PRIVATE KEY-----\n"));
        assert!(cleaned.starts_with(REDACTED));
    }

    #[test]
    fn a_multi_token_authorization_value_is_removed_whole() {
        // `Basic dXNlcjpwYXNz` is two tokens: stopping at the first space would redact
        // the word "Basic" and publish the credential.
        for line in [
            "authorization: Basic dXNlcjpwYXNz",
            "authorization=Digest username=admin response=deadbeef",
        ] {
            let cleaned = redact(line);
            assert!(!cleaned.contains("dXNlcjpwYXNz"), "leaked: {cleaned}");
            assert!(!cleaned.contains("deadbeef"), "leaked: {cleaned}");
        }
    }

    #[test]
    fn a_line_tail_redaction_stops_at_the_newline() {
        let cleaned = redact("authorization: Basic secret\nnext line is fine");
        assert!(!cleaned.contains("secret"), "leaked: {cleaned}");
        assert!(
            cleaned.contains("next line is fine"),
            "over-redacted: {cleaned}"
        );
    }

    #[test]
    fn bearer_and_oauth_headers_are_removed_entirely() {
        for line in [
            "sending Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload",
            "header OAuth y0_AgAAAABsecret",
        ] {
            let cleaned = redact(line);
            assert!(
                !cleaned.contains("eyJhbGciOiJIUzI1NiJ9"),
                "leaked: {cleaned}"
            );
            assert!(!cleaned.contains("y0_AgAAAABsecret"), "leaked: {cleaned}");
        }
    }

    #[test]
    fn quoted_values_are_redacted_whole() {
        let line = r#"config password="a value with spaces" next=ok"#;
        let cleaned = redact(line);
        assert!(
            !cleaned.contains("a value with spaces"),
            "leaked: {cleaned}"
        );
        assert!(
            cleaned.contains("next=ok"),
            "the following field must survive: {cleaned}"
        );
    }

    #[test]
    fn several_secrets_on_one_line_are_all_removed() {
        let line = "password=first token=second host=keepme";
        let cleaned = redact(line);
        assert!(!cleaned.contains("first"), "leaked: {cleaned}");
        assert!(!cleaned.contains("second"), "leaked: {cleaned}");
        assert!(cleaned.contains("keepme"));
    }

    #[test]
    fn ordinary_lines_pass_through_untouched() {
        // Over-redaction would make the logs useless.
        for line in [
            "collected metrics for prod-01 in 142ms",
            "server prod-01 became offline after 3 failed checks",
            "GET https://example.com/health -> 200 in 142ms",
        ] {
            assert_eq!(redact(line), line, "over-redacted");
        }
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(!redact("PASSWORD=Hunter2").contains("Hunter2"));
        assert!(!redact("Token: abc").contains("abc"));
    }

    #[test]
    fn an_empty_line_is_handled() {
        assert_eq!(redact(""), "");
    }

    #[test]
    fn the_redacting_writer_cleans_what_passes_through_it() {
        let mut sink: Vec<u8> = Vec::new();
        {
            let mut writer = RedactingWriter::new(&mut sink);
            writer
                .write_all(b"connecting password=hunter2\n")
                .expect("written");
            writer.flush().expect("flushed");
        }

        let written = String::from_utf8(sink).expect("utf-8");
        assert!(!written.contains("hunter2"), "the writer leaked: {written}");
        assert!(written.contains("password="));
    }

    #[test]
    fn the_writer_reports_the_input_length_so_callers_do_not_resend() {
        // Redaction changes the byte count; reporting the rewritten length would make
        // `write_all` loop on a tail that was never really there.
        let mut sink: Vec<u8> = Vec::new();
        let input = b"password=hunter2";
        let written = RedactingWriter::new(&mut sink)
            .write(input)
            .expect("written");
        assert_eq!(written, input.len());
    }
}
