//! Filesystem usage from `df`.

use crate::parse::split_n;
use vds_domain::ids::CollectorId;
use vds_domain::ports::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, TransportError,
};
use vds_domain::server::FilesystemUsage;

/// Pseudo-filesystems that are not worth alerting on.
///
/// `tmpfs` at 100% is normal and says nothing about the disk; `overlay` double-counts
/// the backing filesystem on container hosts; the rest are kernel interfaces with no
/// meaningful capacity.
const PSEUDO_FILESYSTEMS: &[&str] = &[
    "tmpfs",
    "devtmpfs",
    "devfs",
    "sysfs",
    "proc",
    "procfs",
    "squashfs",
    "overlay",
    "aufs",
    "ramfs",
    "cgroup",
    "cgroup2",
    "efivarfs",
    "debugfs",
    "tracefs",
    "mqueue",
    "hugetlbfs",
    "fuse.gvfsd-fuse",
    "fuse.portal",
    "nsfs",
    "autofs",
    "binfmt_misc",
    "configfs",
    "pstore",
    "securityfs",
];

/// Mount-point prefixes to ignore even when the filesystem type is unknown.
const PSEUDO_MOUNTS: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/snap",
    "/var/lib/docker/",
    "/var/snap",
];

/// Reads mounted filesystem usage.
///
/// `-P` requests POSIX output, which guarantees one line per filesystem — without it,
/// long device names wrap onto a second line and the parse falls apart. `-k` fixes the
/// block size at 1024 bytes, because the default varies between distributions and
/// `POSIXLY_CORRECT` settings. `-T` adds the type column but is a GNU extension, so the
/// command degrades to plain `-Pk` where it is unsupported and the parser handles both
/// shapes.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiskCollector;

impl Collector for DiskCollector {
    fn id(&self) -> CollectorId {
        CollectorId::new("disk")
    }

    fn requires(&self) -> &'static [Capability] {
        &[Capability::CoreUtils]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command::shell("df -PkT 2>/dev/null || df -Pk")]
    }

    fn parse(
        &self,
        outputs: &[Result<CommandOutput, TransportError>],
    ) -> Result<CollectorOutput, CollectError> {
        let id = self.id();
        let output = outputs
            .first()
            .ok_or_else(|| CollectError::parse(&id, "no output for df"))?
            .as_ref()
            .map_err(|e| CollectError::Transport(e.clone()))?;

        if !output.is_success() {
            return Err(CollectError::parse(
                &id,
                format!("df failed: {}", output.stderr.trim()),
            ));
        }

        // Parse and filter in separate steps, so "df printed something we could not
        // read" stays distinguishable from "this host only has tmpfs mounts". The first
        // is a parser bug worth surfacing; the second is a legitimately empty result.
        let all_rows = parse_df_unfiltered(&output.stdout);
        if all_rows.is_empty() && !output.stdout.trim().is_empty() {
            return Err(CollectError::parse(&id, "df produced no recognisable rows"));
        }
        let filesystems = all_rows.into_iter().filter(is_real_filesystem).collect();
        Ok(CollectorOutput::Filesystems(filesystems))
    }
}

/// Parses `df -Pk` or `df -PkT` output, keeping only real filesystems.
pub fn parse_df(text: &str) -> Vec<FilesystemUsage> {
    parse_df_unfiltered(text)
        .into_iter()
        .filter(is_real_filesystem)
        .collect()
}

/// Parses every row `df` printed, including pseudo-filesystems.
///
/// Kept separate from [`parse_df`] so that callers can tell an unreadable `df` from one
/// whose rows were all legitimately filtered out.
pub fn parse_df_unfiltered(text: &str) -> Vec<FilesystemUsage> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());

    // The header tells us whether the type column is present. `df -PkT` prints
    // "Filesystem Type 1024-blocks ..."; `df -Pk` prints "Filesystem 1024-blocks ...".
    let has_type = match lines.next() {
        Some(header) => {
            let lower = header.to_ascii_lowercase();
            if !lower.starts_with("filesystem") {
                // No header at all: assume the narrow form and re-process this line.
                return parse_rows(text.lines(), false);
            }
            lower.split_whitespace().nth(1) == Some("type")
        }
        None => return Vec::new(),
    };

    parse_rows(lines, has_type)
}

fn parse_rows<'a>(lines: impl Iterator<Item = &'a str>, has_type: bool) -> Vec<FilesystemUsage> {
    // Columns before the mount point: device [type] blocks used available capacity.
    let leading = if has_type { 6 } else { 5 };

    lines
        .filter(|line| !line.trim().is_empty())
        .filter(|line| !line.to_ascii_lowercase().starts_with("filesystem"))
        .filter_map(|line| {
            // The mount point is everything after the fixed columns, because it may
            // contain spaces.
            let (fields, mount_point) = split_n(line, leading)?;
            let device = fields.first()?;
            let (fs_type, offset) = if has_type {
                (Some(*fields.get(1)?), 2)
            } else {
                (None, 1)
            };

            let total_kb: u64 = fields.get(offset)?.parse().ok()?;
            let used_kb: u64 = fields.get(offset + 1)?.parse().ok()?;
            let available_kb: u64 = fields.get(offset + 2)?.parse().ok()?;

            let mount_point = mount_point.trim();
            if mount_point.is_empty() {
                return None;
            }

            Some(FilesystemUsage {
                mount_point: mount_point.to_owned(),
                device: Some((*device).to_owned()),
                filesystem: fs_type.map(str::to_owned),
                total_bytes: total_kb.saturating_mul(1_024),
                used_bytes: used_kb.saturating_mul(1_024),
                available_bytes: available_kb.saturating_mul(1_024),
            })
        })
        .collect()
}

/// Whether a filesystem is worth reporting.
///
/// Takes the value by reference-in-closure form so it composes with `Iterator::filter`
/// in both the borrowed and owned cases.
pub fn is_real_filesystem(fs: &FilesystemUsage) -> bool {
    // A zero-capacity filesystem cannot have a usage percentage.
    if fs.total_bytes == 0 {
        return false;
    }
    if let Some(kind) = &fs.filesystem
        && PSEUDO_FILESYSTEMS
            .iter()
            .any(|p| kind.eq_ignore_ascii_case(p))
    {
        return false;
    }
    // Without a type column, fall back to the mount point. `/dev/shm` and `/run/...`
    // are tmpfs; `/snap/...` are squashfs images that are always 100% full.
    if PSEUDO_MOUNTS.iter().any(|prefix| {
        fs.mount_point == *prefix || fs.mount_point.starts_with(&format!("{prefix}/"))
    }) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::metrics::MetricValue;

    const DF_WITH_TYPE: &str = "\
Filesystem     Type     1024-blocks     Used Available Capacity Mounted on
udev           devtmpfs     8123456        0   8123456       0% /dev
tmpfs          tmpfs        1633360     2108   1631252       1% /run
/dev/nvme0n1p2 ext4        491222444 61234567 405123456      14% /
/dev/nvme0n1p1 vfat          523248     6220    517028       2% /boot/efi
/dev/sdb1      xfs         976761560 927923482  48838078      95% /var/lib/data
/dev/loop0     squashfs       64128    64128         0     100% /snap/core20/2015
tmpfs          tmpfs        1633360       48   1633312       1% /run/user/1000";

    const DF_WITHOUT_TYPE: &str = "\
Filesystem     1024-blocks     Used Available Capacity Mounted on
/dev/root         41151808 12345678  26789012      32% /
tmpfs               509416        0    509416       0% /dev/shm";

    #[test]
    fn the_gnu_form_with_a_type_column_parses() {
        let filesystems = parse_df(DF_WITH_TYPE);
        let mounts: Vec<&str> = filesystems.iter().map(|f| f.mount_point.as_str()).collect();
        assert_eq!(mounts, vec!["/", "/boot/efi", "/var/lib/data"]);
    }

    #[test]
    fn pseudo_filesystems_are_excluded() {
        let filesystems = parse_df(DF_WITH_TYPE);
        // tmpfs at 100% would otherwise trigger a permanent disk alert.
        assert!(filesystems.iter().all(|f| f.mount_point != "/run"));
        assert!(filesystems.iter().all(|f| f.mount_point != "/dev"));
        // A snap image is always exactly 100% full by construction.
        assert!(
            filesystems
                .iter()
                .all(|f| !f.mount_point.starts_with("/snap"))
        );
    }

    #[test]
    fn the_posix_form_without_a_type_column_parses() {
        let filesystems = parse_df(DF_WITHOUT_TYPE);
        assert_eq!(filesystems.len(), 1);
        let root = &filesystems[0];
        assert_eq!(root.mount_point, "/");
        assert_eq!(root.device.as_deref(), Some("/dev/root"));
        assert_eq!(root.filesystem, None);
        assert_eq!(root.total_bytes, 41_151_808 * 1_024);
    }

    #[test]
    fn dev_shm_is_excluded_even_without_a_type_column() {
        let filesystems = parse_df(DF_WITHOUT_TYPE);
        assert!(filesystems.iter().all(|f| f.mount_point != "/dev/shm"));
    }

    #[test]
    fn sizes_are_converted_from_kibibytes_to_bytes() {
        let filesystems = parse_df(DF_WITHOUT_TYPE);
        let root = &filesystems[0];
        assert_eq!(root.used_bytes, 12_345_678 * 1_024);
        assert_eq!(root.available_bytes, 26_789_012 * 1_024);
        let percent = root.used_percent().value().expect("percentage");
        assert!((percent - 30.0).abs() < 1.0, "unexpected {percent}%");
    }

    #[test]
    fn mount_points_containing_spaces_survive() {
        let text = "\
Filesystem     1024-blocks     Used Available Capacity Mounted on
/dev/sdc1           100000    50000     50000      50% /mnt/my backup drive";
        let filesystems = parse_df(text);
        assert_eq!(filesystems.len(), 1);
        assert_eq!(filesystems[0].mount_point, "/mnt/my backup drive");
    }

    #[test]
    fn a_nearly_full_filesystem_is_the_one_that_dominates() {
        let filesystems = parse_df(DF_WITH_TYPE);
        let worst = filesystems
            .iter()
            .filter_map(|f| f.used_percent().value())
            .fold(0.0_f64, f64::max);
        assert!(worst > 94.0 && worst < 96.0, "unexpected worst {worst}%");
    }

    #[test]
    fn zero_capacity_rows_are_dropped_rather_than_dividing_by_zero() {
        let text = "\
Filesystem     1024-blocks     Used Available Capacity Mounted on
none                     0        0         0       -  /weird";
        assert!(parse_df(text).is_empty());
    }

    #[test]
    fn malformed_rows_are_skipped_not_fatal() {
        let text = "\
Filesystem     1024-blocks     Used Available Capacity Mounted on
/dev/sda1         41151808 12345678  26789012      32% /
this line is garbage
/dev/sda2          1000000   500000    500000      50% /home";
        let filesystems = parse_df(text);
        let mounts: Vec<&str> = filesystems.iter().map(|f| f.mount_point.as_str()).collect();
        assert_eq!(mounts, vec!["/", "/home"]);
    }

    #[test]
    fn output_with_no_header_still_parses() {
        let text = "/dev/sda1         41151808 12345678  26789012      32% /";
        let filesystems = parse_df(text);
        assert_eq!(filesystems.len(), 1);
        assert_eq!(filesystems[0].mount_point, "/");
    }

    #[test]
    fn a_failed_df_is_reported_rather_than_returning_an_empty_disk_list() {
        // Returning an empty list would silently look like "no disks", which reads as
        // healthy. It must be an error.
        let err = DiskCollector
            .parse(&[Ok(CommandOutput::failure(127, "df: not found"))])
            .expect_err("must fail");
        assert!(matches!(err, CollectError::Parse { .. }));
    }

    #[test]
    fn unfiltered_parsing_keeps_the_pseudo_rows() {
        // The distinction the collector relies on to tell a broken df from a tmpfs-only
        // host.
        let all = parse_df_unfiltered(DF_WITH_TYPE);
        assert_eq!(all.len(), 7);
        assert_eq!(parse_df(DF_WITH_TYPE).len(), 3);
    }

    #[test]
    fn a_host_with_only_pseudo_filesystems_yields_an_empty_list_without_error() {
        let only_tmpfs = "\
Filesystem     Type     1024-blocks     Used Available Capacity Mounted on
tmpfs          tmpfs        1633360     2108   1631252       1% /run";
        let output = DiskCollector
            .parse(&[Ok(CommandOutput::success(only_tmpfs))])
            .expect("parses");
        let CollectorOutput::Filesystems(filesystems) = output else {
            panic!("expected filesystem output")
        };
        assert!(filesystems.is_empty());
    }

    #[test]
    fn usage_percentage_is_computed_from_bytes_not_from_the_capacity_column() {
        // df's own Capacity column rounds and, on some systems, excludes reserved
        // blocks; computing from used/total keeps it consistent with the thresholds.
        let fs = FilesystemUsage {
            mount_point: "/".into(),
            device: None,
            filesystem: None,
            total_bytes: 200,
            used_bytes: 150,
            available_bytes: 50,
        };
        assert_eq!(fs.used_percent(), MetricValue::Available(75.0));
    }
}
