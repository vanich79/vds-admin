//! The dashboard widget system.
//!
//! The dashboard is not hard-coded: it is a list of widget descriptors that the user can
//! reorder, resize and switch off. Adding a widget kind means adding a variant and a
//! renderer — the layout engine, persistence and settings screen do not change.

use serde::{Deserialize, Serialize};

/// A kind of dashboard widget.
///
/// A string form is persisted rather than an index, so adding or removing a kind never
/// scrambles a saved layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetKind {
    ServerStatus,
    ServerMetrics,
    CpuHistory,
    RamHistory,
    NetworkHistory,
    WebsiteStatus,
    WebsiteScreenshots,
    TrafficSummary,
    VisitorsGraph,
    DockerStatistics,
    Alerts,
    Events,
}

impl WidgetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WidgetKind::ServerStatus => "server_status",
            WidgetKind::ServerMetrics => "server_metrics",
            WidgetKind::CpuHistory => "cpu_history",
            WidgetKind::RamHistory => "ram_history",
            WidgetKind::NetworkHistory => "network_history",
            WidgetKind::WebsiteStatus => "website_status",
            WidgetKind::WebsiteScreenshots => "website_screenshots",
            WidgetKind::TrafficSummary => "traffic_summary",
            WidgetKind::VisitorsGraph => "visitors_graph",
            WidgetKind::DockerStatistics => "docker_statistics",
            WidgetKind::Alerts => "alerts",
            WidgetKind::Events => "events",
        }
    }

    pub fn parse(raw: &str) -> Option<WidgetKind> {
        let kind = match raw {
            "server_status" => WidgetKind::ServerStatus,
            "server_metrics" => WidgetKind::ServerMetrics,
            "cpu_history" => WidgetKind::CpuHistory,
            "ram_history" => WidgetKind::RamHistory,
            "network_history" => WidgetKind::NetworkHistory,
            "website_status" => WidgetKind::WebsiteStatus,
            "website_screenshots" => WidgetKind::WebsiteScreenshots,
            "traffic_summary" => WidgetKind::TrafficSummary,
            "visitors_graph" => WidgetKind::VisitorsGraph,
            "docker_statistics" => WidgetKind::DockerStatistics,
            "alerts" => WidgetKind::Alerts,
            "events" => WidgetKind::Events,
            _ => return None,
        };
        Some(kind)
    }

    /// Label shown in the dashboard settings list.
    pub fn label(self) -> &'static str {
        match self {
            WidgetKind::ServerStatus => "Server status",
            WidgetKind::ServerMetrics => "Server metrics",
            WidgetKind::CpuHistory => "CPU graph",
            WidgetKind::RamHistory => "RAM graph",
            WidgetKind::NetworkHistory => "Network graph",
            WidgetKind::WebsiteStatus => "Website status",
            WidgetKind::WebsiteScreenshots => "Website screenshots",
            WidgetKind::TrafficSummary => "Traffic summary",
            WidgetKind::VisitorsGraph => "Visitors graph",
            WidgetKind::DockerStatistics => "Docker statistics",
            WidgetKind::Alerts => "Alerts",
            WidgetKind::Events => "Recent events",
        }
    }

    /// Whether this widget needs an analytics provider to be configured.
    ///
    /// The settings screen greys these out until one is, rather than offering a widget
    /// that would render empty.
    pub fn requires_analytics(self) -> bool {
        matches!(self, WidgetKind::TrafficSummary | WidgetKind::VisitorsGraph)
    }

    /// Whether this widget needs screenshots to be available.
    pub fn requires_screenshots(self) -> bool {
        matches!(self, WidgetKind::WebsiteScreenshots)
    }

    pub const ALL: &'static [WidgetKind] = &[
        WidgetKind::ServerStatus,
        WidgetKind::ServerMetrics,
        WidgetKind::CpuHistory,
        WidgetKind::RamHistory,
        WidgetKind::NetworkHistory,
        WidgetKind::WebsiteStatus,
        WidgetKind::WebsiteScreenshots,
        WidgetKind::TrafficSummary,
        WidgetKind::VisitorsGraph,
        WidgetKind::DockerStatistics,
        WidgetKind::Alerts,
        WidgetKind::Events,
    ];
}

/// How much room a widget takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetSize {
    /// One column.
    Small,
    /// Two columns.
    Medium,
    /// Full width.
    Large,
}

impl WidgetSize {
    /// Columns occupied in a twelve-column grid.
    pub fn columns(self) -> u8 {
        match self {
            WidgetSize::Small => 3,
            WidgetSize::Medium => 6,
            WidgetSize::Large => 12,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            WidgetSize::Small => "small",
            WidgetSize::Medium => "medium",
            WidgetSize::Large => "large",
        }
    }

    pub fn parse(raw: &str) -> Option<WidgetSize> {
        match raw {
            "small" => Some(WidgetSize::Small),
            "medium" => Some(WidgetSize::Medium),
            "large" => Some(WidgetSize::Large),
            _ => None,
        }
    }
}

/// One widget's placement and configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WidgetConfig {
    pub kind: WidgetKind,
    pub visible: bool,
    /// Position in the layout; lower comes first.
    pub order: u32,
    pub size: WidgetSize,
    /// Widget-specific settings, e.g. which time range a graph shows.
    #[serde(default)]
    pub settings: serde_json::Value,
}

impl WidgetConfig {
    pub fn new(kind: WidgetKind, order: u32, size: WidgetSize) -> Self {
        Self {
            kind,
            visible: true,
            order,
            size,
            settings: serde_json::Value::Object(Default::default()),
        }
    }
}

/// A whole dashboard layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashboardLayout {
    pub widgets: Vec<WidgetConfig>,
}

impl DashboardLayout {
    /// The layout a fresh installation starts with.
    pub fn default_desktop() -> Self {
        Self {
            widgets: vec![
                WidgetConfig::new(WidgetKind::ServerStatus, 0, WidgetSize::Large),
                WidgetConfig::new(WidgetKind::VisitorsGraph, 1, WidgetSize::Medium),
                WidgetConfig::new(WidgetKind::CpuHistory, 2, WidgetSize::Medium),
                WidgetConfig::new(WidgetKind::WebsiteStatus, 3, WidgetSize::Medium),
                WidgetConfig::new(WidgetKind::Alerts, 4, WidgetSize::Medium),
                WidgetConfig::new(WidgetKind::WebsiteScreenshots, 5, WidgetSize::Large),
                WidgetConfig::new(WidgetKind::Events, 6, WidgetSize::Large),
            ],
        }
    }

    /// A layout tuned for a narrow screen: everything full width, heaviest content last.
    pub fn default_mobile() -> Self {
        Self {
            widgets: vec![
                WidgetConfig::new(WidgetKind::ServerStatus, 0, WidgetSize::Large),
                WidgetConfig::new(WidgetKind::WebsiteStatus, 1, WidgetSize::Large),
                WidgetConfig::new(WidgetKind::TrafficSummary, 2, WidgetSize::Large),
                WidgetConfig::new(WidgetKind::VisitorsGraph, 3, WidgetSize::Large),
                WidgetConfig::new(WidgetKind::Alerts, 4, WidgetSize::Large),
                // Screenshots are the most expensive thing to load on a phone, so they
                // come last and scroll into view rather than blocking the first paint.
                WidgetConfig::new(WidgetKind::WebsiteScreenshots, 5, WidgetSize::Large),
            ],
        }
    }

    /// Visible widgets in display order.
    pub fn visible(&self) -> Vec<&WidgetConfig> {
        let mut widgets: Vec<&WidgetConfig> = self.widgets.iter().filter(|w| w.visible).collect();
        widgets.sort_by_key(|w| w.order);
        widgets
    }

    /// Turns a widget on or off, adding it to the layout if it was never present.
    pub fn set_visible(&mut self, kind: WidgetKind, visible: bool) {
        match self.widgets.iter_mut().find(|w| w.kind == kind) {
            Some(widget) => widget.visible = visible,
            None => {
                let order = self
                    .widgets
                    .iter()
                    .map(|w| w.order)
                    .max()
                    .map_or(0, |o| o + 1);
                let mut widget = WidgetConfig::new(kind, order, WidgetSize::Medium);
                widget.visible = visible;
                self.widgets.push(widget);
            }
        }
    }

    pub fn is_visible(&self, kind: WidgetKind) -> bool {
        self.widgets.iter().any(|w| w.kind == kind && w.visible)
    }

    /// Moves a widget to a new position, renumbering the rest.
    pub fn reorder(&mut self, kind: WidgetKind, new_order: u32) {
        let Some(index) = self.widgets.iter().position(|w| w.kind == kind) else {
            return;
        };
        let mut ordered: Vec<WidgetConfig> = self.widgets.clone();
        ordered.sort_by_key(|w| w.order);

        let Some(current) = ordered.iter().position(|w| w.kind == kind) else {
            return;
        };
        let widget = ordered.remove(current);
        let target = (new_order as usize).min(ordered.len());
        ordered.insert(target, widget);

        for (position, widget) in ordered.iter_mut().enumerate() {
            widget.order = position as u32;
        }
        let _ = index;
        self.widgets = ordered;
    }

    /// Removes widgets whose prerequisites are missing.
    ///
    /// Called with what the installation actually has, so a user without an analytics
    /// provider never sees an empty traffic panel.
    pub fn filtered(&self, has_analytics: bool, has_screenshots: bool) -> DashboardLayout {
        DashboardLayout {
            widgets: self
                .widgets
                .iter()
                .filter(|w| !(w.kind.requires_analytics() && !has_analytics))
                .filter(|w| !(w.kind.requires_screenshots() && !has_screenshots))
                .cloned()
                .collect(),
        }
    }
}

impl Default for DashboardLayout {
    fn default() -> Self {
        Self::default_desktop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_widget_kind_round_trips() {
        for kind in WidgetKind::ALL {
            assert_eq!(WidgetKind::parse(kind.as_str()), Some(*kind));
        }
        assert_eq!(WidgetKind::parse("teapot"), None);
    }

    #[test]
    fn widget_identifiers_are_unique() {
        let mut names: Vec<&str> = WidgetKind::ALL.iter().map(|k| k.as_str()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn the_default_desktop_layout_is_ordered_and_visible() {
        let layout = DashboardLayout::default_desktop();
        let visible = layout.visible();
        assert_eq!(visible.len(), layout.widgets.len());
        assert!(visible.windows(2).all(|w| w[0].order <= w[1].order));
    }

    #[test]
    fn the_mobile_layout_puts_screenshots_last() {
        // The most expensive thing to load on a phone must not block the first paint.
        let layout = DashboardLayout::default_mobile();
        let visible = layout.visible();
        assert_eq!(
            visible.last().map(|w| w.kind),
            Some(WidgetKind::WebsiteScreenshots)
        );
    }

    #[test]
    fn the_mobile_layout_is_single_column() {
        let layout = DashboardLayout::default_mobile();
        assert!(layout.widgets.iter().all(|w| w.size == WidgetSize::Large));
    }

    #[test]
    fn a_hidden_widget_disappears_from_the_layout_but_keeps_its_place() {
        let mut layout = DashboardLayout::default_desktop();
        assert!(layout.is_visible(WidgetKind::Alerts));

        layout.set_visible(WidgetKind::Alerts, false);
        assert!(!layout.is_visible(WidgetKind::Alerts));
        assert!(
            layout
                .visible()
                .iter()
                .all(|w| w.kind != WidgetKind::Alerts)
        );

        // Turning it back on restores it where it was, rather than appending it.
        layout.set_visible(WidgetKind::Alerts, true);
        assert!(layout.is_visible(WidgetKind::Alerts));
        assert_eq!(
            layout
                .widgets
                .iter()
                .filter(|w| w.kind == WidgetKind::Alerts)
                .count(),
            1
        );
    }

    #[test]
    fn enabling_a_widget_that_was_never_in_the_layout_appends_it() {
        let mut layout = DashboardLayout::default_desktop();
        assert!(!layout.is_visible(WidgetKind::DockerStatistics));

        layout.set_visible(WidgetKind::DockerStatistics, true);
        assert!(layout.is_visible(WidgetKind::DockerStatistics));
        assert_eq!(
            layout.visible().last().map(|w| w.kind),
            Some(WidgetKind::DockerStatistics)
        );
    }

    #[test]
    fn reordering_moves_a_widget_and_renumbers_the_rest() {
        let mut layout = DashboardLayout::default_desktop();
        layout.reorder(WidgetKind::Events, 0);

        let visible = layout.visible();
        assert_eq!(visible[0].kind, WidgetKind::Events);
        // Orders stay dense and unique, so a later insert cannot collide.
        let orders: Vec<u32> = visible.iter().map(|w| w.order).collect();
        assert_eq!(orders, (0..orders.len() as u32).collect::<Vec<_>>());
    }

    #[test]
    fn reordering_past_the_end_clamps_to_last() {
        let mut layout = DashboardLayout::default_desktop();
        layout.reorder(WidgetKind::ServerStatus, 999);
        assert_eq!(
            layout.visible().last().map(|w| w.kind),
            Some(WidgetKind::ServerStatus)
        );
    }

    #[test]
    fn reordering_an_absent_widget_is_a_no_op() {
        let mut layout = DashboardLayout::default_desktop();
        let before = layout.clone();
        layout.reorder(WidgetKind::DockerStatistics, 0);
        assert_eq!(layout, before);
    }

    #[test]
    fn widgets_without_their_prerequisites_are_filtered_out() {
        // A user with no analytics provider must not be shown an empty traffic panel.
        let layout = DashboardLayout::default_desktop();
        let filtered = layout.filtered(false, true);
        assert!(
            filtered
                .widgets
                .iter()
                .all(|w| !w.kind.requires_analytics())
        );
        assert!(
            filtered
                .widgets
                .iter()
                .any(|w| w.kind == WidgetKind::WebsiteScreenshots)
        );

        let filtered = layout.filtered(true, false);
        assert!(
            filtered
                .widgets
                .iter()
                .all(|w| !w.kind.requires_screenshots())
        );
        assert!(
            filtered
                .widgets
                .iter()
                .any(|w| w.kind == WidgetKind::VisitorsGraph)
        );
    }

    #[test]
    fn a_fully_equipped_installation_keeps_every_widget() {
        let layout = DashboardLayout::default_desktop();
        assert_eq!(
            layout.filtered(true, true).widgets.len(),
            layout.widgets.len()
        );
    }

    #[test]
    fn layouts_round_trip_through_json() {
        let layout = DashboardLayout::default_desktop();
        let json = serde_json::to_string(&layout).expect("serialises");
        let parsed: DashboardLayout = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(parsed, layout);
    }

    #[test]
    fn widget_sizes_map_onto_a_twelve_column_grid() {
        assert_eq!(WidgetSize::Small.columns(), 3);
        assert_eq!(WidgetSize::Medium.columns(), 6);
        assert_eq!(WidgetSize::Large.columns(), 12);
        for size in [WidgetSize::Small, WidgetSize::Medium, WidgetSize::Large] {
            assert_eq!(WidgetSize::parse(size.as_str()), Some(size));
        }
    }
}
