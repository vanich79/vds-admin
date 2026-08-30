//! Ports: every trait the outside world must implement.
//!
//! This module is the entire surface through which the domain touches reality. Nothing
//! here mentions SQLite, SSH, HTTP, Slint or any provider by name — which is exactly
//! what makes those replaceable.
//!
//! Dependency direction:
//!
//! ```text
//! application ──uses──▶ ports ◀──implements── infrastructure
//! ```

mod analytics;
mod clock;
mod events;
mod files;
mod notification;
mod repositories;
mod screenshot;
mod secrets;
mod transport;

pub use analytics::{AnalyticsProvider, AnalyticsQuery, ProviderError, ProviderHealth};
pub use clock::{Clock, FixedClock, SystemClock};
pub use events::{EventPublisher, NullEventPublisher, RecordingEventPublisher};
pub use files::{
    DEFAULT_MAX_READ_BYTES, DirectoryEntry, EntryKind, FileBrowser, FileBytes, FileContents,
    FileError,
};
pub use notification::{NotificationCapabilities, NotificationError, NotificationProvider};
pub use repositories::{
    AlertRepository, AnalyticsRepository, EventRepository, MetricsRepository, RepositoryError,
    ScreenshotRepository, ServerRepository, WebsiteRepository,
};
pub use screenshot::{ScreenshotError, ScreenshotProvider};
pub use secrets::{Secret, SecretKind, SecretStore, SecretStoreError};
pub use transport::{
    Capability, CollectError, Collector, CollectorOutput, Command, CommandOutput, CommandRunner,
    SAMPLE_SEPARATOR, ServerProbe, TransportCapabilities, TransportError, TransportErrorKind,
    shell_quote,
};
