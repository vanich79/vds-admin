//! The single status model shared by every subsystem.
//!
//! Servers, websites, collectors, services and containers all report the same five
//! states. Because the variants are declared worst-last, aggregating a set of statuses
//! is `max()` — see [`Status::worst_of`].

use serde::{Deserialize, Serialize};
use std::fmt;

/// Health of any monitored subject.
///
/// Ordering is *severity* ordering: `Unknown < Healthy < Warning < Critical < Offline`.
/// `Unknown` sorts lowest so that a subject we have not yet measured never drags an
/// aggregate down, while `Offline` sorts highest so that an unreachable member always
/// dominates.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Never measured, or the measurement was inconclusive.
    #[default]
    Unknown,
    /// Measured and within all configured thresholds.
    Healthy,
    /// Measured and above a warning threshold.
    Warning,
    /// Measured and above a critical threshold.
    Critical,
    /// Unreachable for at least the configured number of consecutive checks.
    Offline,
}

impl Status {
    /// Severity-dominant combination: the worst status in the iterator.
    ///
    /// An empty iterator yields [`Status::Unknown`].
    pub fn worst_of<I: IntoIterator<Item = Status>>(statuses: I) -> Status {
        statuses.into_iter().max().unwrap_or(Status::Unknown)
    }

    /// Whether this status should draw a user's attention.
    pub fn is_problem(self) -> bool {
        matches!(self, Status::Warning | Status::Critical | Status::Offline)
    }

    /// Whether the subject is known to be reachable.
    pub fn is_reachable(self) -> bool {
        matches!(self, Status::Healthy | Status::Warning | Status::Critical)
    }

    /// Stable machine-readable identifier, used for persistence and for the wire format.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unknown => "unknown",
            Status::Healthy => "healthy",
            Status::Warning => "warning",
            Status::Critical => "critical",
            Status::Offline => "offline",
        }
    }

    /// Parses the identifier produced by [`Status::as_str`].
    ///
    /// Unrecognised input maps to [`Status::Unknown`] rather than failing: a status
    /// column written by a newer version of the application must not make an older
    /// version unable to read its own database.
    pub fn from_str_lenient(raw: &str) -> Status {
        match raw {
            "healthy" | "online" => Status::Healthy,
            "warning" => Status::Warning,
            "critical" => Status::Critical,
            "offline" => Status::Offline,
            _ => Status::Unknown,
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A threshold pair used to turn a numeric measurement into a [`Status`].
///
/// `warning` and `critical` are expressed in the metric's own unit. The direction of
/// comparison is chosen by the constructor, because "disk 95% full" and "free memory
/// 2%" are both bad but point in opposite directions.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Threshold {
    pub warning: f64,
    pub critical: f64,
    pub direction: ThresholdDirection,
}

/// Which side of the threshold is unhealthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdDirection {
    /// Higher values are worse (CPU load, disk usage, response time).
    Above,
    /// Lower values are worse (free disk space, days until certificate expiry).
    Below,
}

impl Threshold {
    /// Threshold where exceeding the values is unhealthy.
    pub const fn above(warning: f64, critical: f64) -> Self {
        Self {
            warning,
            critical,
            direction: ThresholdDirection::Above,
        }
    }

    /// Threshold where falling below the values is unhealthy.
    pub const fn below(warning: f64, critical: f64) -> Self {
        Self {
            warning,
            critical,
            direction: ThresholdDirection::Below,
        }
    }

    /// Classifies a measurement.
    ///
    /// A non-finite measurement yields [`Status::Unknown`]; it means the source produced
    /// something we cannot interpret, which is not the same as being healthy.
    pub fn classify(&self, value: f64) -> Status {
        if !value.is_finite() {
            return Status::Unknown;
        }
        match self.direction {
            ThresholdDirection::Above => {
                if value >= self.critical {
                    Status::Critical
                } else if value >= self.warning {
                    Status::Warning
                } else {
                    Status::Healthy
                }
            }
            ThresholdDirection::Below => {
                if value <= self.critical {
                    Status::Critical
                } else if value <= self.warning {
                    Status::Warning
                } else {
                    Status::Healthy
                }
            }
        }
    }

    /// True when the threshold values are ordered consistently with the direction.
    pub fn is_coherent(&self) -> bool {
        match self.direction {
            ThresholdDirection::Above => self.critical >= self.warning,
            ThresholdDirection::Below => self.critical <= self.warning,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_ordering_places_offline_last() {
        assert!(Status::Unknown < Status::Healthy);
        assert!(Status::Healthy < Status::Warning);
        assert!(Status::Warning < Status::Critical);
        assert!(Status::Critical < Status::Offline);
    }

    #[test]
    fn worst_of_picks_the_dominant_status() {
        let combined = Status::worst_of([Status::Healthy, Status::Warning, Status::Healthy]);
        assert_eq!(combined, Status::Warning);
    }

    #[test]
    fn worst_of_empty_is_unknown() {
        assert_eq!(Status::worst_of([]), Status::Unknown);
    }

    #[test]
    fn unknown_never_dominates_a_real_measurement() {
        assert_eq!(
            Status::worst_of([Status::Unknown, Status::Healthy]),
            Status::Healthy
        );
    }

    #[test]
    fn round_trips_through_its_string_form() {
        for status in [
            Status::Unknown,
            Status::Healthy,
            Status::Warning,
            Status::Critical,
            Status::Offline,
        ] {
            assert_eq!(Status::from_str_lenient(status.as_str()), status);
        }
    }

    #[test]
    fn unrecognised_status_string_degrades_to_unknown() {
        assert_eq!(
            Status::from_str_lenient("quantum-superposition"),
            Status::Unknown
        );
    }

    #[test]
    fn above_threshold_classifies_by_severity() {
        let cpu = Threshold::above(80.0, 95.0);
        assert_eq!(cpu.classify(12.0), Status::Healthy);
        assert_eq!(cpu.classify(80.0), Status::Warning);
        assert_eq!(cpu.classify(94.9), Status::Warning);
        assert_eq!(cpu.classify(95.0), Status::Critical);
    }

    #[test]
    fn below_threshold_classifies_in_the_opposite_direction() {
        let ssl_days = Threshold::below(14.0, 3.0);
        assert_eq!(ssl_days.classify(60.0), Status::Healthy);
        assert_eq!(ssl_days.classify(14.0), Status::Warning);
        assert_eq!(ssl_days.classify(3.0), Status::Critical);
        assert_eq!(ssl_days.classify(-1.0), Status::Critical);
    }

    #[test]
    fn non_finite_measurements_are_unknown_not_healthy() {
        let cpu = Threshold::above(80.0, 95.0);
        assert_eq!(cpu.classify(f64::NAN), Status::Unknown);
        assert_eq!(cpu.classify(f64::INFINITY), Status::Unknown);
    }

    #[test]
    fn coherence_check_catches_inverted_thresholds() {
        assert!(Threshold::above(80.0, 95.0).is_coherent());
        assert!(!Threshold::above(95.0, 80.0).is_coherent());
        assert!(Threshold::below(14.0, 3.0).is_coherent());
        assert!(!Threshold::below(3.0, 14.0).is_coherent());
    }
}
