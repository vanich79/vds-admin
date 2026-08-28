//! Host identity: hostname, OS, kernel, architecture, CPU model and core count.

use crate::parse::{key_value, unquote};
use vds_domain::ids::CollectorId;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};
use vds_domain::server::SystemInfo;

/// Collects the static facts about a machine.
///
/// Every source is optional: a container without `/etc/os-release`, a busybox host
/// without `nproc`, and a kernel without `/proc/cpuinfo` model names all still produce a
/// usable — just less complete — result. This collector never fails the cycle, because
/// not knowing the distribution name is not a monitoring failure.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCollector;

/// Indices into [`SystemCollector::commands`].
const HOSTNAME: usize = 0;
const OS_RELEASE: usize = 1;
const UNAME: usize = 2;
const CPUINFO: usize = 3;

impl Collector for SystemCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("system")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::ProcFs]
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            // Reading the file avoids depending on a `hostname` binary, which busybox
            // and minimal images do not always ship.
            Command::read("/proc/sys/kernel/hostname"),
            Command::shell("cat /etc/os-release 2>/dev/null || true"),
            Command::shell("uname -s -r -m"),
            Command::read("/proc/cpuinfo"),
        ]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let mut info = SystemInfo::default();

        if let Some(text) = ok_stdout(outputs, HOSTNAME) {
            let hostname = text.trim();
            if !hostname.is_empty() {
                info.hostname = Some(hostname.to_owned());
            }
        }

        if let Some(text) = ok_stdout(outputs, OS_RELEASE) {
            let release = parse_os_release(text);
            info.os_name = release.name;
            info.os_version = release.version;
        }

        if let Some(text) = ok_stdout(outputs, UNAME) {
            let uname = parse_uname(text);
            info.kernel = uname.kernel;
            info.architecture = uname.architecture;
            // `uname -s` is the fallback OS name for hosts without /etc/os-release.
            if info.os_name.is_none() {
                info.os_name = uname.os_name;
            }
        }

        if let Some(text) = ok_stdout(outputs, CPUINFO) {
            info.cpu_model = parse_cpu_model(text);
            info.cpu_cores = count_processors(text);
        }

        Ok(CollectorOutput::System(info))
    }
}

/// Returns stdout for a command that succeeded, or `None` for anything else.
fn ok_stdout(outputs: &[Result<CommandOutput, TransportError>], index: usize) -> Option<&str> {
    match outputs.get(index) {
        Some(Ok(output)) if output.is_success() => Some(&output.stdout),
        _ => None,
    }
}

/// What `/etc/os-release` told us.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OsRelease {
    pub name: Option<String>,
    pub version: Option<String>,
}

/// Parses `/etc/os-release`.
///
/// Prefers `PRETTY_NAME` for display but falls back to `NAME`, and takes the version
/// from `VERSION_ID` (stable, machine-readable) rather than `VERSION` (free text).
pub fn parse_os_release(text: &str) -> OsRelease {
    let mut name = None;
    let mut pretty = None;
    let mut version = None;

    for line in text.lines() {
        let Some((key, value)) = key_value(line, '=') else {
            continue;
        };
        let value = unquote(value.trim());
        if value.is_empty() {
            continue;
        }
        match key {
            "NAME" => name = Some(value.to_owned()),
            "PRETTY_NAME" => pretty = Some(value.to_owned()),
            "VERSION_ID" => version = Some(value.to_owned()),
            _ => {}
        }
    }

    OsRelease {
        name: pretty.or(name),
        version,
    }
}

/// What `uname -s -r -m` told us.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Uname {
    pub os_name: Option<String>,
    pub kernel: Option<String>,
    pub architecture: Option<String>,
}

/// Parses `uname -s -r -m` output: `"Linux 5.15.0-91-generic x86_64"`.
pub fn parse_uname(text: &str) -> Uname {
    let mut fields = text.split_whitespace();
    Uname {
        os_name: fields.next().map(str::to_owned),
        kernel: fields.next().map(str::to_owned),
        architecture: fields.next().map(str::to_owned),
    }
}

/// Extracts a human-readable CPU model from `/proc/cpuinfo`.
///
/// x86 uses `model name`; ARM boards vary, so `Hardware`, `Model` and `cpu model` are
/// all accepted before giving up.
pub fn parse_cpu_model(text: &str) -> Option<String> {
    const KEYS: &[&str] = &["model name", "Model", "Hardware", "cpu model", "cpu"];
    for key in KEYS {
        for line in text.lines() {
            let Some((found, value)) = key_value(line, ':') else {
                continue;
            };
            if found.eq_ignore_ascii_case(key) && !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// Counts `processor` entries in `/proc/cpuinfo`.
pub fn count_processors(text: &str) -> Option<u32> {
    let count = text
        .lines()
        .filter_map(|line| key_value(line, ':'))
        .filter(|(key, _)| key.eq_ignore_ascii_case("processor"))
        .count();
    if count == 0 {
        None
    } else {
        u32::try_from(count).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OS_RELEASE_UBUNTU: &str = r#"PRETTY_NAME="Ubuntu 22.04.3 LTS"
NAME="Ubuntu"
VERSION_ID="22.04"
VERSION="22.04.3 LTS (Jammy Jellyfish)"
ID=ubuntu
ID_LIKE=debian"#;

    const CPUINFO_X86: &str = "\
processor	: 0
vendor_id	: GenuineIntel
model name	: Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz
cpu MHz		: 2400.000

processor	: 1
vendor_id	: GenuineIntel
model name	: Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz
cpu MHz		: 2400.000
";

    const CPUINFO_ARM: &str = "\
processor	: 0
BogoMIPS	: 108.00
Features	: fp asimd evtstrm

processor	: 1
BogoMIPS	: 108.00

Hardware	: BCM2711
Revision	: c03114
Model		: Raspberry Pi 4 Model B Rev 1.4
";

    fn collect(outputs: Vec<Result<CommandOutput, TransportError>>) -> SystemInfo {
        let output = SystemCollector
            .parse(&outputs)
            .expect("system collector never fails");
        let CollectorOutput::System(info) = output else {
            panic!("expected system output")
        };
        info
    }

    fn ok(text: &str) -> Result<CommandOutput, TransportError> {
        Ok(CommandOutput::success(text))
    }

    #[test]
    fn a_complete_linux_host_is_fully_described() {
        let info = collect(vec![
            ok("web-01\n"),
            ok(OS_RELEASE_UBUNTU),
            ok("Linux 5.15.0-91-generic x86_64\n"),
            ok(CPUINFO_X86),
        ]);

        assert_eq!(info.hostname.as_deref(), Some("web-01"));
        assert_eq!(info.os_name.as_deref(), Some("Ubuntu 22.04.3 LTS"));
        assert_eq!(info.os_version.as_deref(), Some("22.04"));
        assert_eq!(info.kernel.as_deref(), Some("5.15.0-91-generic"));
        assert_eq!(info.architecture.as_deref(), Some("x86_64"));
        assert_eq!(
            info.cpu_model.as_deref(),
            Some("Intel(R) Xeon(R) CPU E5-2680 v4 @ 2.40GHz")
        );
        assert_eq!(info.cpu_cores, Some(2));
    }

    #[test]
    fn an_arm_board_reports_its_hardware_string_as_the_cpu_model() {
        let info = collect(vec![
            ok("raspberrypi\n"),
            ok(""),
            ok("Linux 6.1.0-rpi7-rpi-v8 aarch64\n"),
            ok(CPUINFO_ARM),
        ]);
        assert_eq!(info.architecture.as_deref(), Some("aarch64"));
        assert_eq!(info.cpu_cores, Some(2));
        // No `model name` on ARM; `Model` is the useful one here.
        assert_eq!(
            info.cpu_model.as_deref(),
            Some("Raspberry Pi 4 Model B Rev 1.4")
        );
    }

    #[test]
    fn a_host_without_os_release_falls_back_to_uname() {
        let info = collect(vec![
            ok("minimal\n"),
            ok(""),
            ok("Linux 5.10.0 armv7l\n"),
            ok(CPUINFO_X86),
        ]);
        assert_eq!(info.os_name.as_deref(), Some("Linux"));
        assert_eq!(info.os_version, None);
        assert_eq!(info.architecture.as_deref(), Some("armv7l"));
    }

    #[test]
    fn the_collector_never_fails_even_when_every_command_does() {
        // Not knowing the distribution name is not a monitoring failure; failing here
        // would take CPU and memory down with it.
        let info = collect(vec![
            Err(TransportError::Execution("no such file".into())),
            Err(TransportError::Execution("no such file".into())),
            Ok(CommandOutput::failure(127, "uname: not found")),
            Err(TransportError::Execution("no such file".into())),
        ]);
        assert_eq!(info, SystemInfo::default());
    }

    #[test]
    fn missing_outputs_entirely_are_tolerated() {
        let info = collect(vec![]);
        assert_eq!(info, SystemInfo::default());
    }

    #[test]
    fn pretty_name_beats_plain_name() {
        let release = parse_os_release(OS_RELEASE_UBUNTU);
        assert_eq!(release.name.as_deref(), Some("Ubuntu 22.04.3 LTS"));

        let no_pretty = "NAME=\"Alpine Linux\"\nVERSION_ID=3.19.1";
        let release = parse_os_release(no_pretty);
        assert_eq!(release.name.as_deref(), Some("Alpine Linux"));
        assert_eq!(release.version.as_deref(), Some("3.19.1"));
    }

    #[test]
    fn quoted_and_unquoted_os_release_values_both_work() {
        let mixed = "PRETTY_NAME=Debian GNU/Linux 12\nVERSION_ID=\"12\"";
        let release = parse_os_release(mixed);
        assert_eq!(release.name.as_deref(), Some("Debian GNU/Linux 12"));
        assert_eq!(release.version.as_deref(), Some("12"));
    }

    #[test]
    fn empty_hostname_is_reported_as_absent_not_as_an_empty_string() {
        let info = collect(vec![ok("  \n"), ok(""), ok(""), ok("")]);
        assert_eq!(info.hostname, None);
    }

    #[test]
    fn processor_counting_ignores_other_keys() {
        assert_eq!(count_processors(CPUINFO_X86), Some(2));
        assert_eq!(count_processors("no processors here"), None);
        // "processor" as part of another key must not count.
        assert_eq!(count_processors("coprocessor\t: yes"), None);
    }
}
