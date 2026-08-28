//! Job priorities.
//!
//! The order here is the answer to "what gets dropped when the machine cannot keep up".
//! It is declared once and honoured by the queue; no subsystem gets to decide it is
//! special.

use serde::{Deserialize, Serialize};

/// What kind of work a job does, in descending order of importance.
///
/// Variants are declared most-important-first so that `Ord` sorts them correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Re-evaluating alert rules. Nothing may delay this: it is how the user finds out.
    CriticalAlert,
    /// Is the server reachable at all.
    ServerAvailability,
    /// Is the website responding.
    WebsiteAvailability,
    /// CPU, memory, disk collection.
    CoreMetrics,
    /// Analytics provider refresh.
    Analytics,
    /// Browser captures. Heavy and never urgent.
    Screenshots,
    /// Rollups, retention, cleanup.
    Maintenance,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::CriticalAlert => "critical_alert",
            Priority::ServerAvailability => "server_availability",
            Priority::WebsiteAvailability => "website_availability",
            Priority::CoreMetrics => "core_metrics",
            Priority::Analytics => "analytics",
            Priority::Screenshots => "screenshots",
            Priority::Maintenance => "maintenance",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Priority::CriticalAlert => "Alerts",
            Priority::ServerAvailability => "Server availability",
            Priority::WebsiteAvailability => "Website availability",
            Priority::CoreMetrics => "Metrics",
            Priority::Analytics => "Analytics",
            Priority::Screenshots => "Screenshots",
            Priority::Maintenance => "Maintenance",
        }
    }

    pub const ALL: &'static [Priority] = &[
        Priority::CriticalAlert,
        Priority::ServerAvailability,
        Priority::WebsiteAvailability,
        Priority::CoreMetrics,
        Priority::Analytics,
        Priority::Screenshots,
        Priority::Maintenance,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priorities_are_ordered_most_important_first() {
        // The queue sorts ascending, so "less" must mean "more important".
        assert!(Priority::CriticalAlert < Priority::ServerAvailability);
        assert!(Priority::ServerAvailability < Priority::WebsiteAvailability);
        assert!(Priority::WebsiteAvailability < Priority::CoreMetrics);
        assert!(Priority::CoreMetrics < Priority::Analytics);
        assert!(Priority::Analytics < Priority::Screenshots);
        assert!(Priority::Screenshots < Priority::Maintenance);
    }

    #[test]
    fn the_all_list_is_in_priority_order_and_complete() {
        assert!(Priority::ALL.windows(2).all(|w| w[0] < w[1]));
        let mut names: Vec<&str> = Priority::ALL.iter().map(|p| p.as_str()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }
}
