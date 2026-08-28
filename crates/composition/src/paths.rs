//! Where the application keeps its files.
//!
//! One place decides this, so nothing else has to guess — and so the whole layout can be
//! redirected at once for a portable installation or a test.

use std::path::{Path, PathBuf};

/// Directory name used under the platform's data and config roots.
const APPLICATION_DIRECTORY: &str = "vds-admin";

/// Environment variable that overrides the data root.
///
/// Makes portable installations and integration tests possible without special-casing
/// either in the code.
pub const DATA_DIR_ENV: &str = "VDS_ADMIN_DATA_DIR";

/// Every path the application uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub config_file: PathBuf,
    pub database: PathBuf,
    pub screenshots: PathBuf,
    pub logs: PathBuf,
    pub known_hosts: PathBuf,
    pub secrets_vault: PathBuf,
}

impl AppPaths {
    /// Resolves paths for this machine.
    ///
    /// Falls back to a directory beside the executable when the platform reports no data
    /// directory — which happens on some stripped-down Linux setups, and would otherwise
    /// stop the application starting at all.
    pub fn discover() -> Self {
        if let Some(root) = std::env::var_os(DATA_DIR_ENV) {
            return Self::rooted(PathBuf::from(root));
        }

        let root = dirs::data_dir()
            .map(|dir| dir.join(APPLICATION_DIRECTORY))
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(Path::to_path_buf))
                    .map(|dir| dir.join("data"))
            })
            .unwrap_or_else(|| PathBuf::from(APPLICATION_DIRECTORY));

        Self::rooted(root)
    }

    /// Puts everything under one root.
    pub fn rooted(root: impl Into<PathBuf>) -> Self {
        let data_dir: PathBuf = root.into();
        Self {
            config_file: data_dir.join("config.toml"),
            database: data_dir.join("vds-admin.db"),
            screenshots: data_dir.join("screenshots"),
            logs: data_dir.join("logs"),
            known_hosts: data_dir.join("known_hosts.json"),
            secrets_vault: data_dir.join("secrets.vault"),
            data_dir,
        }
    }

    /// Creates every directory the application writes to.
    pub fn ensure(&self) -> std::io::Result<()> {
        for directory in [&self.data_dir, &self.screenshots, &self.logs] {
            std::fs::create_dir_all(directory)?;
        }
        Ok(())
    }
}

impl Default for AppPaths {
    fn default() -> Self {
        Self::discover()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_lives_under_one_root() {
        let paths = AppPaths::rooted("/data/vds");

        for path in [
            &paths.config_file,
            &paths.database,
            &paths.screenshots,
            &paths.logs,
            &paths.known_hosts,
            &paths.secrets_vault,
        ] {
            assert!(path.starts_with("/data/vds"), "{path:?} escaped the root");
        }
    }

    #[test]
    fn the_paths_are_distinct() {
        // A collision would mean one file silently overwriting another.
        let paths = AppPaths::rooted("/data/vds");
        let all = [
            paths.config_file.clone(),
            paths.database.clone(),
            paths.screenshots.clone(),
            paths.logs.clone(),
            paths.known_hosts.clone(),
            paths.secrets_vault.clone(),
        ];
        let mut unique = all.clone().to_vec();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), all.len());
    }

    #[test]
    fn discovery_always_produces_a_usable_layout() {
        // Even on a machine where the platform reports no data directory.
        let paths = AppPaths::discover();
        assert!(!paths.data_dir.as_os_str().is_empty());
        assert!(paths.database.starts_with(&paths.data_dir));
    }

    #[test]
    fn ensure_creates_the_directories_it_needs() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::rooted(dir.path().join("nested"));

        paths.ensure().expect("creates");

        assert!(paths.data_dir.is_dir());
        assert!(paths.screenshots.is_dir());
        assert!(paths.logs.is_dir());
        // Files are not created, only their directories.
        assert!(!paths.database.exists());
    }

    #[test]
    fn ensure_is_idempotent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = AppPaths::rooted(dir.path());
        paths.ensure().expect("creates");
        paths.ensure().expect("creates again");
    }
}
