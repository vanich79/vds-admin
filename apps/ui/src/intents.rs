//! What the interface can ask for.
//!
//! One enum, in one place. Callbacks push an intent and return immediately; the worker
//! decides what it means. Modelling requests as data rather than as closures is what
//! keeps the UI thread free, and it makes the set of things the application can be asked
//! to do something you can read in a single screen.

use crate::i18n::Language;
use vds_application::provisioning::{NewServer, NewWebsite, ServerEdit};
use vds_domain::ids::{IncidentId, ServerId, WebsiteId};
use vds_domain::ports::Secret;

/// What the user asked for.
///
/// Callbacks push one of these and return immediately; the worker decides what it means.
/// Modelling intents as data rather than as closures is what keeps the UI thread free
/// and makes the set of things the interface can ask for enumerable in one place.
#[derive(Debug, Clone)]
pub(crate) enum Intent {
    RefreshDashboard,
    RefreshServers,
    RefreshWebsites,
    RefreshAlerts,
    RefreshAnalytics,
    OpenServer(ServerId),
    OpenWebsite(WebsiteId),
    ChangeRange(i32),
    ChangeWebsitePeriod(i32),
    ChangeAnalyticsPeriod(i32),
    ChangeAnalyticsMetric(i32),
    CollectServerNow(ServerId),
    CreateServer(Box<NewServer>),
    CreateWebsite(Box<NewWebsite>),
    ChangeLanguage(Language),
    ForgetHostKey(ServerId),
    DeleteServer(ServerId),
    CaptureScreenshotNow(WebsiteId),
    AcknowledgeIncident(IncidentId),
    ToggleRule(vds_domain::ids::AlertRuleId),
    SaveAnalyticsToken(Secret),
    UpdateServer {
        id: ServerId,
        edit: Box<ServerEdit>,
    },
    UpdateWebsite {
        id: WebsiteId,
        edit: Box<NewWebsite>,
    },
    BeginEditServer(ServerId),
    BeginEditWebsite(WebsiteId),
    ConnectAnalytics {
        website: WebsiteId,
        counter: String,
    },
    DisconnectAnalytics(WebsiteId),

    // --- files ---
    // The one group of intents that changes something on a server. Each carries only a
    // name or a path; which server it applies to is the one the worker has open, so a
    // stale click cannot land on a different machine.
    OpenFiles,
    BrowseTo(String),
    OpenFileEntry {
        name: String,
        is_directory: bool,
    },
    SaveOpenFile(String),
    CloseOpenFile,
    DeleteFileEntry(String),
    CreateFileEntry {
        name: String,
        is_directory: bool,
    },
    RefreshFiles,
}
