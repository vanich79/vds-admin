//! Batching several commands into one SSH round trip.
//!
//! A collection cycle needs a dozen or so commands. Opening a channel for each costs a
//! round trip apiece, which at 200 servers over a slow link is the difference between a
//! cycle finishing and a cycle timing out. So the whole set is wrapped into one shell
//! script with delimiters, executed once, and split apart again here.
//!
//! The parsing is pure, which is what lets the tricky part — output that itself contains
//! something resembling a delimiter — be tested directly.

use vds_domain::ports::{Command, CommandOutput, TransportError};

/// Delimiter prefix.
///
/// Deliberately unlikely to occur in `df` or `ps` output, and the index and exit status
/// are part of the line so a stray occurrence cannot silently shift the alignment.
const MARKER: &str = "__VDS_ADMIN_MARKER__";

/// Builds the single shell script that runs every command.
///
/// Each command's output is bracketed by a begin and an end marker, and the end marker
/// carries the exit status. `2>&1` is deliberate: several collectors distinguish "not
/// installed" from "broken" by reading the error text, and a separate stderr channel
/// would interleave unpredictably across a batch.
pub fn build_script(commands: &[Command]) -> String {
    let mut script = String::with_capacity(commands.len() * 128);
    // A predictable locale keeps `df` and `ps` column headers in the form the parsers
    // expect, whatever the server's language is.
    script.push_str("export LC_ALL=C LANG=C 2>/dev/null || true\n");

    for (index, command) in commands.iter().enumerate() {
        script.push_str(&format!("printf '\\n{MARKER}:BEGIN:{index}\\n'\n"));
        script.push_str(&command.to_shell());
        script.push('\n');
        script.push_str(&format!("printf '\\n{MARKER}:END:{index}:%d\\n' \"$?\"\n"));
    }
    script
}

/// Splits a batched result back into per-command outputs.
///
/// Returns one entry per input command. A command whose markers are missing — because
/// the connection dropped part way, or the shell died — is reported as an error rather
/// than as empty output, so a collector never mistakes a truncated batch for a host that
/// has nothing to say.
pub fn split_output(raw: &str, count: usize) -> Vec<Result<CommandOutput, TransportError>> {
    let mut results: Vec<Option<CommandOutput>> = vec![None; count];

    for (index, slot) in results.iter_mut().enumerate() {
        let begin = format!("{MARKER}:BEGIN:{index}\n");
        let end_prefix = format!("{MARKER}:END:{index}:");

        let Some(start) = raw.find(&begin).map(|position| position + begin.len()) else {
            continue;
        };
        let Some(end_offset) = raw[start..].find(&end_prefix) else {
            continue;
        };

        let body = &raw[start..start + end_offset];
        let status_line = raw[start + end_offset + end_prefix.len()..]
            .lines()
            .next()
            .unwrap_or("")
            .trim();
        let exit_code = status_line.parse::<i32>().unwrap_or(-1);

        *slot = Some(CommandOutput {
            stdout: body.trim_matches('\n').to_owned(),
            stderr: String::new(),
            exit_code,
        });
    }

    results
        .into_iter()
        .enumerate()
        .map(|(index, output)| {
            output.ok_or_else(|| {
                TransportError::Protocol(format!(
                    "the response for command {index} was missing or truncated"
                ))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(index: usize, body: &str, status: i32) -> String {
        format!("\n{MARKER}:BEGIN:{index}\n{body}\n{MARKER}:END:{index}:{status}\n")
    }

    #[test]
    fn the_script_contains_every_command() {
        let commands = vec![
            Command::read("/proc/meminfo"),
            Command::shell("df -Pk"),
            Command::sample_twice("/proc/stat", 500),
        ];
        let script = build_script(&commands);

        assert!(script.contains("cat '/proc/meminfo'"));
        assert!(script.contains("df -Pk"));
        assert!(script.contains("sleep 0.500"));
        for index in 0..commands.len() {
            assert!(script.contains(&format!("{MARKER}:BEGIN:{index}")));
            assert!(script.contains(&format!("{MARKER}:END:{index}")));
        }
    }

    #[test]
    fn the_script_pins_the_locale_so_parsers_see_the_expected_columns() {
        // A Russian or German server would otherwise translate `df` headers.
        assert!(build_script(&[Command::shell("df")]).contains("LC_ALL=C"));
    }

    #[test]
    fn outputs_are_split_back_out_in_order() {
        let raw = format!(
            "{}{}{}",
            marked(0, "MemTotal: 100 kB", 0),
            marked(1, "Filesystem 1024-blocks", 0),
            marked(2, "cpu 1 2 3 4", 0)
        );

        let results = split_output(&raw, 3);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].as_ref().expect("ok").stdout, "MemTotal: 100 kB");
        assert_eq!(
            results[1].as_ref().expect("ok").stdout,
            "Filesystem 1024-blocks"
        );
        assert_eq!(results[2].as_ref().expect("ok").stdout, "cpu 1 2 3 4");
    }

    #[test]
    fn exit_codes_are_recovered_per_command() {
        // This is how the Docker collector tells "not installed" from "working".
        let raw = format!(
            "{}{}",
            marked(0, "ok", 0),
            marked(1, "sh: docker: not found", 127)
        );

        let results = split_output(&raw, 2);
        assert!(results[0].as_ref().expect("ok").is_success());
        assert_eq!(results[1].as_ref().expect("ok").exit_code, 127);
        assert!(!results[1].as_ref().expect("ok").is_success());
    }

    #[test]
    fn multi_line_output_survives_intact() {
        let body = "line one\nline two\nline three";
        let results = split_output(&marked(0, body, 0), 1);
        assert_eq!(results[0].as_ref().expect("ok").stdout, body);
    }

    #[test]
    fn empty_output_is_preserved_as_empty_not_as_missing() {
        // `docker ps` on a host with no containers prints nothing and exits zero.
        let results = split_output(&marked(0, "", 0), 1);
        let output = results[0].as_ref().expect("ok");
        assert_eq!(output.stdout, "");
        assert!(output.is_success());
    }

    #[test]
    fn a_truncated_batch_reports_the_missing_commands_rather_than_empty_output() {
        // A dropped connection must not look like a host with no disks and no memory.
        let raw = format!("{}{}", marked(0, "first", 0), marked(1, "second", 0));

        let results = split_output(&raw, 4);
        assert_eq!(results.len(), 4);
        assert!(results[0].is_ok());
        assert!(results[1].is_ok());
        assert!(matches!(results[2], Err(TransportError::Protocol(_))));
        assert!(matches!(results[3], Err(TransportError::Protocol(_))));
    }

    #[test]
    fn output_cut_off_mid_command_is_an_error_not_partial_data() {
        let raw = format!("\n{MARKER}:BEGIN:0\nhalf of the out");
        let results = split_output(&raw, 1);
        assert!(matches!(results[0], Err(TransportError::Protocol(_))));
    }

    #[test]
    fn command_output_that_mentions_the_marker_does_not_break_alignment() {
        // A process listing containing the marker text is contrived but survivable: the
        // index and status are part of the real delimiter, so a bare mention does not
        // terminate a block.
        let body = format!("root 123 grep {MARKER}");
        let raw = format!("{}{}", marked(0, &body, 0), marked(1, "second", 0));

        let results = split_output(&raw, 2);
        assert!(results[0].as_ref().expect("ok").stdout.contains("grep"));
        assert_eq!(results[1].as_ref().expect("ok").stdout, "second");
    }

    #[test]
    fn a_missing_exit_status_becomes_minus_one_rather_than_a_false_success() {
        let raw = format!("\n{MARKER}:BEGIN:0\nbody\n{MARKER}:END:0:\n");
        let results = split_output(&raw, 1);
        let output = results[0].as_ref().expect("parsed");
        assert_eq!(output.exit_code, -1);
        assert!(!output.is_success());
    }

    #[test]
    fn shell_noise_before_the_first_marker_is_ignored() {
        // Login banners, motd, "You have new mail" — all common, all harmless.
        let raw = format!(
            "Welcome to Ubuntu 22.04 LTS\nLast login: Tue\n{}",
            marked(0, "MemTotal: 100 kB", 0)
        );
        let results = split_output(&raw, 1);
        assert_eq!(results[0].as_ref().expect("ok").stdout, "MemTotal: 100 kB");
    }

    #[test]
    fn an_empty_batch_produces_an_empty_result() {
        assert!(split_output("", 0).is_empty());
        assert!(build_script(&[]).contains("LC_ALL"));
    }

    #[test]
    fn a_realistic_batch_round_trips_through_script_and_split() {
        // The end-to-end shape, without a server: build the script, simulate what a shell
        // would emit, and split it back.
        let commands = vec![
            Command::read("/proc/sys/kernel/hostname"),
            Command::read("/proc/meminfo"),
            Command::shell("df -PkT 2>/dev/null || df -Pk"),
        ];
        let script = build_script(&commands);
        assert_eq!(script.matches(&format!("{MARKER}:BEGIN:")).count(), 3);

        let simulated = format!(
            "{}{}{}",
            marked(0, "web-01", 0),
            marked(1, "MemTotal: 16316456 kB\nMemAvailable: 12000000 kB", 0),
            marked(
                2,
                "Filesystem Type 1024-blocks Used Available Capacity Mounted on",
                0
            )
        );

        let results = split_output(&simulated, commands.len());
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(results[0].as_ref().expect("ok").trimmed(), "web-01");
        assert!(
            results[1]
                .as_ref()
                .expect("ok")
                .stdout
                .contains("MemAvailable")
        );
    }
}
