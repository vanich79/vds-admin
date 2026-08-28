//! Website screenshots: the stored record and the freshness policy.
//!
//! The honesty rule from the brief lives here: a cached image is always accompanied by
//! its capture time, and a stale capture is never presented as current.

use crate::ids::{ProviderId, WebsiteId};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// How often screenshots are refreshed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotRefreshPolicy {
    Hourly,
    #[default]
    EverySixHours,
    Daily,
    /// Only when the user asks.
    Manual,
}

impl ScreenshotRefreshPolicy {
    /// How long a capture stays fresh. `None` for [`Manual`](Self::Manual), which never
    /// expires on its own.
    pub fn max_age(self) -> Option<Duration> {
        match self {
            ScreenshotRefreshPolicy::Hourly => Some(Duration::hours(1)),
            ScreenshotRefreshPolicy::EverySixHours => Some(Duration::hours(6)),
            ScreenshotRefreshPolicy::Daily => Some(Duration::hours(24)),
            ScreenshotRefreshPolicy::Manual => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ScreenshotRefreshPolicy::Hourly => "hourly",
            ScreenshotRefreshPolicy::EverySixHours => "six_hourly",
            ScreenshotRefreshPolicy::Daily => "daily",
            ScreenshotRefreshPolicy::Manual => "manual",
        }
    }

    pub fn parse(raw: &str) -> Option<ScreenshotRefreshPolicy> {
        match raw {
            "hourly" => Some(ScreenshotRefreshPolicy::Hourly),
            "six_hourly" => Some(ScreenshotRefreshPolicy::EverySixHours),
            "daily" => Some(ScreenshotRefreshPolicy::Daily),
            "manual" => Some(ScreenshotRefreshPolicy::Manual),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScreenshotRefreshPolicy::Hourly => "Every hour",
            ScreenshotRefreshPolicy::EverySixHours => "Every 6 hours",
            ScreenshotRefreshPolicy::Daily => "Every 24 hours",
            ScreenshotRefreshPolicy::Manual => "Manual",
        }
    }

    pub const ALL: &'static [ScreenshotRefreshPolicy] = &[
        ScreenshotRefreshPolicy::Hourly,
        ScreenshotRefreshPolicy::EverySixHours,
        ScreenshotRefreshPolicy::Daily,
        ScreenshotRefreshPolicy::Manual,
    ];
}

/// Outcome of the most recent capture attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ScreenshotStatus {
    /// A capture exists and is usable.
    Captured,
    /// The site was unreachable, so there is nothing meaningful to capture.
    WebsiteOffline,
    /// The capture mechanism itself failed.
    Failed { reason: String },
    /// No capture provider is available on this platform or machine.
    Unavailable,
}

impl ScreenshotStatus {
    pub fn is_captured(&self) -> bool {
        matches!(self, ScreenshotStatus::Captured)
    }
}

/// A stored screenshot of a website.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Screenshot {
    pub website_id: WebsiteId,
    pub provider: ProviderId,
    /// Path to the full-size PNG, relative to the screenshot cache directory.
    pub path: String,
    /// Path to the downscaled preview, loaded first on mobile.
    pub thumbnail_path: Option<String>,
    pub captured_at: DateTime<Utc>,
    pub status: ScreenshotStatus,
    /// Content hash, used to skip rewriting an unchanged capture.
    pub hash: String,
    pub width: u32,
    pub height: u32,
}

impl Screenshot {
    /// How long ago this was captured.
    pub fn age(&self, now: DateTime<Utc>) -> Duration {
        now - self.captured_at
    }

    /// Whether the policy considers this capture due for refresh.
    pub fn is_stale(&self, policy: ScreenshotRefreshPolicy, now: DateTime<Utc>) -> bool {
        match policy.max_age() {
            Some(max_age) => self.age(now) >= max_age,
            None => false,
        }
    }
}

/// What the UI should render for a website's preview.
///
/// Making this an explicit enum is what prevents the "silently show a four-hour-old
/// image as if it were live" failure mode: every branch the UI can render forces the
/// age to be carried alongside the image.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScreenshotPresentation {
    /// Show the image, labelled with how old it is.
    Cached {
        screenshot: Screenshot,
        age: Duration,
    },
    /// A capture is in flight.
    Capturing,
    /// The site is down, so there is nothing to show.
    WebsiteOffline,
    /// Capture failed; offer a retry.
    Failed { reason: String },
    /// Screenshots are not supported here at all.
    Unavailable,
    /// Nothing has been captured yet.
    NotCaptured,
}

impl ScreenshotPresentation {
    /// Builds the presentation for a stored screenshot.
    pub fn from_stored(screenshot: Option<Screenshot>, now: DateTime<Utc>) -> Self {
        match screenshot {
            None => ScreenshotPresentation::NotCaptured,
            Some(shot) => match &shot.status {
                ScreenshotStatus::Captured => {
                    let age = shot.age(now);
                    ScreenshotPresentation::Cached {
                        screenshot: shot,
                        age,
                    }
                }
                ScreenshotStatus::WebsiteOffline => ScreenshotPresentation::WebsiteOffline,
                ScreenshotStatus::Failed { reason } => ScreenshotPresentation::Failed {
                    reason: reason.clone(),
                },
                ScreenshotStatus::Unavailable => ScreenshotPresentation::Unavailable,
            },
        }
    }

    /// Whether an image is available to draw.
    pub fn has_image(&self) -> bool {
        matches!(self, ScreenshotPresentation::Cached { .. })
    }
}

/// Requested capture parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRequest {
    pub website_id: WebsiteId,
    pub url: String,
    pub viewport_width: u32,
    pub viewport_height: u32,
    /// Hard limit on how long a capture may take.
    pub timeout_secs: u32,
    /// Longest edge of the generated thumbnail, in pixels.
    pub thumbnail_max_edge: u32,
}

pub const DEFAULT_VIEWPORT_WIDTH: u32 = 1_280;
pub const DEFAULT_VIEWPORT_HEIGHT: u32 = 800;
pub const DEFAULT_CAPTURE_TIMEOUT_SECS: u32 = 30;
pub const DEFAULT_THUMBNAIL_MAX_EDGE: u32 = 480;

impl CaptureRequest {
    pub fn new(website_id: WebsiteId, url: impl Into<String>) -> Self {
        Self {
            website_id,
            url: url.into(),
            viewport_width: DEFAULT_VIEWPORT_WIDTH,
            viewport_height: DEFAULT_VIEWPORT_HEIGHT,
            timeout_secs: DEFAULT_CAPTURE_TIMEOUT_SECS,
            thumbnail_max_edge: DEFAULT_THUMBNAIL_MAX_EDGE,
        }
    }
}

/// A freshly captured image, before it is written to the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// What a screenshot provider can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenshotCapabilities {
    pub supports_full_page: bool,
    pub supports_custom_viewport: bool,
    pub max_viewport_width: u32,
    pub max_viewport_height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn shot(captured_at: DateTime<Utc>, status: ScreenshotStatus) -> Screenshot {
        Screenshot {
            website_id: WebsiteId::new(),
            provider: ProviderId::new("chromium_cli"),
            path: "abc.png".into(),
            thumbnail_path: Some("abc.thumb.png".into()),
            captured_at,
            status,
            hash: "deadbeef".into(),
            width: 1_280,
            height: 800,
        }
    }

    #[test]
    fn policies_round_trip_and_expose_their_horizon() {
        for policy in ScreenshotRefreshPolicy::ALL {
            assert_eq!(
                ScreenshotRefreshPolicy::parse(policy.as_str()),
                Some(*policy)
            );
        }
        assert_eq!(
            ScreenshotRefreshPolicy::Hourly.max_age(),
            Some(Duration::hours(1))
        );
        assert_eq!(ScreenshotRefreshPolicy::Manual.max_age(), None);
    }

    #[test]
    fn manual_captures_never_go_stale_on_their_own() {
        let old = shot(at(0), ScreenshotStatus::Captured);
        let much_later = at(86_400 * 30);
        assert!(!old.is_stale(ScreenshotRefreshPolicy::Manual, much_later));
        assert!(old.is_stale(ScreenshotRefreshPolicy::Daily, much_later));
    }

    #[test]
    fn staleness_uses_the_policy_horizon() {
        let capture = shot(at(0), ScreenshotStatus::Captured);
        assert!(!capture.is_stale(ScreenshotRefreshPolicy::EverySixHours, at(3_600 * 5)));
        assert!(capture.is_stale(ScreenshotRefreshPolicy::EverySixHours, at(3_600 * 6)));
    }

    #[test]
    fn a_cached_presentation_always_carries_its_age() {
        let capture = shot(at(0), ScreenshotStatus::Captured);
        let presentation = ScreenshotPresentation::from_stored(Some(capture), at(3_600 * 4));
        match presentation {
            ScreenshotPresentation::Cached { age, .. } => {
                assert_eq!(age, Duration::hours(4));
            }
            other => panic!("expected a cached presentation, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_capture_is_not_rendered_as_an_image() {
        let capture = shot(
            at(0),
            ScreenshotStatus::Failed {
                reason: "no browser".into(),
            },
        );
        let presentation = ScreenshotPresentation::from_stored(Some(capture), at(10));
        assert!(!presentation.has_image());
        assert_eq!(
            presentation,
            ScreenshotPresentation::Failed {
                reason: "no browser".into()
            }
        );
    }

    #[test]
    fn an_offline_website_shows_the_offline_state_not_an_old_image() {
        let capture = shot(at(0), ScreenshotStatus::WebsiteOffline);
        let presentation = ScreenshotPresentation::from_stored(Some(capture), at(10));
        assert_eq!(presentation, ScreenshotPresentation::WebsiteOffline);
        assert!(!presentation.has_image());
    }

    #[test]
    fn nothing_stored_means_not_captured() {
        assert_eq!(
            ScreenshotPresentation::from_stored(None, at(0)),
            ScreenshotPresentation::NotCaptured
        );
    }
}
