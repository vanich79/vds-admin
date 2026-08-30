//! Building and parsing the commands behind the file browser.
//!
//! Pure, like every other module here: these functions produce a [`Command`] and read its
//! output, and perform no I/O of their own. That is what lets the same code serve SSH
//! today and the agent later, and what lets a parser be tested against captured output
//! from a real machine instead of a mock.
//!
//! # Two decisions worth stating
//!
//! **Content travels as base64, in both directions.** Writing a file by interpolating its
//! text into a shell command is a quoting problem with no good solution: newlines, quotes
//! and `$` all mean something, and a configuration file contains all three. Base64 has no
//! metacharacters, so the shell sees one opaque word. Reading uses it for the mirror
//! reason — the bytes arrive intact, and whether they are text is decided here, on real
//! bytes, rather than guessed from a mangled string.
//!
//! **The listing asks for epoch seconds.** `ls` renders dates in the locale's format and
//! drops the year on recent files; parsing that back is guesswork. `--time-style=+%s`
//! makes it a number. It is a GNU coreutils option, which Debian, Ubuntu and RHEL all
//! have and BusyBox does not — [`parse_listing`] says so plainly rather than returning a
//! confidently wrong listing.

use vds_domain::ports::{
    Command, CommandOutput, DirectoryEntry, EntryKind, FileBytes, FileContents, FileError,
    shell_quote,
};

/// Lists one directory.
///
/// `-A` includes dotfiles but not `.` and `..`: `.env` and `.htaccess` are among the more
/// interesting files on a web server, and an administration tool that hid them would be
/// an odd one. `LC_ALL=C` keeps the output in a form this parser understands whatever the
/// server's language is.
pub fn list_command(path: &str) -> Command {
    Command::Shell(format!(
        "LC_ALL=C ls -lA --time-style=+%s -- {} 2>&1",
        shell_quote(path)
    ))
}

/// Reads a file, capped.
///
/// `head -c` stops the read at the far end, so a huge file never crosses the network at
/// all. One extra byte is requested so the caller can tell "exactly the limit" from
/// "longer than the limit" without a second round trip.
pub fn read_command(path: &str, max_bytes: u64) -> Command {
    let quoted = shell_quote(path);
    Command::Shell(format!(
        "if [ ! -e {quoted} ]; then echo __VDS_MISSING__; \
         elif [ -d {quoted} ]; then echo __VDS_ISDIR__; \
         elif [ ! -r {quoted} ]; then echo __VDS_DENIED__; \
         else stat -c %s {quoted} 2>/dev/null || wc -c < {quoted}; \
         echo __VDS_BODY__; head -c {} -- {quoted} | base64; fi",
        max_bytes.saturating_add(1)
    ))
}

/// Replaces a file's contents.
///
/// Written beside the target and moved over it, so an interrupted transfer leaves the
/// original intact. A half-written nginx configuration is worse than an unchanged one.
/// The temporary file sits in the same directory because `mv` is only atomic within a
/// filesystem.
pub fn write_command(path: &str, contents: &str) -> Command {
    let quoted = shell_quote(path);
    let encoded = shell_quote(&base64_encode(contents.as_bytes()));
    Command::Shell(format!(
        "tmp={quoted}.vds-tmp.$$ && \
         printf %s {encoded} | base64 -d > \"$tmp\" && \
         mv -f \"$tmp\" {quoted} || {{ rm -f \"$tmp\"; exit 1; }}"
    ))
}

/// Deletes a file or an empty directory.
///
/// `rmdir` rather than `rm -r`: a recursive delete driven by a path from a text field is
/// how this kind of tool destroys a machine. Removing a full directory is a deliberate
/// sequence of steps the user can see, not one click.
pub fn delete_command(path: &str) -> Command {
    let quoted = shell_quote(path);
    Command::Shell(format!(
        "if [ -d {quoted} ]; then rmdir -- {quoted}; else rm -f -- {quoted}; fi"
    ))
}

/// Creates a directory and any missing parents.
pub fn create_directory_command(path: &str) -> Command {
    Command::Shell(format!("mkdir -p -- {}", shell_quote(path)))
}

/// Turns `ls -lA --time-style=+%s` output into entries.
pub fn parse_listing(output: &CommandOutput, path: &str) -> Result<Vec<DirectoryEntry>, FileError> {
    let text = output.stdout.trim();

    if let Some(error) = listing_failure(text, path) {
        return Err(error);
    }

    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        // `total 48` heads a GNU listing, and blank lines separate nothing useful.
        if line.is_empty() || line.starts_with("total ") {
            continue;
        }
        match parse_entry(line) {
            Some(entry) => entries.push(entry),
            None => {
                return Err(FileError::Malformed(format!(
                    "could not read a directory listing; the server's `ls` may not support \
                     --time-style: {line}"
                )));
            }
        }
    }

    // Directories first, then by name: the order a person reading a tree expects, and
    // `ls` does not guarantee it.
    entries.sort_by(|a, b| {
        b.kind
            .is_directory()
            .cmp(&a.kind.is_directory())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Recognises the shell's own complaints, which arrive on stdout because of `2>&1`.
fn listing_failure(text: &str, path: &str) -> Option<FileError> {
    let lowered = text.to_lowercase();
    if lowered.contains("no such file or directory") {
        Some(FileError::NotFound(path.to_owned()))
    } else if lowered.contains("permission denied") {
        Some(FileError::PermissionDenied(path.to_owned()))
    } else if lowered.contains("not a directory") {
        Some(FileError::NotADirectory(path.to_owned()))
    } else {
        None
    }
}

/// One line of a listing.
fn parse_entry(line: &str) -> Option<DirectoryEntry> {
    // mode links owner group size epoch name…
    //
    // Split by hand rather than with `splitn`: `ls` pads its columns, so the separators
    // are runs of spaces, and `splitn` would spend its limit on the empty pieces between
    // them. The name is whatever is left, which is how a name containing spaces survives.
    let mut rest = line;
    let mut fields = [""; 6];
    for field in &mut fields {
        let trimmed = rest.trim_start();
        let end = trimmed.find(char::is_whitespace)?;
        *field = &trimmed[..end];
        rest = &trimmed[end..];
    }
    let [mode, _links, owner, group, size, epoch] = fields;
    let name = rest.trim_start();

    // A mode string is ten characters: type plus three triples. Anything else means the
    // line is not what this parser was built for.
    if mode.len() < 10 {
        return None;
    }
    let kind = match mode.as_bytes().first()? {
        b'-' => EntryKind::File,
        b'd' => EntryKind::Directory,
        b'l' => EntryKind::Symlink,
        _ => EntryKind::Other,
    };

    // A symlink line ends with ` -> target`, and a file may legitimately contain that
    // sequence in its name, so only a link is split on it.
    let (name, target) = match kind {
        EntryKind::Symlink => match name.split_once(" -> ") {
            Some((name, target)) => (name.to_owned(), Some(target.to_owned())),
            None => (name.to_owned(), None),
        },
        _ => (name.to_owned(), None),
    };

    if name.is_empty() {
        return None;
    }

    Some(DirectoryEntry {
        name,
        kind,
        size_bytes: size.parse().ok()?,
        // A timestamp that is not a number means the layout is not the one that was
        // asked for -- BusyBox's `ls`, most likely -- and the rest of the line cannot be
        // trusted either, so the whole listing is rejected rather than half-read.
        modified: chrono::DateTime::from_timestamp(epoch.parse::<i64>().ok()?, 0),
        mode: mode[1..].to_owned(),
        owner: owner.to_owned(),
        group: group.to_owned(),
        target,
    })
}

/// Turns the read command's output into raw bytes.
///
/// The general form. [`parse_read`] is this plus the decision that what came back is
/// text — a decision that has to be made *after* the bytes exist, not from a filename.
pub fn parse_read_bytes(
    output: &CommandOutput,
    path: &str,
    max_bytes: u64,
) -> Result<FileBytes, FileError> {
    let text = output.stdout.trim();

    if text.contains("__VDS_MISSING__") {
        return Err(FileError::NotFound(path.to_owned()));
    }
    if text.contains("__VDS_ISDIR__") {
        return Err(FileError::NotAFile(path.to_owned()));
    }
    if text.contains("__VDS_DENIED__") {
        return Err(FileError::PermissionDenied(path.to_owned()));
    }

    let (header, body) = text
        .split_once("__VDS_BODY__")
        .ok_or_else(|| FileError::Malformed("the read did not complete".to_owned()))?;

    let size_bytes: u64 = header.trim().parse().unwrap_or(0);
    let mut bytes = base64_decode(&body.replace(['\n', '\r'], ""))
        .ok_or_else(|| FileError::Malformed("the file did not arrive intact".to_owned()))?;

    let truncated = size_bytes > max_bytes;
    // The extra byte was requested only to detect truncation; it is not part of what the
    // caller asked for.
    bytes.truncate(max_bytes as usize);

    Ok(FileBytes {
        bytes,
        truncated,
        size_bytes,
    })
}

/// Turns the read command's output into file contents, if it is text at all.
pub fn parse_read(
    output: &CommandOutput,
    path: &str,
    max_bytes: u64,
) -> Result<FileContents, FileError> {
    parse_read_bytes(output, path, max_bytes)?.into_text(path)
}

/// Reads the outcome of a write, delete or mkdir.
pub fn parse_action(output: &CommandOutput, path: &str) -> Result<(), FileError> {
    if output.exit_code == 0 {
        return Ok(());
    }
    let combined = format!("{} {}", output.stdout, output.stderr).to_lowercase();

    if combined.contains("permission denied") {
        Err(FileError::PermissionDenied(path.to_owned()))
    } else if combined.contains("no such file") {
        Err(FileError::NotFound(path.to_owned()))
    } else if combined.contains("directory not empty") {
        Err(FileError::Malformed(format!(
            "{path} is not empty; remove what is inside it first"
        )))
    } else {
        Err(FileError::Malformed(
            combined.trim().chars().take(200).collect(),
        ))
    }
}

// --- base64 ---------------------------------------------------------------------------
//
// Hand-written rather than a dependency: it is twenty lines, this crate is compiled into
// the agent where every kilobyte is counted, and the alphabet has not changed since 1987.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let triple = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);

        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;

    for byte in input.bytes() {
        if byte == b'=' || byte.is_ascii_whitespace() {
            continue;
        }
        let value = ALPHABET.iter().position(|c| *c == byte)? as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(stdout: &str) -> CommandOutput {
        CommandOutput::success(stdout)
    }

    // --- commands ---------------------------------------------------------------------

    #[test]
    fn every_command_quotes_the_path_it_was_given() {
        // The path comes from a text field, so this is the boundary between what a user
        // types and what runs as root. `shell_quote` is tested exhaustively in the domain;
        // this checks that nothing here forgets to call it.
        let attack = "/var/www; rm -rf /";
        let commands = [
            list_command(attack),
            read_command(attack, 1024),
            write_command(attack, "hello"),
            delete_command(attack),
            create_directory_command(attack),
        ];

        let quoted = shell_quote(attack);
        for command in commands {
            let shell = command.to_shell();
            assert!(shell.contains(&quoted), "the path was not quoted: {shell}");
            // The payload does appear in the command — that is the point of a path. What
            // matters is that every occurrence of it sits inside the quoted form, so the
            // shell reads all of it as one literal word.
            assert_eq!(
                shell.matches(attack).count(),
                shell.matches(quoted.as_str()).count(),
                "the payload appears somewhere unquoted: {shell}"
            );
        }
    }

    #[test]
    fn deleting_never_recurses() {
        // A recursive delete driven by a path from a text field is how a tool like this
        // destroys a machine.
        let shell = delete_command("/var/www").to_shell();
        assert!(shell.contains("rmdir"));
        assert!(
            !shell.contains("rm -r"),
            "a recursive delete crept in: {shell}"
        );
        assert!(!shell.contains("-rf /"), "{shell}");
    }

    #[test]
    fn writing_goes_through_a_temporary_file() {
        // An interrupted write must leave the original intact; a half-written nginx
        // configuration is worse than an unchanged one.
        let shell = write_command("/etc/nginx/nginx.conf", "server {}").to_shell();
        assert!(shell.contains("vds-tmp"), "{shell}");
        assert!(shell.contains("mv -f"), "{shell}");
        // And the content never appears as shell syntax.
        assert!(
            !shell.contains("server {}"),
            "content leaked into the command"
        );
    }

    #[test]
    fn content_travels_as_base64_so_the_shell_sees_no_syntax() {
        // A configuration file is full of quotes, newlines and dollar signs.
        let nasty = "server {\n  root '/var/www';\n  # $HOME `id`\n}\n";
        let shell = write_command("/tmp/x", nasty).to_shell();

        for fragment in ["`id`", "$HOME", "root '/var/www'"] {
            assert!(!shell.contains(fragment), "{fragment} reached the shell");
        }
        assert!(shell.contains("base64 -d"));
    }

    #[test]
    fn a_read_asks_for_one_byte_more_than_the_limit() {
        // So "exactly the limit" and "longer than the limit" are distinguishable without
        // a second round trip.
        assert!(
            read_command("/var/log/syslog", 1000)
                .to_shell()
                .contains("head -c 1001")
        );
    }

    // --- listings ---------------------------------------------------------------------

    const LISTING: &str = "total 24\n\
        drwxr-xr-x 2 root     root     4096 1756543210 html\n\
        -rw-r--r-- 1 www-data www-data 1234 1756543100 index.php\n\
        -rw------- 1 root     root      120 1756543000 .env\n\
        lrwxrwxrwx 1 root     root       12 1756542900 current -> /var/www/v2\n";

    #[test]
    fn a_listing_is_parsed_into_entries() {
        let entries = parse_listing(&ok(LISTING), "/var/www").expect("parses");
        assert_eq!(entries.len(), 4);

        // Directories first, then alphabetical — the order a person expects.
        assert_eq!(entries[0].name, "html");
        assert!(entries[0].kind.is_directory());

        let env = entries
            .iter()
            .find(|e| e.name == ".env")
            .expect("hidden file listed");
        assert_eq!(env.size_bytes, 120);
        assert_eq!(env.mode, "rw-------");
        assert_eq!(env.owner, "root");
        assert!(env.is_hidden());
    }

    #[test]
    fn a_symlink_keeps_its_target_unresolved() {
        // A link pointing somewhere unexpected is exactly what an operator wants to see.
        let entries = parse_listing(&ok(LISTING), "/var/www").expect("parses");
        let link = entries
            .iter()
            .find(|e| e.name == "current")
            .expect("link listed");

        assert_eq!(link.kind, EntryKind::Symlink);
        assert_eq!(link.target.as_deref(), Some("/var/www/v2"));
    }

    #[test]
    fn a_name_containing_spaces_survives() {
        let listing = "-rw-r--r-- 1 root root 10 1756543210 my report final.txt\n";
        let entries = parse_listing(&ok(listing), "/tmp").expect("parses");
        assert_eq!(entries[0].name, "my report final.txt");
    }

    #[test]
    fn an_impossible_timestamp_is_absent_rather_than_nineteen_seventy() {
        // Defaulting to the epoch would render as a real date and be believed. A
        // number outside the calendar's range is left empty instead.
        let listing = "-rw-r--r-- 1 root root 10 99999999999999 file.txt\n";
        let entries = parse_listing(&ok(listing), "/tmp").expect("parses");
        assert_eq!(entries[0].modified, None);
    }

    #[test]
    fn the_shells_own_complaints_become_proper_errors() {
        let cases = [
            (
                "ls: cannot access '/nope': No such file or directory",
                "not_found",
            ),
            (
                "ls: cannot open directory '/root': Permission denied",
                "permission_denied",
            ),
            ("ls: /etc/passwd: Not a directory", "not_a_directory"),
        ];
        for (output, expected) in cases {
            let error = parse_listing(&ok(output), "/x").expect_err("must fail");
            assert_eq!(error.kind(), expected, "for {output}");
        }
    }

    #[test]
    fn a_listing_from_a_shell_without_the_option_says_so_rather_than_guessing() {
        // BusyBox has no --time-style. Returning a confidently wrong listing would be
        // worse than admitting the limitation.
        let busybox = "-rw-r--r--    1 root     root            10 Aug 30 12:00 file.txt\n";
        let error = parse_listing(&ok(busybox), "/tmp").expect_err("must fail");
        assert_eq!(error.kind(), "malformed");
        assert!(error.to_string().contains("--time-style"), "{error}");
    }

    // --- reading ----------------------------------------------------------------------

    #[test]
    fn a_file_round_trips_through_base64() {
        let body = base64_encode(b"hello\nworld\n");
        let output = ok(&format!("12\n__VDS_BODY__\n{body}"));

        let contents = parse_read(&output, "/tmp/x", 1024).expect("reads");
        assert_eq!(contents.text, "hello\nworld\n");
        assert!(!contents.truncated);
        assert_eq!(contents.size_bytes, 12);
    }

    #[test]
    fn a_file_longer_than_the_limit_is_marked_truncated() {
        // An editor that silently opens the first part of a file and then saves it would
        // destroy the rest, so the interface has to know.
        let body = base64_encode(b"abcd");
        let output = ok(&format!("999999\n__VDS_BODY__\n{body}"));

        let contents = parse_read(&output, "/var/log/syslog", 4).expect("reads");
        assert!(contents.truncated);
        assert_eq!(contents.size_bytes, 999_999);
    }

    #[test]
    fn a_binary_file_is_refused_rather_than_shown_as_rubbish() {
        // Showing it would be meaningless and saving it back would corrupt it.
        let body = base64_encode(&[0x7f, 0x45, 0x4c, 0x46, 0x00, 0xff, 0xfe]);
        let output = ok(&format!("7\n__VDS_BODY__\n{body}"));

        let error = parse_read(&output, "/bin/ls", 1024).expect_err("must refuse");
        assert_eq!(error.kind(), "not_text");
    }

    #[test]
    fn the_read_probes_report_what_went_wrong() {
        for (marker, expected) in [
            ("__VDS_MISSING__", "not_found"),
            ("__VDS_ISDIR__", "not_a_file"),
            ("__VDS_DENIED__", "permission_denied"),
        ] {
            let error = parse_read(&ok(marker), "/x", 10).expect_err("must fail");
            assert_eq!(error.kind(), expected);
        }
    }

    // --- actions ----------------------------------------------------------------------

    #[test]
    fn a_successful_action_is_silent() {
        assert!(parse_action(&CommandOutput::success(""), "/tmp/x").is_ok());
    }

    #[test]
    fn a_failed_action_explains_itself() {
        let denied = CommandOutput {
            stdout: String::new(),
            stderr: "rm: cannot remove '/etc/passwd': Permission denied".into(),
            exit_code: 1,
        };
        assert_eq!(
            parse_action(&denied, "/etc/passwd")
                .expect_err("fails")
                .kind(),
            "permission_denied"
        );

        let full = CommandOutput {
            stdout: String::new(),
            stderr: "rmdir: failed to remove '/var/www': Directory not empty".into(),
            exit_code: 1,
        };
        let error = parse_action(&full, "/var/www").expect_err("fails");
        assert!(error.to_string().contains("not empty"), "{error}");
    }

    // --- base64 -----------------------------------------------------------------------

    #[test]
    fn base64_matches_the_standard_vectors() {
        // RFC 4648 §10. A hand-written codec earns its place only if it is right.
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(
                base64_encode(input.as_bytes()),
                expected,
                "encoding {input:?}"
            );
            assert_eq!(
                base64_decode(expected).as_deref(),
                Some(input.as_bytes()),
                "decoding {expected:?}"
            );
        }
    }

    #[test]
    fn base64_round_trips_arbitrary_bytes() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        assert_eq!(
            base64_decode(&base64_encode(&bytes)).as_deref(),
            Some(&bytes[..])
        );
    }

    #[test]
    fn base64_rejects_characters_outside_the_alphabet() {
        // Truncated or corrupted output must not silently decode to something plausible.
        assert_eq!(base64_decode("not base64!"), None);
    }
}
