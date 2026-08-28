//! Configuration migrations.
//!
//! Settings files outlive the code that wrote them. When a field is renamed or a section
//! restructured, a migration step brings the old shape forward rather than the loader
//! quietly falling back to defaults — which would silently discard a user's tuning.
//!
//! Each step takes the raw TOML value at version `N` and returns it at `N + 1`. Steps
//! are applied in order until the file reaches [`CONFIG_VERSION`].

use super::{CONFIG_VERSION, ConfigError};

/// What migration did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationOutcome {
    /// Version the file was at before migrating.
    pub from: u32,
    /// Version it is at now.
    pub to: u32,
    /// Human-readable description of each step applied.
    pub steps: Vec<&'static str>,
}

impl MigrationOutcome {
    pub fn is_noop(&self) -> bool {
        self.steps.is_empty()
    }
}

/// One migration step.
struct Step {
    /// Version this step upgrades *from*.
    from: u32,
    description: &'static str,
    apply: fn(toml::Value) -> Result<toml::Value, ConfigError>,
}

/// Every known step, ordered by `from`.
///
/// Empty at v1 because there is nothing older yet. The machinery exists from the start
/// so that the first real migration is a data change rather than an architecture change.
const STEPS: &[Step] = &[];

/// Brings a raw configuration value up to [`CONFIG_VERSION`].
///
/// A file with no `version` key is assumed to be version 1, which is what the first
/// release wrote.
pub fn migrate(mut value: toml::Value) -> Result<(toml::Value, MigrationOutcome), ConfigError> {
    let from = read_version(&value).unwrap_or(1);

    if from > CONFIG_VERSION {
        return Err(ConfigError::FromTheFuture {
            found: from,
            supported: CONFIG_VERSION,
        });
    }

    let mut steps = Vec::new();
    let mut current = from;

    while current < CONFIG_VERSION {
        let Some(step) = STEPS.iter().find(|s| s.from == current) else {
            return Err(ConfigError::UnknownVersion(current));
        };
        value = (step.apply)(value)?;
        steps.push(step.description);
        current += 1;
        set_version(&mut value, current);
    }

    // Normalise: a file written before versioning gains the key it was missing.
    if read_version(&value).is_none() {
        set_version(&mut value, CONFIG_VERSION);
    }

    Ok((
        value,
        MigrationOutcome {
            from,
            to: current.max(from),
            steps,
        },
    ))
}

fn read_version(value: &toml::Value) -> Option<u32> {
    value
        .get("version")?
        .as_integer()
        .and_then(|v| u32::try_from(v).ok())
}

fn set_version(value: &mut toml::Value, version: u32) {
    if let Some(table) = value.as_table_mut() {
        table.insert(
            "version".to_owned(),
            toml::Value::Integer(i64::from(version)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> toml::Value {
        toml::from_str(text).expect("valid toml")
    }

    #[test]
    fn a_current_file_is_left_alone() {
        let value = parse(&format!(
            "version = {CONFIG_VERSION}\n[monitoring]\ntimeout_secs = 5"
        ));
        let (migrated, outcome) = migrate(value).expect("migrates");
        assert!(outcome.is_noop());
        assert_eq!(outcome.from, CONFIG_VERSION);
        assert_eq!(outcome.to, CONFIG_VERSION);
        assert_eq!(
            migrated
                .get("monitoring")
                .and_then(|m| m.get("timeout_secs")),
            Some(&toml::Value::Integer(5))
        );
    }

    #[test]
    fn a_file_without_a_version_key_is_treated_as_version_one_and_stamped() {
        let (migrated, outcome) = migrate(parse("[logging]\nlevel = \"debug\"")).expect("migrates");
        assert_eq!(outcome.from, 1);
        assert_eq!(read_version(&migrated), Some(CONFIG_VERSION));
        // The user's setting survives.
        assert_eq!(
            migrated.get("logging").and_then(|l| l.get("level")),
            Some(&toml::Value::String("debug".to_owned()))
        );
    }

    #[test]
    fn a_file_from_the_future_is_refused_rather_than_mangled() {
        let value = parse(&format!("version = {}", CONFIG_VERSION + 1));
        let err = migrate(value).expect_err("must refuse");
        assert!(matches!(err, ConfigError::FromTheFuture { .. }));
    }

    // The range is empty while `CONFIG_VERSION` is 1 and there is nothing to migrate
    // from; the test earns its keep the moment a version 2 is added, which is precisely
    // when someone is most likely to forget a step.
    #[allow(clippy::reversed_empty_ranges)]
    #[test]
    fn every_step_is_reachable_from_the_one_before_it() {
        // Guards against adding a step for version 3 while forgetting version 2, which
        // would strand any file at version 2 forever.
        for version in 1..CONFIG_VERSION {
            assert!(
                STEPS.iter().any(|s| s.from == version),
                "no migration step upgrades version {version}"
            );
        }
    }

    #[test]
    fn steps_are_unique_and_ordered() {
        let mut seen: Vec<u32> = STEPS.iter().map(|s| s.from).collect();
        let count = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), count, "two steps migrate from the same version");
        assert!(
            STEPS.windows(2).all(|w| w[0].from < w[1].from),
            "steps are out of order"
        );
    }
}
