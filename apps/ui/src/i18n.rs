//! The interface's strings, in every language it speaks.
//!
//! Generated from one table together with `ui/strings.slint`, so the two cannot
//! disagree: a key added on one side without the other stops compiling.
//!
//! ## Why a catalogue rather than `@tr()`
//!
//! Slint can use gettext, which would mean `.po` files and a system gettext library.
//! That is a build dependency this project has gone out of its way not to need —
//! see `docs/adr/001-technology-stack.md`. A plain global costs a generated file and
//! gives the compiler the chance to catch a missing string, which `@tr()` does not.

use crate::{AppWindow, L};
use slint::ComponentHandle;

/// The languages the interface speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Russian,
}

impl Language {
    /// The code stored in the configuration file.
    pub fn as_str(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Russian => "ru",
        }
    }

    /// How the language is named *in that language*, for the picker.
    ///
    /// A Russian speaker looking for their language should not have to recognise the
    /// word "Russian" first.
    pub fn endonym(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Russian => "Русский",
        }
    }

    /// Every language, in the order the picker shows them.
    pub const ALL: &'static [Language] = &[Language::English, Language::Russian];

    /// Resolves the configured value, which may be a code or `"system"`.
    ///
    /// Anything unrecognised falls back to the system's choice rather than failing:
    /// a typo in a configuration file must not leave the application without words.
    pub fn resolve(configured: &str) -> Language {
        match configured.trim().to_ascii_lowercase().as_str() {
            "en" | "english" => Language::English,
            "ru" | "russian" => Language::Russian,
            _ => Language::from_system(),
        }
    }

    /// What the operating system says the user prefers.
    ///
    /// Read from the usual environment variables, which is enough on Linux and macOS.
    /// Windows does not set them, so a Windows user gets English until they choose;
    /// the picker is one click away and the choice is remembered.
    pub fn from_system() -> Language {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(value) = std::env::var(key) {
                let value = value.to_ascii_lowercase();
                if value.starts_with("ru") {
                    return Language::Russian;
                }
                if !value.is_empty() && value != "c" && value != "posix" {
                    return Language::English;
                }
            }
        }
        Language::English
    }

    /// The index of this language in [`Language::ALL`], for the picker.
    pub fn index(self) -> i32 {
        Language::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .and_then(|index| i32::try_from(index).ok())
            .unwrap_or(0)
    }

    /// Resolves a picker index back to a language.
    pub fn at(index: i32) -> Language {
        usize::try_from(index)
            .ok()
            .and_then(|index| Language::ALL.get(index))
            .copied()
            .unwrap_or(Language::English)
    }

    pub fn strings(self) -> Strings {
        match self {
            Language::English => Strings::english(),
            Language::Russian => Strings::russian(),
        }
    }
}

/// Every string the interface shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strings {
    pub app_title: &'static str,
    pub app_subtitle: &'static str,
    pub nav_dashboard: &'static str,
    pub nav_home: &'static str,
    pub nav_servers: &'static str,
    pub nav_websites: &'static str,
    pub nav_sites_short: &'static str,
    pub nav_analytics: &'static str,
    pub nav_alerts: &'static str,
    pub nav_settings: &'static str,
    pub nav_more: &'static str,
    pub action_add: &'static str,
    pub action_cancel: &'static str,
    pub action_save: &'static str,
    pub action_remove: &'static str,
    pub action_refresh: &'static str,
    pub action_retry: &'static str,
    pub action_open: &'static str,
    pub action_back: &'static str,
    pub action_working: &'static str,
    pub label_name: &'static str,
    pub label_status: &'static str,
    pub label_host: &'static str,
    pub label_port: &'static str,
    pub label_address: &'static str,
    pub label_uptime: &'static str,
    pub label_last_check: &'static str,
    pub label_cpu: &'static str,
    pub label_ram: &'static str,
    pub label_memory: &'static str,
    pub label_disk: &'static str,
    pub label_subject: &'static str,
    pub label_size: &'static str,
    pub label_expires: &'static str,
    pub label_issuer: &'static str,
    pub not_measured: &'static str,
    pub no_measurement: &'static str,
    pub nothing_here_yet: &'static str,
    pub could_not_load: &'static str,
    pub dash_recent_alerts: &'static str,
    pub dash_recent_events: &'static str,
    pub dash_problem_servers: &'static str,
    pub dash_needs_attention: &'static str,
    pub dash_preview: &'static str,
    pub dash_all_healthy: &'static str,
    pub dash_nothing_monitored: &'static str,
    pub dash_nothing_monitored_detail: &'static str,
    pub dash_nothing_happened: &'static str,
    pub dash_no_alerts: &'static str,
    pub dash_no_analytics: &'static str,
    pub dash_no_analytics_detail: &'static str,
    pub dash_connect_provider: &'static str,
    pub tile_servers: &'static str,
    pub tile_online: &'static str,
    pub tile_offline: &'static str,
    pub tile_websites: &'static str,
    pub tile_average_cpu: &'static str,
    pub tile_average_ram: &'static str,
    pub tile_visitors: &'static str,
    pub tile_visits: &'static str,
    pub tile_page_views: &'static str,
    pub tile_bounce_rate: &'static str,
    pub servers_filter: &'static str,
    pub servers_empty: &'static str,
    pub servers_empty_detail: &'static str,
    pub servers_add: &'static str,
    pub servers_add_a: &'static str,
    pub tab_overview: &'static str,
    pub tab_metrics: &'static str,
    pub tab_processes: &'static str,
    pub tab_services: &'static str,
    pub tab_docker: &'static str,
    pub tab_websites: &'static str,
    pub tab_events: &'static str,
    pub tab_settings: &'static str,
    pub tab_analytics: &'static str,
    pub tab_availability: &'static str,
    pub tab_ssl: &'static str,
    pub tab_screenshot: &'static str,
    pub tab_history: &'static str,
    pub tab_rules: &'static str,
    pub sd_system: &'static str,
    pub sd_operating_system: &'static str,
    pub sd_kernel: &'static str,
    pub sd_architecture: &'static str,
    pub sd_cores: &'static str,
    pub sd_connection: &'static str,
    pub sd_top_processes: &'static str,
    pub sd_sorted_by_cpu: &'static str,
    pub sd_no_processes: &'static str,
    pub sd_containers: &'static str,
    pub sd_no_docker: &'static str,
    pub sd_no_docker_detail: &'static str,
    pub sd_docker_empty: &'static str,
    pub sd_no_systemd: &'static str,
    pub sd_no_systemd_detail: &'static str,
    pub sd_no_websites: &'static str,
    pub sd_no_websites_detail: &'static str,
    pub sd_no_events: &'static str,
    pub sd_host_key: &'static str,
    pub sd_host_key_detail: &'static str,
    pub sd_host_key_warning: &'static str,
    pub sd_forget_host_key: &'static str,
    pub sd_forget_confirm: &'static str,
    pub sd_forget_it: &'static str,
    pub sd_remove_server: &'static str,
    pub sd_remove_detail: &'static str,
    pub sd_remove_confirm_prefix: &'static str,
    pub sd_remove_confirm_suffix: &'static str,
    pub websites_empty: &'static str,
    pub websites_empty_detail: &'static str,
    pub websites_add: &'static str,
    pub websites_add_a: &'static str,
    pub websites_grid: &'static str,
    pub websites_list: &'static str,
    pub wd_http_status: &'static str,
    pub wd_response_time: &'static str,
    pub wd_uptime_24h: &'static str,
    pub wd_tls_certificate: &'static str,
    pub wd_top_pages: &'static str,
    pub wd_no_events: &'static str,
    pub wd_no_analytics: &'static str,
    pub wd_refresh_screenshot: &'static str,
    pub wd_offline_no_shot: &'static str,
    pub analytics_by_website: &'static str,
    pub analytics_vs_previous: &'static str,
    pub alerts_nothing_firing: &'static str,
    pub alerts_acknowledge: &'static str,
    pub alerts_acknowledged: &'static str,
    pub alerts_add_rule: &'static str,
    pub alerts_open: &'static str,
    pub alerts_resolved: &'static str,
    pub set_appearance: &'static str,
    pub set_theme: &'static str,
    pub set_language: &'static str,
    pub set_notifications: &'static str,
    pub set_desktop_notifications: &'static str,
    pub set_play_sound: &'static str,
    pub set_webhook_url: &'static str,
    pub set_screenshots: &'static str,
    pub set_no_browser: &'static str,
    pub set_counter_id: &'static str,
    pub set_oauth_token: &'static str,
    pub set_token_hint: &'static str,
    pub set_credential_storage: &'static str,
    pub set_backend: &'static str,
    pub set_encrypted_file_warning: &'static str,
    pub set_storage_diagnostics: &'static str,
    pub set_database: &'static str,
    pub set_logs: &'static str,
    pub set_debug_mode: &'static str,
    pub dlg_add_server: &'static str,
    pub dlg_add_website: &'static str,
    pub dlg_ssh_subtitle: &'static str,
    pub dlg_agent_subtitle: &'static str,
    pub dlg_website_subtitle: &'static str,
    pub dlg_connect_via: &'static str,
    pub dlg_mode_ssh: &'static str,
    pub dlg_mode_agent: &'static str,
    pub dlg_username: &'static str,
    pub dlg_authentication: &'static str,
    pub dlg_auth_password: &'static str,
    pub dlg_auth_key: &'static str,
    pub dlg_auth_encrypted_key: &'static str,
    pub dlg_passphrase: &'static str,
    pub dlg_token: &'static str,
    pub dlg_poll_every: &'static str,
    pub dlg_check_every: &'static str,
    pub dlg_expected_status: &'static str,
    pub dlg_expected_text: &'static str,
    pub dlg_url: &'static str,
    pub dlg_ph_server_name: &'static str,
    pub dlg_ph_host: &'static str,
    pub dlg_ph_username: &'static str,
    pub dlg_ph_password: &'static str,
    pub dlg_ph_key: &'static str,
    pub dlg_ph_passphrase: &'static str,
    pub dlg_ph_token: &'static str,
    pub dlg_ph_seconds: &'static str,
    pub dlg_ph_website_name: &'static str,
    pub dlg_ph_url: &'static str,
    pub dlg_ph_expected_text: &'static str,
    pub dlg_ph_counter: &'static str,
    pub dlg_ph_webhook: &'static str,
    pub dlg_scheme_hint: &'static str,
    pub dlg_expected_text_hint: &'static str,
    pub dlg_fingerprint_hint: &'static str,
    pub status_online: &'static str,
    pub status_warning: &'static str,
    pub status_critical: &'static str,
    pub status_offline: &'static str,
    pub status_unknown: &'static str,
    pub theme_light: &'static str,
    pub theme_dark: &'static str,
    pub theme_system: &'static str,
    pub range_1h: &'static str,
    pub range_6h: &'static str,
    pub range_24h: &'static str,
    pub range_7d: &'static str,
    pub range_30d: &'static str,
    pub range_90d: &'static str,
    pub range_1y: &'static str,
    pub period_today: &'static str,
    pub period_yesterday: &'static str,
    pub period_7d: &'static str,
    pub period_30d: &'static str,
    pub period_90d: &'static str,
    pub am_visitors: &'static str,
    pub am_visits: &'static str,
    pub am_page_views: &'static str,
    pub am_sessions: &'static str,
    pub am_unique_visitors: &'static str,
    pub am_new_visitors: &'static str,
    pub am_returning_visitors: &'static str,
    pub am_bounce_rate: &'static str,
    pub am_session_duration: &'static str,
    pub am_pages_per_session: &'static str,
    pub policy_hourly: &'static str,
    pub policy_six_hours: &'static str,
    pub policy_daily: &'static str,
    pub policy_manual: &'static str,
    pub mk_cpu: &'static str,
    pub mk_ram: &'static str,
    pub mk_ram_used: &'static str,
    pub mk_swap: &'static str,
    pub mk_disk: &'static str,
    pub mk_disk_used: &'static str,
    pub mk_network_in: &'static str,
    pub mk_network_out: &'static str,
    pub mk_load_1m: &'static str,
    pub mk_load_5m: &'static str,
    pub mk_load_15m: &'static str,
    pub mk_uptime: &'static str,
    pub mk_processes: &'static str,
    pub mk_temperature: &'static str,
    pub mk_response_time: &'static str,
    pub mk_ssl_expiry: &'static str,
    pub time_never: &'static str,
    pub time_just_now: &'static str,
    pub time_secs_ago: &'static str,
    pub time_mins_ago: &'static str,
    pub time_hours_ago: &'static str,
    pub time_days_ago: &'static str,
    pub dur_days_hours: &'static str,
    pub dur_hours_mins: &'static str,
    pub dur_mins: &'static str,
    pub dur_secs: &'static str,
    pub ssl_expired_days_ago: &'static str,
    pub ssl_expires_today: &'static str,
    pub ssl_one_day: &'static str,
    pub ssl_days: &'static str,
    pub card_response: &'static str,
    pub card_ssl: &'static str,
    pub card_uptime_24h: &'static str,
    pub card_visitors_today: &'static str,
    pub card_analytics_updated: &'static str,
    pub shot_captured: &'static str,
    pub shot_capturing: &'static str,
    pub shot_none_yet: &'static str,
    pub shot_offline: &'static str,
    pub shot_failed: &'static str,
    pub shot_unsupported: &'static str,
    pub ev_server_status: &'static str,
    pub ev_collection_failed: &'static str,
    pub ev_website_status: &'static str,
    pub ev_threshold: &'static str,
    pub ev_certificate: &'static str,
    pub ev_traffic_anomaly: &'static str,
    pub ev_analytics_refreshed: &'static str,
    pub ev_analytics_failed: &'static str,
    pub ev_screenshot_updated: &'static str,
    pub ev_screenshot_failed: &'static str,
    pub ev_incident_resolved: &'static str,
    pub ev_container_state: &'static str,
    pub ev_service_state: &'static str,
    pub ev_website_checked: &'static str,
    pub ev_metrics_collected: &'static str,
    pub incident_open_for: &'static str,
    pub err_server_name_empty: &'static str,
    pub err_server_host_empty: &'static str,
    pub err_port_invalid: &'static str,
    pub err_interval_invalid: &'static str,
    pub err_failures_invalid: &'static str,
    pub err_timeout_invalid: &'static str,
    pub err_timeout_too_long: &'static str,
    pub err_thresholds_inverted: &'static str,
    pub err_website_name_empty: &'static str,
    pub err_url_malformed: &'static str,
    pub err_url_scheme: &'static str,
    pub err_url_no_host: &'static str,
    pub err_status_invalid: &'static str,
    pub err_credential_missing: &'static str,
    pub err_credential_store: &'static str,
    pub err_save_failed: &'static str,
}

impl Strings {
    pub fn english() -> Self {
        Self {
            app_title: "VDS Admin",
            app_subtitle: "Infrastructure & analytics",
            nav_dashboard: "Dashboard",
            nav_home: "Home",
            nav_servers: "Servers",
            nav_websites: "Websites",
            nav_sites_short: "Sites",
            nav_analytics: "Analytics",
            nav_alerts: "Alerts",
            nav_settings: "Settings",
            nav_more: "More",
            action_add: "Add",
            action_cancel: "Cancel",
            action_save: "Save",
            action_remove: "Remove",
            action_refresh: "Refresh",
            action_retry: "Retry",
            action_open: "Open",
            action_back: "← Back",
            action_working: "Working…",
            label_name: "Name",
            label_status: "Status",
            label_host: "Host",
            label_port: "Port",
            label_address: "Address",
            label_uptime: "Uptime",
            label_last_check: "Last check",
            label_cpu: "CPU",
            label_ram: "RAM",
            label_memory: "Memory",
            label_disk: "Disk",
            label_subject: "Subject",
            label_size: "Size",
            label_expires: "Expires",
            label_issuer: "Issuer",
            not_measured: "not measured",
            no_measurement: "no measurement",
            nothing_here_yet: "nothing here yet",
            could_not_load: "we could not load this",
            dash_recent_alerts: "Recent alerts",
            dash_recent_events: "Recent events",
            dash_problem_servers: "Servers that are not healthy",
            dash_needs_attention: "Needs attention",
            dash_preview: "Preview",
            dash_all_healthy: "Every monitored server and website is within its configured thresholds.",
            dash_nothing_monitored: "Nothing is being monitored yet",
            dash_nothing_monitored_detail: "Add a server to start collecting metrics, or add a website to watch its availability and certificate.",
            dash_nothing_happened: "Nothing has happened yet.",
            dash_no_alerts: "No alerts have fired.",
            dash_no_analytics: "No analytics provider is connected",
            dash_no_analytics_detail: "Connect Yandex.Metrica and enter a counter ID to see visitors, visits and page views alongside your infrastructure.",
            dash_connect_provider: "Connect a provider",
            tile_servers: "Servers",
            tile_online: "Online",
            tile_offline: "Offline",
            tile_websites: "Websites",
            tile_average_cpu: "Average CPU",
            tile_average_ram: "Average RAM",
            tile_visitors: "Visitors",
            tile_visits: "Visits",
            tile_page_views: "Page views",
            tile_bounce_rate: "Bounce rate",
            servers_filter: "Filter by name, host or tag",
            servers_empty: "No servers yet",
            servers_empty_detail: "Add a Linux server over SSH, or install the agent on it, to start collecting CPU, memory, disk and network metrics.",
            servers_add: "Add server",
            servers_add_a: "Add a server",
            tab_overview: "Overview",
            tab_metrics: "Metrics",
            tab_processes: "Processes",
            tab_services: "Services",
            tab_docker: "Docker",
            tab_websites: "Websites",
            tab_events: "Events",
            tab_settings: "Settings",
            tab_analytics: "Analytics",
            tab_availability: "Availability",
            tab_ssl: "SSL",
            tab_screenshot: "Screenshots",
            tab_history: "History",
            tab_rules: "Rules",
            sd_system: "System",
            sd_operating_system: "Operating system",
            sd_kernel: "Kernel",
            sd_architecture: "Architecture",
            sd_cores: "Cores",
            sd_connection: "Connection",
            sd_top_processes: "Top processes",
            sd_sorted_by_cpu: "Sorted by CPU",
            sd_no_processes: "No process information was collected.",
            sd_containers: "Containers",
            sd_no_docker: "Docker is not installed on this host",
            sd_no_docker_detail: "Container monitoring appears automatically if Docker is installed later. This does not affect the server's health.",
            sd_docker_empty: "Docker is installed, but there are no containers.",
            sd_no_systemd: "This host does not use systemd",
            sd_no_systemd_detail: "Service monitoring is only available on systemd-based systems. Everything else about this server is still monitored.",
            sd_no_websites: "No websites are linked to this server",
            sd_no_websites_detail: "Linking a website to the server it runs on lets the app suggest a possible connection between an infrastructure event and a change in traffic.",
            sd_no_events: "Nothing has happened on this server yet.",
            sd_host_key: "Host key",
            sd_host_key_detail: "This server's SSH host key was recorded on the first connection and is checked every time since. A key that changes stops the connection.",
            sd_host_key_warning: "Forget it only after a host was rebuilt or its keys were regenerated — and check the new fingerprint against the server before reconnecting.",
            sd_forget_host_key: "Forget host key",
            sd_forget_confirm: "Forget the recorded key?",
            sd_forget_it: "Forget it",
            sd_remove_server: "Remove server",
            sd_remove_detail: "Removes this server, its stored credential and its metric history. The server itself is not touched — nothing is installed on it to remove.",
            sd_remove_confirm_prefix: "Remove ",
            sd_remove_confirm_suffix: " and its history?",
            websites_empty: "No websites yet",
            websites_empty_detail: "Add a URL to monitor its availability, response time and TLS certificate — and, if you connect an analytics provider, its traffic.",
            websites_add: "Add website",
            websites_add_a: "Add a website",
            websites_grid: "Grid",
            websites_list: "List",
            wd_http_status: "HTTP status",
            wd_response_time: "Response time",
            wd_uptime_24h: "Uptime (24h)",
            wd_tls_certificate: "TLS certificate",
            wd_top_pages: "Top pages",
            wd_no_events: "Nothing has happened for this website yet.",
            wd_no_analytics: "Connect Yandex.Metrica in Settings to see visitors, visits and page views for this website.",
            wd_refresh_screenshot: "Refresh screenshot",
            wd_offline_no_shot: "Website is currently offline",
            analytics_by_website: "By website",
            analytics_vs_previous: "vs previous period",
            alerts_nothing_firing: "Nothing is firing",
            alerts_acknowledge: "Acknowledge",
            alerts_acknowledged: "Acknowledged",
            alerts_add_rule: "Add rule",
            alerts_open: "open",
            alerts_resolved: "resolved",
            set_appearance: "Appearance",
            set_theme: "Theme",
            set_language: "Language",
            set_notifications: "Notifications",
            set_desktop_notifications: "Desktop notifications",
            set_play_sound: "Play a sound",
            set_webhook_url: "Webhook URL",
            set_screenshots: "Screenshots",
            set_no_browser: "No Chromium-family browser was found, so website previews are unavailable. Install Chrome, Chromium, Edge or Brave, or set a browser path in the configuration file.",
            set_counter_id: "Counter ID",
            set_oauth_token: "OAuth token",
            set_token_hint: "Stored in the system keychain, never in the database",
            set_credential_storage: "Credential storage",
            set_backend: "Backend",
            set_encrypted_file_warning: "Credentials are stored in an encrypted file because no system keystore was available on this machine. They are encrypted, but a system keystore is stronger.",
            set_storage_diagnostics: "Storage and diagnostics",
            set_database: "Database",
            set_logs: "Logs",
            set_debug_mode: "Debug mode (verbose logging and the scheduler panel)",
            dlg_add_server: "Add server",
            dlg_add_website: "Add website",
            dlg_ssh_subtitle: "Credentials are stored in the system keychain, never in the database.",
            dlg_agent_subtitle: "The agent must already be installed on this host. Its installer printed the token.",
            dlg_website_subtitle: "Checked for DNS, connection, HTTP status, response time and certificate expiry.",
            dlg_connect_via: "Connect via",
            dlg_mode_ssh: "SSH (no agent required)",
            dlg_mode_agent: "Agent (HTTPS)",
            dlg_username: "Username",
            dlg_authentication: "Authentication",
            dlg_auth_password: "Password",
            dlg_auth_key: "Private key",
            dlg_auth_encrypted_key: "Encrypted private key",
            dlg_passphrase: "Passphrase",
            dlg_token: "Token",
            dlg_poll_every: "Poll every",
            dlg_check_every: "Check every",
            dlg_expected_status: "Expected status",
            dlg_expected_text: "Expected text",
            dlg_url: "URL",
            dlg_ph_server_name: "prod-web-01",
            dlg_ph_host: "10.0.0.5 or web01.example.com",
            dlg_ph_username: "vds-monitor",
            dlg_ph_password: "The account's password",
            dlg_ph_key: "Paste the key, including the BEGIN and END lines",
            dlg_ph_passphrase: "The passphrase protecting that key",
            dlg_ph_token: "Printed by the agent's installer",
            dlg_ph_seconds: "seconds",
            dlg_ph_website_name: "Company website",
            dlg_ph_url: "example.com",
            dlg_ph_expected_text: "Optional — a substring the page must contain",
            dlg_ph_counter: "e.g. 12345678",
            dlg_ph_webhook: "https://hooks.example.com/…",
            dlg_scheme_hint: "https:// is assumed when no scheme is given.",
            dlg_expected_text_hint: "Leave the text blank to check only the status code. A blank expectation matches every response, which is a check that looks like it passes and does nothing.",
            dlg_fingerprint_hint: "The agent's certificate fingerprint is shown for confirmation on the first connection. Check it against what the installer printed.",
            status_online: "Online",
            status_warning: "Warning",
            status_critical: "Critical",
            status_offline: "Offline",
            status_unknown: "Unknown",
            theme_light: "Light",
            theme_dark: "Dark",
            theme_system: "System",
            range_1h: "1 hour",
            range_6h: "6 hours",
            range_24h: "24 hours",
            range_7d: "7 days",
            range_30d: "30 days",
            range_90d: "90 days",
            range_1y: "1 year",
            period_today: "Today",
            period_yesterday: "Yesterday",
            period_7d: "7 days",
            period_30d: "30 days",
            period_90d: "90 days",
            am_visitors: "Visitors",
            am_visits: "Visits",
            am_page_views: "Page views",
            am_sessions: "Sessions",
            am_unique_visitors: "Unique visitors",
            am_new_visitors: "New visitors",
            am_returning_visitors: "Returning visitors",
            am_bounce_rate: "Bounce rate",
            am_session_duration: "Avg. session duration",
            am_pages_per_session: "Pages per session",
            policy_hourly: "Every hour",
            policy_six_hours: "Every 6 hours",
            policy_daily: "Every 24 hours",
            policy_manual: "Manual",
            mk_cpu: "CPU",
            mk_ram: "RAM",
            mk_ram_used: "RAM used",
            mk_swap: "Swap",
            mk_disk: "Disk",
            mk_disk_used: "Disk used",
            mk_network_in: "Network in",
            mk_network_out: "Network out",
            mk_load_1m: "Load 1m",
            mk_load_5m: "Load 5m",
            mk_load_15m: "Load 15m",
            mk_uptime: "Uptime",
            mk_processes: "Processes",
            mk_temperature: "Temperature",
            mk_response_time: "Response time",
            mk_ssl_expiry: "SSL expiry",
            time_never: "never",
            time_just_now: "just now",
            time_secs_ago: "{}s ago",
            time_mins_ago: "{}m ago",
            time_hours_ago: "{}h ago",
            time_days_ago: "{}d ago",
            dur_days_hours: "{}d {}h",
            dur_hours_mins: "{}h {}m",
            dur_mins: "{}m",
            dur_secs: "{}s",
            ssl_expired_days_ago: "expired {} days ago",
            ssl_expires_today: "expires today",
            ssl_one_day: "1 day",
            ssl_days: "{} days",
            card_response: "Response: {}",
            card_ssl: "SSL: {}",
            card_uptime_24h: "Uptime 24h: {}",
            card_visitors_today: "Visitors today: {}",
            card_analytics_updated: "Analytics updated {}",
            shot_captured: "Captured {}",
            shot_capturing: "Capturing…",
            shot_none_yet: "No screenshot yet",
            shot_offline: "Screenshot unavailable — the website is currently offline",
            shot_failed: "Screenshot generation failed: {}",
            shot_unsupported: "Screenshots are not available on this machine",
            ev_server_status: "Server went from {} to {}",
            ev_collection_failed: "Collection failed ({} in a row): {}",
            ev_website_status: "Website went from {} to {}",
            ev_threshold: "{} reached {}, above {}",
            ev_certificate: "Certificate {}",
            ev_traffic_anomaly: "Traffic anomaly: {} changed by {}",
            ev_analytics_refreshed: "Analytics refreshed",
            ev_analytics_failed: "Analytics refresh failed: {}",
            ev_screenshot_updated: "Screenshot updated",
            ev_screenshot_failed: "Screenshot failed: {}",
            ev_incident_resolved: "Incident resolved",
            ev_container_state: "Container {} is {}",
            ev_service_state: "Service {} is {}",
            ev_website_checked: "Website checked",
            ev_metrics_collected: "Collected {} metrics",
            incident_open_for: "Open for {}",
            err_server_name_empty: "Enter a name for the server",
            err_server_host_empty: "Enter the server's address",
            err_port_invalid: "The port must be between 1 and 65535",
            err_interval_invalid: "The interval must be at least 1 second",
            err_failures_invalid: "The failure threshold must be at least 1 check",
            err_timeout_invalid: "The timeout must be at least 1 second",
            err_timeout_too_long: "The timeout must not exceed four polling intervals, or checks will pile up",
            err_thresholds_inverted: "The warning and critical thresholds are the wrong way round",
            err_website_name_empty: "Enter a name for the website",
            err_url_malformed: "That address is not a valid URL",
            err_url_scheme: "Only http and https addresses are monitored",
            err_url_no_host: "The address has no host name",
            err_status_invalid: "The expected status must be between 100 and 599",
            err_credential_missing: "Enter the password, key or token for this connection",
            err_credential_store: "The credential could not be saved: {}",
            err_save_failed: "Could not save: {}",
        }
    }

    pub fn russian() -> Self {
        Self {
            app_title: "VDS Admin",
            app_subtitle: "Инфраструктура и аналитика",
            nav_dashboard: "Сводка",
            nav_home: "Сводка",
            nav_servers: "Серверы",
            nav_websites: "Сайты",
            nav_sites_short: "Сайты",
            nav_analytics: "Аналитика",
            nav_alerts: "Оповещения",
            nav_settings: "Настройки",
            nav_more: "Ещё",
            action_add: "Добавить",
            action_cancel: "Отмена",
            action_save: "Сохранить",
            action_remove: "Удалить",
            action_refresh: "Обновить",
            action_retry: "Повторить",
            action_open: "Открыть",
            action_back: "← Назад",
            action_working: "Выполняется…",
            label_name: "Название",
            label_status: "Состояние",
            label_host: "Хост",
            label_port: "Порт",
            label_address: "Адрес",
            label_uptime: "Аптайм",
            label_last_check: "Последняя проверка",
            label_cpu: "ЦП",
            label_ram: "Память",
            label_memory: "Память",
            label_disk: "Диск",
            label_subject: "Объект",
            label_size: "Размер",
            label_expires: "Истекает",
            label_issuer: "Выдан",
            not_measured: "не измерено",
            no_measurement: "нет измерений",
            nothing_here_yet: "здесь пока пусто",
            could_not_load: "не удалось загрузить",
            dash_recent_alerts: "Недавние оповещения",
            dash_recent_events: "Недавние события",
            dash_problem_servers: "Серверы с проблемами",
            dash_needs_attention: "Требуют внимания",
            dash_preview: "Превью",
            dash_all_healthy: "Все серверы и сайты укладываются в заданные пороги.",
            dash_nothing_monitored: "Мониторинг ещё не настроен",
            dash_nothing_monitored_detail: "Добавьте сервер, чтобы начать собирать метрики, или сайт — чтобы следить за доступностью и сертификатом.",
            dash_nothing_happened: "Событий пока не было.",
            dash_no_alerts: "Оповещений не было.",
            dash_no_analytics: "Провайдер аналитики не подключён",
            dash_no_analytics_detail: "Подключите Яндекс.Метрику и укажите номер счётчика, чтобы видеть посетителей, визиты и просмотры рядом с инфраструктурой.",
            dash_connect_provider: "Подключить провайдер",
            tile_servers: "Серверы",
            tile_online: "В сети",
            tile_offline: "Не в сети",
            tile_websites: "Сайты",
            tile_average_cpu: "Средний ЦП",
            tile_average_ram: "Средняя память",
            tile_visitors: "Посетители",
            tile_visits: "Визиты",
            tile_page_views: "Просмотры",
            tile_bounce_rate: "Отказы",
            servers_filter: "Поиск по названию, хосту или метке",
            servers_empty: "Серверов пока нет",
            servers_empty_detail: "Добавьте Linux-сервер по SSH или установите на него агент, чтобы собирать метрики ЦП, памяти, диска и сети.",
            servers_add: "Добавить сервер",
            servers_add_a: "Добавить сервер",
            tab_overview: "Обзор",
            tab_metrics: "Метрики",
            tab_processes: "Процессы",
            tab_services: "Службы",
            tab_docker: "Docker",
            tab_websites: "Сайты",
            tab_events: "События",
            tab_settings: "Настройки",
            tab_analytics: "Аналитика",
            tab_availability: "Доступность",
            tab_ssl: "SSL",
            tab_screenshot: "Скриншот",
            tab_history: "История",
            tab_rules: "Правила",
            sd_system: "Система",
            sd_operating_system: "Операционная система",
            sd_kernel: "Ядро",
            sd_architecture: "Архитектура",
            sd_cores: "Ядер",
            sd_connection: "Подключение",
            sd_top_processes: "Основные процессы",
            sd_sorted_by_cpu: "По загрузке ЦП",
            sd_no_processes: "Информация о процессах не собрана.",
            sd_containers: "Контейнеры",
            sd_no_docker: "На этом хосте нет Docker",
            sd_no_docker_detail: "Мониторинг контейнеров появится сам, если Docker установят позже. На состояние сервера это не влияет.",
            sd_docker_empty: "Docker установлен, но контейнеров нет.",
            sd_no_systemd: "На этом хосте нет systemd",
            sd_no_systemd_detail: "Мониторинг служб доступен только в системах с systemd. Всё остальное по этому серверу собирается как обычно.",
            sd_no_websites: "К этому серверу не привязан ни один сайт",
            sd_no_websites_detail: "Привязка сайта к серверу позволяет приложению указывать на возможную связь между событием инфраструктуры и изменением трафика.",
            sd_no_events: "Событий по этому серверу пока не было.",
            sd_host_key: "Ключ хоста",
            sd_host_key_detail: "SSH-ключ этого сервера записан при первом подключении и проверяется при каждом следующем. Если ключ изменится, подключение прервётся.",
            sd_host_key_warning: "Забывайте ключ только после переустановки хоста или смены его ключей — и сверьте новый отпечаток с сервером перед подключением.",
            sd_forget_host_key: "Забыть ключ хоста",
            sd_forget_confirm: "Забыть записанный ключ?",
            sd_forget_it: "Забыть",
            sd_remove_server: "Удалить сервер",
            sd_remove_detail: "Удаляет сервер, его сохранённые учётные данные и историю метрик. Сам сервер не затрагивается — на нём ничего не установлено.",
            sd_remove_confirm_prefix: "Удалить ",
            sd_remove_confirm_suffix: " вместе с историей?",
            websites_empty: "Сайтов пока нет",
            websites_empty_detail: "Добавьте адрес, чтобы следить за доступностью, временем ответа и TLS-сертификатом — а если подключить аналитику, то и за трафиком.",
            websites_add: "Добавить сайт",
            websites_add_a: "Добавить сайт",
            websites_grid: "Плитка",
            websites_list: "Список",
            wd_http_status: "Код HTTP",
            wd_response_time: "Время ответа",
            wd_uptime_24h: "Доступность (24 ч)",
            wd_tls_certificate: "TLS-сертификат",
            wd_top_pages: "Популярные страницы",
            wd_no_events: "Событий по этому сайту пока не было.",
            wd_no_analytics: "Подключите Яндекс.Метрику в настройках, чтобы видеть посетителей, визиты и просмотры этого сайта.",
            wd_refresh_screenshot: "Обновить скриншот",
            wd_offline_no_shot: "Сайт сейчас недоступен",
            analytics_by_website: "По сайтам",
            analytics_vs_previous: "к прошлому периоду",
            alerts_nothing_firing: "Ничего не сработало",
            alerts_acknowledge: "Принять",
            alerts_acknowledged: "Принято",
            alerts_add_rule: "Добавить правило",
            alerts_open: "открыт",
            alerts_resolved: "закрыт",
            set_appearance: "Внешний вид",
            set_theme: "Тема",
            set_language: "Язык",
            set_notifications: "Уведомления",
            set_desktop_notifications: "Уведомления на рабочем столе",
            set_play_sound: "Звуковой сигнал",
            set_webhook_url: "Адрес вебхука",
            set_screenshots: "Скриншоты",
            set_no_browser: "Браузер на основе Chromium не найден, поэтому превью сайтов недоступны. Установите Chrome, Chromium, Edge или Brave — либо укажите путь к браузеру в файле конфигурации.",
            set_counter_id: "Номер счётчика",
            set_oauth_token: "OAuth-токен",
            set_token_hint: "Хранится в системном хранилище учётных данных, а не в базе",
            set_credential_storage: "Хранилище учётных данных",
            set_backend: "Хранилище",
            set_encrypted_file_warning: "Учётные данные хранятся в зашифрованном файле, потому что системное хранилище на этой машине недоступно. Шифрование надёжное, но системное хранилище безопаснее.",
            set_storage_diagnostics: "Хранение и диагностика",
            set_database: "База данных",
            set_logs: "Журналы",
            set_debug_mode: "Режим отладки (подробные журналы и панель планировщика)",
            dlg_add_server: "Добавление сервера",
            dlg_add_website: "Добавление сайта",
            dlg_ssh_subtitle: "Учётные данные сохраняются в системном хранилище, а не в базе.",
            dlg_agent_subtitle: "Агент уже должен быть установлен на этом хосте. Его установщик напечатал токен.",
            dlg_website_subtitle: "Проверяются DNS, подключение, код HTTP, время ответа и срок действия сертификата.",
            dlg_connect_via: "Подключение",
            dlg_mode_ssh: "SSH (агент не нужен)",
            dlg_mode_agent: "Агент (HTTPS)",
            dlg_username: "Пользователь",
            dlg_authentication: "Аутентификация",
            dlg_auth_password: "Пароль",
            dlg_auth_key: "Приватный ключ",
            dlg_auth_encrypted_key: "Зашифрованный приватный ключ",
            dlg_passphrase: "Парольная фраза",
            dlg_token: "Токен",
            dlg_poll_every: "Опрашивать раз в",
            dlg_check_every: "Проверять раз в",
            dlg_expected_status: "Ожидаемый код",
            dlg_expected_text: "Ожидаемый текст",
            dlg_url: "Адрес",
            dlg_ph_server_name: "prod-web-01",
            dlg_ph_host: "10.0.0.5 или web01.example.com",
            dlg_ph_username: "vds-monitor",
            dlg_ph_password: "Пароль этой учётной записи",
            dlg_ph_key: "Вставьте ключ целиком, вместе со строками BEGIN и END",
            dlg_ph_passphrase: "Парольная фраза от этого ключа",
            dlg_ph_token: "Напечатан установщиком агента",
            dlg_ph_seconds: "секунд",
            dlg_ph_website_name: "Сайт компании",
            dlg_ph_url: "example.com",
            dlg_ph_expected_text: "Необязательно — подстрока, которая должна быть на странице",
            dlg_ph_counter: "например, 12345678",
            dlg_ph_webhook: "https://hooks.example.com/…",
            dlg_scheme_hint: "Если схема не указана, подставляется https://.",
            dlg_expected_text_hint: "Оставьте текст пустым, чтобы проверять только код ответа. Пустое ожидание совпадает с любым ответом — такая проверка выглядит успешной, но ничего не проверяет.",
            dlg_fingerprint_hint: "При первом подключении приложение покажет отпечаток сертификата агента. Сверьте его с тем, что напечатал установщик.",
            status_online: "В сети",
            status_warning: "Предупреждение",
            status_critical: "Критично",
            status_offline: "Не в сети",
            status_unknown: "Неизвестно",
            theme_light: "Светлая",
            theme_dark: "Тёмная",
            theme_system: "Как в системе",
            range_1h: "1 час",
            range_6h: "6 часов",
            range_24h: "24 часа",
            range_7d: "7 дней",
            range_30d: "30 дней",
            range_90d: "90 дней",
            range_1y: "1 год",
            period_today: "Сегодня",
            period_yesterday: "Вчера",
            period_7d: "7 дней",
            period_30d: "30 дней",
            period_90d: "90 дней",
            am_visitors: "Посетители",
            am_visits: "Визиты",
            am_page_views: "Просмотры",
            am_sessions: "Сессии",
            am_unique_visitors: "Уникальные посетители",
            am_new_visitors: "Новые посетители",
            am_returning_visitors: "Вернувшиеся посетители",
            am_bounce_rate: "Отказы",
            am_session_duration: "Средняя длительность сессии",
            am_pages_per_session: "Страниц за сессию",
            policy_hourly: "Каждый час",
            policy_six_hours: "Каждые 6 часов",
            policy_daily: "Раз в сутки",
            policy_manual: "Вручную",
            mk_cpu: "ЦП",
            mk_ram: "Память",
            mk_ram_used: "Занято памяти",
            mk_swap: "Подкачка",
            mk_disk: "Диск",
            mk_disk_used: "Занято на диске",
            mk_network_in: "Сеть, приём",
            mk_network_out: "Сеть, передача",
            mk_load_1m: "Нагрузка 1 мин",
            mk_load_5m: "Нагрузка 5 мин",
            mk_load_15m: "Нагрузка 15 мин",
            mk_uptime: "Аптайм",
            mk_processes: "Процессы",
            mk_temperature: "Температура",
            mk_response_time: "Время ответа",
            mk_ssl_expiry: "Срок сертификата",
            time_never: "никогда",
            time_just_now: "только что",
            time_secs_ago: "{} с назад",
            time_mins_ago: "{} мин назад",
            time_hours_ago: "{} ч назад",
            time_days_ago: "{} дн назад",
            dur_days_hours: "{} д {} ч",
            dur_hours_mins: "{} ч {} мин",
            dur_mins: "{} мин",
            dur_secs: "{} с",
            ssl_expired_days_ago: "истёк {} дн назад",
            ssl_expires_today: "истекает сегодня",
            ssl_one_day: "1 день",
            ssl_days: "{} дн",
            card_response: "Ответ: {}",
            card_ssl: "SSL: {}",
            card_uptime_24h: "Доступность за 24 ч: {}",
            card_visitors_today: "Посетителей сегодня: {}",
            card_analytics_updated: "Аналитика обновлена {}",
            shot_captured: "Снято {}",
            shot_capturing: "Съёмка…",
            shot_none_yet: "Скриншота ещё нет",
            shot_offline: "Скриншот недоступен — сайт сейчас не отвечает",
            shot_failed: "Не удалось сделать скриншот: {}",
            shot_unsupported: "Скриншоты на этой машине недоступны",
            ev_server_status: "Сервер: {} → {}",
            ev_collection_failed: "Сбор не удался ({} раз подряд): {}",
            ev_website_status: "Сайт: {} → {}",
            ev_threshold: "{} достигло {}, порог {}",
            ev_certificate: "Сертификат {}",
            ev_traffic_anomaly: "Аномалия трафика: {} изменилось на {}",
            ev_analytics_refreshed: "Аналитика обновлена",
            ev_analytics_failed: "Не удалось обновить аналитику: {}",
            ev_screenshot_updated: "Скриншот обновлён",
            ev_screenshot_failed: "Не удалось снять скриншот: {}",
            ev_incident_resolved: "Инцидент закрыт",
            ev_container_state: "Контейнер {}: {}",
            ev_service_state: "Служба {}: {}",
            ev_website_checked: "Сайт проверен",
            ev_metrics_collected: "Собрано метрик: {}",
            incident_open_for: "Открыт {}",
            err_server_name_empty: "Укажите название сервера",
            err_server_host_empty: "Укажите адрес сервера",
            err_port_invalid: "Порт должен быть от 1 до 65535",
            err_interval_invalid: "Интервал должен быть не меньше 1 секунды",
            err_failures_invalid: "Порог должен быть не меньше одной неудачной проверки",
            err_timeout_invalid: "Таймаут должен быть не меньше 1 секунды",
            err_timeout_too_long: "Таймаут не должен превышать четыре интервала опроса, иначе проверки начнут накапливаться",
            err_thresholds_inverted: "Пороги предупреждения и критического уровня перепутаны местами",
            err_website_name_empty: "Укажите название сайта",
            err_url_malformed: "Это не похоже на корректный адрес",
            err_url_scheme: "Отслеживаются только адреса http и https",
            err_url_no_host: "В адресе нет имени хоста",
            err_status_invalid: "Ожидаемый код должен быть от 100 до 599",
            err_credential_missing: "Введите пароль, ключ или токен для этого подключения",
            err_credential_store: "Не удалось сохранить учётные данные: {}",
            err_save_failed: "Не удалось сохранить: {}",
        }
    }
}

/// The language the process is currently speaking.
///
/// Process-global because it genuinely is: every formatted string in the
/// interface is in one language at a time, and threading a `&Strings` through
/// `format::relative_time` and each of its callers would put a parameter on forty
/// functions to express a fact that never varies between them.
///
/// Stored as an index into [`Language::ALL`] so both the UI thread and the worker
/// can read it without a lock.
static CURRENT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Sets the language every later formatting call will use.
pub fn set_current(language: Language) {
    let index = usize::try_from(language.index()).unwrap_or(0);
    CURRENT.store(index, std::sync::atomic::Ordering::Relaxed);
}

/// The language in force.
pub fn current() -> Language {
    let index = CURRENT.load(std::sync::atomic::Ordering::Relaxed);
    Language::ALL
        .get(index)
        .copied()
        .unwrap_or(Language::English)
}

/// The catalogue in force.
pub fn strings() -> Strings {
    current().strings()
}

/// Pushes a catalogue into the window.
///
/// Must run on the UI thread. Called once at startup and again whenever the
/// language changes — Slint re-renders every binding that reads a changed
/// property, so switching takes effect without a restart.
pub fn apply(window: &AppWindow, strings: &Strings) {
    let global = window.global::<L>();
    global.set_app_title(strings.app_title.into());
    global.set_app_subtitle(strings.app_subtitle.into());
    global.set_nav_dashboard(strings.nav_dashboard.into());
    global.set_nav_home(strings.nav_home.into());
    global.set_nav_servers(strings.nav_servers.into());
    global.set_nav_websites(strings.nav_websites.into());
    global.set_nav_sites_short(strings.nav_sites_short.into());
    global.set_nav_analytics(strings.nav_analytics.into());
    global.set_nav_alerts(strings.nav_alerts.into());
    global.set_nav_settings(strings.nav_settings.into());
    global.set_nav_more(strings.nav_more.into());
    global.set_action_add(strings.action_add.into());
    global.set_action_cancel(strings.action_cancel.into());
    global.set_action_save(strings.action_save.into());
    global.set_action_remove(strings.action_remove.into());
    global.set_action_refresh(strings.action_refresh.into());
    global.set_action_retry(strings.action_retry.into());
    global.set_action_open(strings.action_open.into());
    global.set_action_back(strings.action_back.into());
    global.set_action_working(strings.action_working.into());
    global.set_label_name(strings.label_name.into());
    global.set_label_status(strings.label_status.into());
    global.set_label_host(strings.label_host.into());
    global.set_label_port(strings.label_port.into());
    global.set_label_address(strings.label_address.into());
    global.set_label_uptime(strings.label_uptime.into());
    global.set_label_last_check(strings.label_last_check.into());
    global.set_label_cpu(strings.label_cpu.into());
    global.set_label_ram(strings.label_ram.into());
    global.set_label_memory(strings.label_memory.into());
    global.set_label_disk(strings.label_disk.into());
    global.set_label_subject(strings.label_subject.into());
    global.set_label_size(strings.label_size.into());
    global.set_label_expires(strings.label_expires.into());
    global.set_label_issuer(strings.label_issuer.into());
    global.set_not_measured(strings.not_measured.into());
    global.set_no_measurement(strings.no_measurement.into());
    global.set_nothing_here_yet(strings.nothing_here_yet.into());
    global.set_could_not_load(strings.could_not_load.into());
    global.set_dash_recent_alerts(strings.dash_recent_alerts.into());
    global.set_dash_recent_events(strings.dash_recent_events.into());
    global.set_dash_problem_servers(strings.dash_problem_servers.into());
    global.set_dash_needs_attention(strings.dash_needs_attention.into());
    global.set_dash_preview(strings.dash_preview.into());
    global.set_dash_all_healthy(strings.dash_all_healthy.into());
    global.set_dash_nothing_monitored(strings.dash_nothing_monitored.into());
    global.set_dash_nothing_monitored_detail(strings.dash_nothing_monitored_detail.into());
    global.set_dash_nothing_happened(strings.dash_nothing_happened.into());
    global.set_dash_no_alerts(strings.dash_no_alerts.into());
    global.set_dash_no_analytics(strings.dash_no_analytics.into());
    global.set_dash_no_analytics_detail(strings.dash_no_analytics_detail.into());
    global.set_dash_connect_provider(strings.dash_connect_provider.into());
    global.set_tile_servers(strings.tile_servers.into());
    global.set_tile_online(strings.tile_online.into());
    global.set_tile_offline(strings.tile_offline.into());
    global.set_tile_websites(strings.tile_websites.into());
    global.set_tile_average_cpu(strings.tile_average_cpu.into());
    global.set_tile_average_ram(strings.tile_average_ram.into());
    global.set_tile_visitors(strings.tile_visitors.into());
    global.set_tile_visits(strings.tile_visits.into());
    global.set_tile_page_views(strings.tile_page_views.into());
    global.set_tile_bounce_rate(strings.tile_bounce_rate.into());
    global.set_servers_filter(strings.servers_filter.into());
    global.set_servers_empty(strings.servers_empty.into());
    global.set_servers_empty_detail(strings.servers_empty_detail.into());
    global.set_servers_add(strings.servers_add.into());
    global.set_servers_add_a(strings.servers_add_a.into());
    global.set_tab_overview(strings.tab_overview.into());
    global.set_tab_metrics(strings.tab_metrics.into());
    global.set_tab_processes(strings.tab_processes.into());
    global.set_tab_services(strings.tab_services.into());
    global.set_tab_docker(strings.tab_docker.into());
    global.set_tab_websites(strings.tab_websites.into());
    global.set_tab_events(strings.tab_events.into());
    global.set_tab_settings(strings.tab_settings.into());
    global.set_tab_analytics(strings.tab_analytics.into());
    global.set_tab_availability(strings.tab_availability.into());
    global.set_tab_ssl(strings.tab_ssl.into());
    global.set_tab_screenshot(strings.tab_screenshot.into());
    global.set_tab_history(strings.tab_history.into());
    global.set_tab_rules(strings.tab_rules.into());
    global.set_sd_system(strings.sd_system.into());
    global.set_sd_operating_system(strings.sd_operating_system.into());
    global.set_sd_kernel(strings.sd_kernel.into());
    global.set_sd_architecture(strings.sd_architecture.into());
    global.set_sd_cores(strings.sd_cores.into());
    global.set_sd_connection(strings.sd_connection.into());
    global.set_sd_top_processes(strings.sd_top_processes.into());
    global.set_sd_sorted_by_cpu(strings.sd_sorted_by_cpu.into());
    global.set_sd_no_processes(strings.sd_no_processes.into());
    global.set_sd_containers(strings.sd_containers.into());
    global.set_sd_no_docker(strings.sd_no_docker.into());
    global.set_sd_no_docker_detail(strings.sd_no_docker_detail.into());
    global.set_sd_docker_empty(strings.sd_docker_empty.into());
    global.set_sd_no_systemd(strings.sd_no_systemd.into());
    global.set_sd_no_systemd_detail(strings.sd_no_systemd_detail.into());
    global.set_sd_no_websites(strings.sd_no_websites.into());
    global.set_sd_no_websites_detail(strings.sd_no_websites_detail.into());
    global.set_sd_no_events(strings.sd_no_events.into());
    global.set_sd_host_key(strings.sd_host_key.into());
    global.set_sd_host_key_detail(strings.sd_host_key_detail.into());
    global.set_sd_host_key_warning(strings.sd_host_key_warning.into());
    global.set_sd_forget_host_key(strings.sd_forget_host_key.into());
    global.set_sd_forget_confirm(strings.sd_forget_confirm.into());
    global.set_sd_forget_it(strings.sd_forget_it.into());
    global.set_sd_remove_server(strings.sd_remove_server.into());
    global.set_sd_remove_detail(strings.sd_remove_detail.into());
    global.set_sd_remove_confirm_prefix(strings.sd_remove_confirm_prefix.into());
    global.set_sd_remove_confirm_suffix(strings.sd_remove_confirm_suffix.into());
    global.set_websites_empty(strings.websites_empty.into());
    global.set_websites_empty_detail(strings.websites_empty_detail.into());
    global.set_websites_add(strings.websites_add.into());
    global.set_websites_add_a(strings.websites_add_a.into());
    global.set_websites_grid(strings.websites_grid.into());
    global.set_websites_list(strings.websites_list.into());
    global.set_wd_http_status(strings.wd_http_status.into());
    global.set_wd_response_time(strings.wd_response_time.into());
    global.set_wd_uptime_24h(strings.wd_uptime_24h.into());
    global.set_wd_tls_certificate(strings.wd_tls_certificate.into());
    global.set_wd_top_pages(strings.wd_top_pages.into());
    global.set_wd_no_events(strings.wd_no_events.into());
    global.set_wd_no_analytics(strings.wd_no_analytics.into());
    global.set_wd_refresh_screenshot(strings.wd_refresh_screenshot.into());
    global.set_wd_offline_no_shot(strings.wd_offline_no_shot.into());
    global.set_analytics_by_website(strings.analytics_by_website.into());
    global.set_analytics_vs_previous(strings.analytics_vs_previous.into());
    global.set_alerts_nothing_firing(strings.alerts_nothing_firing.into());
    global.set_alerts_acknowledge(strings.alerts_acknowledge.into());
    global.set_alerts_acknowledged(strings.alerts_acknowledged.into());
    global.set_alerts_add_rule(strings.alerts_add_rule.into());
    global.set_alerts_open(strings.alerts_open.into());
    global.set_alerts_resolved(strings.alerts_resolved.into());
    global.set_set_appearance(strings.set_appearance.into());
    global.set_set_theme(strings.set_theme.into());
    global.set_set_language(strings.set_language.into());
    global.set_set_notifications(strings.set_notifications.into());
    global.set_set_desktop_notifications(strings.set_desktop_notifications.into());
    global.set_set_play_sound(strings.set_play_sound.into());
    global.set_set_webhook_url(strings.set_webhook_url.into());
    global.set_set_screenshots(strings.set_screenshots.into());
    global.set_set_no_browser(strings.set_no_browser.into());
    global.set_set_counter_id(strings.set_counter_id.into());
    global.set_set_oauth_token(strings.set_oauth_token.into());
    global.set_set_token_hint(strings.set_token_hint.into());
    global.set_set_credential_storage(strings.set_credential_storage.into());
    global.set_set_backend(strings.set_backend.into());
    global.set_set_encrypted_file_warning(strings.set_encrypted_file_warning.into());
    global.set_set_storage_diagnostics(strings.set_storage_diagnostics.into());
    global.set_set_database(strings.set_database.into());
    global.set_set_logs(strings.set_logs.into());
    global.set_set_debug_mode(strings.set_debug_mode.into());
    global.set_dlg_add_server(strings.dlg_add_server.into());
    global.set_dlg_add_website(strings.dlg_add_website.into());
    global.set_dlg_ssh_subtitle(strings.dlg_ssh_subtitle.into());
    global.set_dlg_agent_subtitle(strings.dlg_agent_subtitle.into());
    global.set_dlg_website_subtitle(strings.dlg_website_subtitle.into());
    global.set_dlg_connect_via(strings.dlg_connect_via.into());
    global.set_dlg_mode_ssh(strings.dlg_mode_ssh.into());
    global.set_dlg_mode_agent(strings.dlg_mode_agent.into());
    global.set_dlg_username(strings.dlg_username.into());
    global.set_dlg_authentication(strings.dlg_authentication.into());
    global.set_dlg_auth_password(strings.dlg_auth_password.into());
    global.set_dlg_auth_key(strings.dlg_auth_key.into());
    global.set_dlg_auth_encrypted_key(strings.dlg_auth_encrypted_key.into());
    global.set_dlg_passphrase(strings.dlg_passphrase.into());
    global.set_dlg_token(strings.dlg_token.into());
    global.set_dlg_poll_every(strings.dlg_poll_every.into());
    global.set_dlg_check_every(strings.dlg_check_every.into());
    global.set_dlg_expected_status(strings.dlg_expected_status.into());
    global.set_dlg_expected_text(strings.dlg_expected_text.into());
    global.set_dlg_url(strings.dlg_url.into());
    global.set_dlg_ph_server_name(strings.dlg_ph_server_name.into());
    global.set_dlg_ph_host(strings.dlg_ph_host.into());
    global.set_dlg_ph_username(strings.dlg_ph_username.into());
    global.set_dlg_ph_password(strings.dlg_ph_password.into());
    global.set_dlg_ph_key(strings.dlg_ph_key.into());
    global.set_dlg_ph_passphrase(strings.dlg_ph_passphrase.into());
    global.set_dlg_ph_token(strings.dlg_ph_token.into());
    global.set_dlg_ph_seconds(strings.dlg_ph_seconds.into());
    global.set_dlg_ph_website_name(strings.dlg_ph_website_name.into());
    global.set_dlg_ph_url(strings.dlg_ph_url.into());
    global.set_dlg_ph_expected_text(strings.dlg_ph_expected_text.into());
    global.set_dlg_ph_counter(strings.dlg_ph_counter.into());
    global.set_dlg_ph_webhook(strings.dlg_ph_webhook.into());
    global.set_dlg_scheme_hint(strings.dlg_scheme_hint.into());
    global.set_dlg_expected_text_hint(strings.dlg_expected_text_hint.into());
    global.set_dlg_fingerprint_hint(strings.dlg_fingerprint_hint.into());
    global.set_status_online(strings.status_online.into());
    global.set_status_warning(strings.status_warning.into());
    global.set_status_critical(strings.status_critical.into());
    global.set_status_offline(strings.status_offline.into());
    global.set_status_unknown(strings.status_unknown.into());
    global.set_theme_light(strings.theme_light.into());
    global.set_theme_dark(strings.theme_dark.into());
    global.set_theme_system(strings.theme_system.into());
    global.set_range_1h(strings.range_1h.into());
    global.set_range_6h(strings.range_6h.into());
    global.set_range_24h(strings.range_24h.into());
    global.set_range_7d(strings.range_7d.into());
    global.set_range_30d(strings.range_30d.into());
    global.set_range_90d(strings.range_90d.into());
    global.set_range_1y(strings.range_1y.into());
    global.set_period_today(strings.period_today.into());
    global.set_period_yesterday(strings.period_yesterday.into());
    global.set_period_7d(strings.period_7d.into());
    global.set_period_30d(strings.period_30d.into());
    global.set_period_90d(strings.period_90d.into());
    global.set_am_visitors(strings.am_visitors.into());
    global.set_am_visits(strings.am_visits.into());
    global.set_am_page_views(strings.am_page_views.into());
    global.set_am_sessions(strings.am_sessions.into());
    global.set_am_unique_visitors(strings.am_unique_visitors.into());
    global.set_am_new_visitors(strings.am_new_visitors.into());
    global.set_am_returning_visitors(strings.am_returning_visitors.into());
    global.set_am_bounce_rate(strings.am_bounce_rate.into());
    global.set_am_session_duration(strings.am_session_duration.into());
    global.set_am_pages_per_session(strings.am_pages_per_session.into());
    global.set_policy_hourly(strings.policy_hourly.into());
    global.set_policy_six_hours(strings.policy_six_hours.into());
    global.set_policy_daily(strings.policy_daily.into());
    global.set_policy_manual(strings.policy_manual.into());
    global.set_mk_cpu(strings.mk_cpu.into());
    global.set_mk_ram(strings.mk_ram.into());
    global.set_mk_ram_used(strings.mk_ram_used.into());
    global.set_mk_swap(strings.mk_swap.into());
    global.set_mk_disk(strings.mk_disk.into());
    global.set_mk_disk_used(strings.mk_disk_used.into());
    global.set_mk_network_in(strings.mk_network_in.into());
    global.set_mk_network_out(strings.mk_network_out.into());
    global.set_mk_load_1m(strings.mk_load_1m.into());
    global.set_mk_load_5m(strings.mk_load_5m.into());
    global.set_mk_load_15m(strings.mk_load_15m.into());
    global.set_mk_uptime(strings.mk_uptime.into());
    global.set_mk_processes(strings.mk_processes.into());
    global.set_mk_temperature(strings.mk_temperature.into());
    global.set_mk_response_time(strings.mk_response_time.into());
    global.set_mk_ssl_expiry(strings.mk_ssl_expiry.into());
    global.set_time_never(strings.time_never.into());
    global.set_time_just_now(strings.time_just_now.into());
    global.set_time_secs_ago(strings.time_secs_ago.into());
    global.set_time_mins_ago(strings.time_mins_ago.into());
    global.set_time_hours_ago(strings.time_hours_ago.into());
    global.set_time_days_ago(strings.time_days_ago.into());
    global.set_dur_days_hours(strings.dur_days_hours.into());
    global.set_dur_hours_mins(strings.dur_hours_mins.into());
    global.set_dur_mins(strings.dur_mins.into());
    global.set_dur_secs(strings.dur_secs.into());
    global.set_ssl_expired_days_ago(strings.ssl_expired_days_ago.into());
    global.set_ssl_expires_today(strings.ssl_expires_today.into());
    global.set_ssl_one_day(strings.ssl_one_day.into());
    global.set_ssl_days(strings.ssl_days.into());
    global.set_card_response(strings.card_response.into());
    global.set_card_ssl(strings.card_ssl.into());
    global.set_card_uptime_24h(strings.card_uptime_24h.into());
    global.set_card_visitors_today(strings.card_visitors_today.into());
    global.set_card_analytics_updated(strings.card_analytics_updated.into());
    global.set_shot_captured(strings.shot_captured.into());
    global.set_shot_capturing(strings.shot_capturing.into());
    global.set_shot_none_yet(strings.shot_none_yet.into());
    global.set_shot_offline(strings.shot_offline.into());
    global.set_shot_failed(strings.shot_failed.into());
    global.set_shot_unsupported(strings.shot_unsupported.into());
    global.set_ev_server_status(strings.ev_server_status.into());
    global.set_ev_collection_failed(strings.ev_collection_failed.into());
    global.set_ev_website_status(strings.ev_website_status.into());
    global.set_ev_threshold(strings.ev_threshold.into());
    global.set_ev_certificate(strings.ev_certificate.into());
    global.set_ev_traffic_anomaly(strings.ev_traffic_anomaly.into());
    global.set_ev_analytics_refreshed(strings.ev_analytics_refreshed.into());
    global.set_ev_analytics_failed(strings.ev_analytics_failed.into());
    global.set_ev_screenshot_updated(strings.ev_screenshot_updated.into());
    global.set_ev_screenshot_failed(strings.ev_screenshot_failed.into());
    global.set_ev_incident_resolved(strings.ev_incident_resolved.into());
    global.set_ev_container_state(strings.ev_container_state.into());
    global.set_ev_service_state(strings.ev_service_state.into());
    global.set_ev_website_checked(strings.ev_website_checked.into());
    global.set_ev_metrics_collected(strings.ev_metrics_collected.into());
    global.set_incident_open_for(strings.incident_open_for.into());
    global.set_err_server_name_empty(strings.err_server_name_empty.into());
    global.set_err_server_host_empty(strings.err_server_host_empty.into());
    global.set_err_port_invalid(strings.err_port_invalid.into());
    global.set_err_interval_invalid(strings.err_interval_invalid.into());
    global.set_err_failures_invalid(strings.err_failures_invalid.into());
    global.set_err_timeout_invalid(strings.err_timeout_invalid.into());
    global.set_err_timeout_too_long(strings.err_timeout_too_long.into());
    global.set_err_thresholds_inverted(strings.err_thresholds_inverted.into());
    global.set_err_website_name_empty(strings.err_website_name_empty.into());
    global.set_err_url_malformed(strings.err_url_malformed.into());
    global.set_err_url_scheme(strings.err_url_scheme.into());
    global.set_err_url_no_host(strings.err_url_no_host.into());
    global.set_err_status_invalid(strings.err_status_invalid.into());
    global.set_err_credential_missing(strings.err_credential_missing.into());
    global.set_err_credential_store(strings.err_credential_store.into());
    global.set_err_save_failed(strings.err_save_failed.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_string_is_translated_in_every_language() {
        // An empty string would render as a blank label rather than as an obvious
        // mistake, so it is caught here instead of on screen.
        for language in Language::ALL {
            let strings = language.strings();
            let rendered = format!("{strings:?}");
            assert!(
                !rendered.contains(": \"\""),
                "{} has an empty string",
                language.as_str()
            );
        }
    }

    #[test]
    fn placeholders_survive_translation() {
        // A translation that drops its `{}` silently loses the number it was
        // meant to carry, which is the one localisation bug nobody spots.
        let english = Strings::english();
        let russian = Strings::russian();

        let pairs: [(&str, &str, &str); 14] = [
            (
                "time_secs_ago",
                english.time_secs_ago,
                russian.time_secs_ago,
            ),
            (
                "time_mins_ago",
                english.time_mins_ago,
                russian.time_mins_ago,
            ),
            (
                "time_hours_ago",
                english.time_hours_ago,
                russian.time_hours_ago,
            ),
            (
                "time_days_ago",
                english.time_days_ago,
                russian.time_days_ago,
            ),
            ("dur_mins", english.dur_mins, russian.dur_mins),
            ("dur_secs", english.dur_secs, russian.dur_secs),
            ("ssl_days", english.ssl_days, russian.ssl_days),
            (
                "ssl_expired_days_ago",
                english.ssl_expired_days_ago,
                russian.ssl_expired_days_ago,
            ),
            (
                "card_response",
                english.card_response,
                russian.card_response,
            ),
            ("card_ssl", english.card_ssl, russian.card_ssl),
            (
                "card_uptime_24h",
                english.card_uptime_24h,
                russian.card_uptime_24h,
            ),
            (
                "card_visitors_today",
                english.card_visitors_today,
                russian.card_visitors_today,
            ),
            (
                "shot_captured",
                english.shot_captured,
                russian.shot_captured,
            ),
            (
                "incident_open_for",
                english.incident_open_for,
                russian.incident_open_for,
            ),
        ];

        for (key, en, ru) in pairs {
            assert!(en.contains("{}"), "{key} lost its placeholder in English");
            assert!(ru.contains("{}"), "{key} lost its placeholder in Russian");
        }
    }

    #[test]
    fn two_placeholder_strings_keep_both() {
        for strings in [Strings::english(), Strings::russian()] {
            assert_eq!(strings.dur_days_hours.matches("{}").count(), 2);
            assert_eq!(strings.dur_hours_mins.matches("{}").count(), 2);
            assert_eq!(strings.ev_server_status.matches("{}").count(), 2);
            assert_eq!(strings.ev_container_state.matches("{}").count(), 2);
        }
    }

    #[test]
    fn a_configured_code_wins_over_the_system() {
        assert_eq!(Language::resolve("ru"), Language::Russian);
        assert_eq!(Language::resolve("en"), Language::English);
        assert_eq!(Language::resolve("RU"), Language::Russian);
        assert_eq!(Language::resolve(" ru "), Language::Russian);
    }

    #[test]
    fn an_unknown_code_falls_back_rather_than_leaving_the_app_wordless() {
        // A typo in a configuration file must not produce a blank interface.
        let fallback = Language::from_system();
        assert_eq!(Language::resolve("klingon"), fallback);
        assert_eq!(Language::resolve(""), fallback);
        assert_eq!(Language::resolve("system"), fallback);
    }

    #[test]
    fn the_picker_round_trips() {
        for language in Language::ALL {
            assert_eq!(Language::at(language.index()), *language);
        }
        // Out of range must not panic; the index comes from the view.
        assert_eq!(Language::at(-1), Language::English);
        assert_eq!(Language::at(99), Language::English);
    }

    #[test]
    fn every_language_names_itself_in_itself() {
        // So a speaker can find their language without first reading another.
        assert_eq!(Language::Russian.endonym(), "Русский");
        assert_eq!(Language::English.endonym(), "English");
        for language in Language::ALL {
            assert!(!language.endonym().is_empty());
            assert!(!language.as_str().is_empty());
        }
    }
}
