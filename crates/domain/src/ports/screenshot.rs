//! The screenshot provider port. See `docs/adr/004-screenshot-architecture.md`.

use crate::ids::ProviderId;
use crate::screenshot::{CaptureRequest, CapturedImage, ScreenshotCapabilities};
use async_trait::async_trait;

/// Why a capture failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScreenshotError {
    /// No browser or capture backend exists on this machine.
    #[error("no screenshot backend is available: {0}")]
    BackendUnavailable(String),
    #[error("the page could not be loaded: {0}")]
    Navigation(String),
    #[error("capture timed out after {seconds}s")]
    Timeout { seconds: u64 },
    #[error("the capture backend failed: {0}")]
    Backend(String),
    #[error("the captured image is not usable: {0}")]
    InvalidImage(String),
}

impl ScreenshotError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ScreenshotError::Navigation(_)
                | ScreenshotError::Timeout { .. }
                | ScreenshotError::Backend(_)
        )
    }
}

/// Captures a picture of a web page.
///
/// Implementations must respect [`CaptureRequest::timeout_secs`] and must never block
/// the caller for longer.
#[async_trait]
pub trait ScreenshotProvider: Send + Sync {
    fn id(&self) -> ProviderId;

    fn display_name(&self) -> &'static str;

    fn capabilities(&self) -> ScreenshotCapabilities;

    /// Whether this provider can currently work.
    ///
    /// Checked before scheduling any capture, so that a machine without a browser
    /// reports the feature as unavailable instead of accumulating failures.
    async fn is_available(&self) -> bool;

    async fn capture(&self, request: &CaptureRequest) -> Result<CapturedImage, ScreenshotError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_backend_is_not_worth_retrying() {
        // Retrying would just fail identically until the user installs a browser.
        assert!(!ScreenshotError::BackendUnavailable("no chrome".into()).is_retryable());
        assert!(ScreenshotError::Timeout { seconds: 30 }.is_retryable());
        assert!(ScreenshotError::Navigation("dns".into()).is_retryable());
    }
}
