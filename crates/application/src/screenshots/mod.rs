//! Website screenshot capture, caching and presentation.
//!
//! The honesty rules from `docs/adr/004-screenshot-architecture.md` are enforced here:
//! a cached image always travels with its age, an offline site is never papered over
//! with an old capture, and a machine with no browser reports the feature as
//! *unavailable* rather than accumulating failures.

use crate::config::ScreenshotSettings;
use crate::scheduler::JobOutcome;
use std::sync::Arc;
use vds_domain::Status;
use vds_domain::events::DomainEvent;
use vds_domain::ids::WebsiteId;
use vds_domain::ports::{
    Clock, EventPublisher, ScreenshotError, ScreenshotProvider, ScreenshotRepository,
    WebsiteRepository,
};
use vds_domain::screenshot::{
    CaptureRequest, CapturedImage, Screenshot, ScreenshotPresentation, ScreenshotRefreshPolicy,
    ScreenshotStatus,
};

/// Persists captured image bytes.
///
/// A port because the desktop writes to a cache directory while a future mobile build
/// may want somewhere else entirely — and because tests must not touch the filesystem.
#[async_trait::async_trait]
pub trait ScreenshotStore: Send + Sync {
    /// Writes a capture and its thumbnail, returning `(path, thumbnail_path)`.
    async fn write(
        &self,
        website_id: WebsiteId,
        image: &CapturedImage,
        thumbnail_max_edge: u32,
    ) -> Result<(String, Option<String>), ScreenshotError>;

    /// Removes a website's stored images.
    async fn remove(&self, website_id: WebsiteId) -> Result<(), ScreenshotError>;
}

/// Captures and serves website previews.
pub struct ScreenshotService {
    provider: Arc<dyn ScreenshotProvider>,
    store: Arc<dyn ScreenshotStore>,
    screenshots: Arc<dyn ScreenshotRepository>,
    websites: Arc<dyn WebsiteRepository>,
    events: Arc<dyn EventPublisher>,
    clock: Arc<dyn Clock>,
    settings: ScreenshotSettings,
}

impl ScreenshotService {
    pub fn new(
        provider: Arc<dyn ScreenshotProvider>,
        store: Arc<dyn ScreenshotStore>,
        screenshots: Arc<dyn ScreenshotRepository>,
        websites: Arc<dyn WebsiteRepository>,
        events: Arc<dyn EventPublisher>,
        clock: Arc<dyn Clock>,
        settings: ScreenshotSettings,
    ) -> Self {
        Self {
            provider,
            store,
            screenshots,
            websites,
            events,
            clock,
            settings,
        }
    }

    pub fn settings(&self) -> &ScreenshotSettings {
        &self.settings
    }

    /// What the UI should render for a website.
    pub async fn presentation(&self, website_id: WebsiteId) -> ScreenshotPresentation {
        if !self.settings.enabled {
            return ScreenshotPresentation::Unavailable;
        }
        let stored = self.screenshots.get(website_id).await.ok().flatten();
        ScreenshotPresentation::from_stored(stored, self.clock.now())
    }

    /// Whether a capture is due under the configured policy.
    ///
    /// Opening the app never triggers a capture; only this predicate and an explicit
    /// user request do.
    pub async fn is_due(&self, website_id: WebsiteId) -> bool {
        if !self.settings.enabled {
            return false;
        }
        match self.screenshots.get(website_id).await.ok().flatten() {
            None => self.settings.refresh_policy != ScreenshotRefreshPolicy::Manual,
            Some(existing) => existing.is_stale(self.settings.refresh_policy, self.clock.now()),
        }
    }

    /// Captures a website now, regardless of policy.
    pub async fn capture(&self, website_id: WebsiteId) -> JobOutcome {
        if !self.settings.enabled {
            return JobOutcome::Skipped;
        }

        let website = match self.websites.get(website_id).await {
            Ok(website) => website,
            Err(_) => return JobOutcome::Skipped,
        };

        if !self.provider.is_available().await {
            // No browser on this machine. Record it once so the UI can hide previews,
            // and stop — retrying would fail identically forever.
            self.record(
                website_id,
                ScreenshotStatus::Unavailable,
                String::new(),
                None,
                String::new(),
                0,
                0,
            )
            .await;
            return JobOutcome::Permanent("no screenshot backend available".into());
        }

        // A site that is down has nothing worth photographing, and the resulting error
        // page would be misleading next to a green status badge.
        let state = self.websites.load_state(website_id).await.ok();
        if state.is_some_and(|s| s.status == Status::Offline) {
            self.record(
                website_id,
                ScreenshotStatus::WebsiteOffline,
                String::new(),
                None,
                String::new(),
                0,
                0,
            )
            .await;
            self.events.publish(DomainEvent::ScreenshotFailed {
                website_id,
                error: "website is offline".into(),
            });
            return JobOutcome::Skipped;
        }

        let request = CaptureRequest {
            website_id,
            url: website.url.clone(),
            viewport_width: self.settings.viewport_width,
            viewport_height: self.settings.viewport_height,
            timeout_secs: self.settings.timeout_secs,
            thumbnail_max_edge: self.settings.thumbnail_max_edge,
        };

        match self.provider.capture(&request).await {
            Ok(image) => self.store_capture(website_id, image).await,
            Err(err) => {
                self.record(
                    website_id,
                    ScreenshotStatus::Failed {
                        reason: err.to_string(),
                    },
                    String::new(),
                    None,
                    String::new(),
                    0,
                    0,
                )
                .await;
                self.events.publish(DomainEvent::ScreenshotFailed {
                    website_id,
                    error: err.to_string(),
                });

                if err.is_retryable() {
                    JobOutcome::Retry(err.to_string())
                } else {
                    JobOutcome::Permanent(err.to_string())
                }
            }
        }
    }

    /// Captures only if the policy says a refresh is due.
    pub async fn capture_if_due(&self, website_id: WebsiteId) -> JobOutcome {
        if self.is_due(website_id).await {
            self.capture(website_id).await
        } else {
            JobOutcome::Skipped
        }
    }

    async fn store_capture(&self, website_id: WebsiteId, image: CapturedImage) -> JobOutcome {
        let hash = content_hash(&image.png);

        // An unchanged page need not be rewritten, but its capture time must still be
        // updated — otherwise the policy would consider it stale forever and recapture
        // on every cycle.
        if let Ok(Some(existing)) = self.screenshots.get(website_id).await
            && existing.hash == hash
            && existing.status.is_captured()
        {
            let mut refreshed = existing;
            refreshed.captured_at = self.clock.now();
            let _ = self.screenshots.save(&refreshed).await;
            return JobOutcome::Success;
        }

        let (path, thumbnail_path) = match self
            .store
            .write(website_id, &image, self.settings.thumbnail_max_edge)
            .await
        {
            Ok(paths) => paths,
            Err(err) => {
                self.events.publish(DomainEvent::ScreenshotFailed {
                    website_id,
                    error: err.to_string(),
                });
                return JobOutcome::Retry(err.to_string());
            }
        };

        self.record(
            website_id,
            ScreenshotStatus::Captured,
            path,
            thumbnail_path,
            hash,
            image.width,
            image.height,
        )
        .await;

        self.events
            .publish(DomainEvent::ScreenshotUpdated { website_id });
        JobOutcome::Success
    }

    #[allow(clippy::too_many_arguments)]
    async fn record(
        &self,
        website_id: WebsiteId,
        status: ScreenshotStatus,
        path: String,
        thumbnail_path: Option<String>,
        hash: String,
        width: u32,
        height: u32,
    ) {
        let screenshot = Screenshot {
            website_id,
            provider: self.provider.id(),
            path,
            thumbnail_path,
            captured_at: self.clock.now(),
            status,
            hash,
            width,
            height,
        };
        if let Err(err) = self.screenshots.save(&screenshot).await {
            tracing::warn!(website = %website_id, error = %err, "could not record screenshot");
        }
    }

    /// Removes a website's screenshot, e.g. when the website is deleted.
    pub async fn forget(&self, website_id: WebsiteId) {
        let _ = self.store.remove(website_id).await;
        let _ = self.screenshots.delete(website_id).await;
    }
}

/// Stable content hash of an image, used to skip rewriting unchanged captures.
///
/// FNV-1a rather than a cryptographic hash: this only needs to detect change, and it
/// runs on every capture on devices where CPU matters.
fn content_hash(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

/// How long ago a capture was taken, in words.
pub fn describe_age(age: chrono::Duration) -> String {
    let minutes = age.num_minutes();
    if minutes < 1 {
        "just now".to_owned()
    } else if minutes < 60 {
        format!("{minutes} minutes ago")
    } else if age.num_hours() < 24 {
        let hours = age.num_hours();
        format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" })
    } else {
        let days = age.num_days();
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeScreenshotRepository, FakeWebsiteRepository};
    use chrono::Duration;
    use chrono::{DateTime, Utc};
    use parking_lot::Mutex;
    use vds_domain::ids::ProviderId;
    use vds_domain::ports::{FixedClock, RecordingEventPublisher};
    use vds_domain::screenshot::ScreenshotCapabilities;
    use vds_domain::website::{Website, WebsiteRuntimeState};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    /// A provider that returns scripted captures.
    struct StubProvider {
        result: Mutex<Result<CapturedImage, ScreenshotError>>,
        available: Mutex<bool>,
        captures: Mutex<u32>,
    }

    impl StubProvider {
        fn returning(png: Vec<u8>) -> Self {
            Self {
                result: Mutex::new(Ok(CapturedImage {
                    png,
                    width: 1_280,
                    height: 800,
                })),
                available: Mutex::new(true),
                captures: Mutex::new(0),
            }
        }

        fn failing(err: ScreenshotError) -> Self {
            Self {
                result: Mutex::new(Err(err)),
                available: Mutex::new(true),
                captures: Mutex::new(0),
            }
        }

        fn captures(&self) -> u32 {
            *self.captures.lock()
        }
    }

    #[async_trait::async_trait]
    impl ScreenshotProvider for StubProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new("stub")
        }

        fn display_name(&self) -> &'static str {
            "Stub"
        }

        fn capabilities(&self) -> ScreenshotCapabilities {
            ScreenshotCapabilities {
                supports_full_page: false,
                supports_custom_viewport: true,
                max_viewport_width: 1_920,
                max_viewport_height: 1_080,
            }
        }

        async fn is_available(&self) -> bool {
            *self.available.lock()
        }

        async fn capture(
            &self,
            _request: &CaptureRequest,
        ) -> Result<CapturedImage, ScreenshotError> {
            *self.captures.lock() += 1;
            self.result.lock().clone()
        }
    }

    /// A store that records what it was asked to write.
    #[derive(Default)]
    struct MemoryStore {
        writes: Mutex<u32>,
        fail: Mutex<bool>,
    }

    impl MemoryStore {
        fn writes(&self) -> u32 {
            *self.writes.lock()
        }
    }

    #[async_trait::async_trait]
    impl ScreenshotStore for MemoryStore {
        async fn write(
            &self,
            website_id: WebsiteId,
            _image: &CapturedImage,
            _thumbnail_max_edge: u32,
        ) -> Result<(String, Option<String>), ScreenshotError> {
            if *self.fail.lock() {
                return Err(ScreenshotError::Backend("disk full".into()));
            }
            *self.writes.lock() += 1;
            Ok((
                format!("{website_id}.png"),
                Some(format!("{website_id}.thumb.png")),
            ))
        }

        async fn remove(&self, _website_id: WebsiteId) -> Result<(), ScreenshotError> {
            Ok(())
        }
    }

    struct Harness {
        service: ScreenshotService,
        screenshots: Arc<FakeScreenshotRepository>,
        websites: Arc<FakeWebsiteRepository>,
        store: Arc<MemoryStore>,
        provider: Arc<StubProvider>,
        events: Arc<RecordingEventPublisher>,
        clock: FixedClock,
        website: Website,
    }

    fn harness_with(provider: Arc<StubProvider>, settings: ScreenshotSettings) -> Harness {
        let website = Website::new("Example", "https://example.com/", at(0));
        let websites = Arc::new(FakeWebsiteRepository::new());
        websites.insert(website.clone());

        let screenshots = Arc::new(FakeScreenshotRepository::new());
        let store = Arc::new(MemoryStore::default());
        let events = Arc::new(RecordingEventPublisher::new());
        let clock = FixedClock::new(at(1_000));

        let service = ScreenshotService::new(
            Arc::clone(&provider) as Arc<dyn ScreenshotProvider>,
            Arc::clone(&store) as Arc<dyn ScreenshotStore>,
            Arc::clone(&screenshots) as Arc<dyn ScreenshotRepository>,
            Arc::clone(&websites) as Arc<dyn WebsiteRepository>,
            Arc::clone(&events) as Arc<dyn EventPublisher>,
            Arc::new(clock.clone()),
            settings,
        );

        Harness {
            service,
            screenshots,
            websites,
            store,
            provider,
            events,
            clock,
            website,
        }
    }

    fn harness() -> Harness {
        harness_with(
            Arc::new(StubProvider::returning(b"fake png bytes".to_vec())),
            ScreenshotSettings::default(),
        )
    }

    #[tokio::test]
    async fn a_capture_is_stored_and_announced() {
        let h = harness();
        assert_eq!(h.service.capture(h.website.id).await, JobOutcome::Success);

        let stored = h
            .screenshots
            .get(h.website.id)
            .await
            .expect("readable")
            .expect("stored");
        assert_eq!(stored.status, ScreenshotStatus::Captured);
        assert_eq!(stored.width, 1_280);
        assert!(stored.thumbnail_path.is_some());
        assert!(h.events.contains(|e| e.kind() == "screenshot_updated"));
    }

    #[tokio::test]
    async fn a_cached_capture_is_presented_with_its_age() {
        // The rule from the brief: never show a stale image without saying how stale.
        let h = harness();
        h.service.capture(h.website.id).await;
        h.clock.set(at(1_000 + 4 * 3_600));

        match h.service.presentation(h.website.id).await {
            ScreenshotPresentation::Cached { age, .. } => {
                assert_eq!(age, Duration::hours(4));
                assert_eq!(describe_age(age), "4 hours ago");
            }
            other => panic!("expected a cached presentation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_offline_website_is_not_papered_over_with_an_old_image() {
        let h = harness();
        h.service.capture(h.website.id).await;

        let mut state = WebsiteRuntimeState::unknown(h.website.id);
        state.status = Status::Offline;
        h.websites.save_state(&state).await.expect("saved");

        assert_eq!(h.service.capture(h.website.id).await, JobOutcome::Skipped);
        assert_eq!(
            h.service.presentation(h.website.id).await,
            ScreenshotPresentation::WebsiteOffline
        );
    }

    #[tokio::test]
    async fn a_machine_without_a_browser_reports_unavailable_and_stops_trying() {
        let provider = Arc::new(StubProvider::returning(Vec::new()));
        *provider.available.lock() = false;
        let h = harness_with(provider, ScreenshotSettings::default());

        let outcome = h.service.capture(h.website.id).await;
        assert!(
            matches!(outcome, JobOutcome::Permanent(_)),
            "got {outcome:?}"
        );
        assert_eq!(h.provider.captures(), 0);
        assert_eq!(
            h.service.presentation(h.website.id).await,
            ScreenshotPresentation::Unavailable
        );
    }

    #[tokio::test]
    async fn a_capture_failure_offers_a_retry_rather_than_showing_a_stale_image() {
        let h = harness_with(
            Arc::new(StubProvider::failing(ScreenshotError::Timeout {
                seconds: 30,
            })),
            ScreenshotSettings::default(),
        );

        let outcome = h.service.capture(h.website.id).await;
        assert!(matches!(outcome, JobOutcome::Retry(_)));

        match h.service.presentation(h.website.id).await {
            ScreenshotPresentation::Failed { reason } => {
                assert!(reason.contains("timed out"), "reason was {reason}");
            }
            other => panic!("expected a failed presentation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn opening_the_app_does_not_trigger_a_capture() {
        // `presentation` is the read path and must never do work.
        let h = harness();
        h.service.presentation(h.website.id).await;
        assert_eq!(h.provider.captures(), 0);
    }

    #[tokio::test]
    async fn a_fresh_capture_is_not_due_again_until_the_policy_says_so() {
        let h = harness_with(
            Arc::new(StubProvider::returning(b"png".to_vec())),
            ScreenshotSettings {
                refresh_policy: ScreenshotRefreshPolicy::EverySixHours,
                ..Default::default()
            },
        );

        assert!(h.service.is_due(h.website.id).await, "nothing captured yet");
        h.service.capture(h.website.id).await;
        assert!(!h.service.is_due(h.website.id).await);

        h.clock.set(at(1_000 + 5 * 3_600));
        assert!(!h.service.is_due(h.website.id).await);

        h.clock.set(at(1_000 + 6 * 3_600));
        assert!(h.service.is_due(h.website.id).await);
    }

    #[tokio::test]
    async fn the_manual_policy_never_schedules_a_capture() {
        let h = harness_with(
            Arc::new(StubProvider::returning(b"png".to_vec())),
            ScreenshotSettings {
                refresh_policy: ScreenshotRefreshPolicy::Manual,
                ..Default::default()
            },
        );

        assert!(!h.service.is_due(h.website.id).await);
        assert_eq!(
            h.service.capture_if_due(h.website.id).await,
            JobOutcome::Skipped
        );
        // But an explicit request still works.
        assert_eq!(h.service.capture(h.website.id).await, JobOutcome::Success);
    }

    #[tokio::test]
    async fn an_unchanged_page_is_not_rewritten_but_its_timestamp_moves_on() {
        // Otherwise a static page would be recaptured on every single cycle forever.
        let h = harness();
        h.service.capture(h.website.id).await;
        assert_eq!(h.store.writes(), 1);

        h.clock.set(at(1_000 + 7 * 3_600));
        h.service.capture(h.website.id).await;

        assert_eq!(h.store.writes(), 1, "identical bytes must not be rewritten");
        let stored = h
            .screenshots
            .get(h.website.id)
            .await
            .expect("readable")
            .expect("stored");
        assert_eq!(stored.captured_at, at(1_000 + 7 * 3_600));
        assert!(!h.service.is_due(h.website.id).await);
    }

    #[tokio::test]
    async fn a_changed_page_is_rewritten() {
        let provider = Arc::new(StubProvider::returning(b"version one".to_vec()));
        let h = harness_with(Arc::clone(&provider), ScreenshotSettings::default());

        h.service.capture(h.website.id).await;
        *provider.result.lock() = Ok(CapturedImage {
            png: b"version two".to_vec(),
            width: 1_280,
            height: 800,
        });
        h.service.capture(h.website.id).await;

        assert_eq!(h.store.writes(), 2);
    }

    #[tokio::test]
    async fn a_storage_failure_is_retried_and_does_not_claim_success() {
        let h = harness();
        *h.store.fail.lock() = true;

        let outcome = h.service.capture(h.website.id).await;
        assert!(matches!(outcome, JobOutcome::Retry(_)));
        assert!(!h.events.contains(|e| e.kind() == "screenshot_updated"));
    }

    #[tokio::test]
    async fn disabling_screenshots_turns_the_whole_feature_off() {
        let h = harness_with(
            Arc::new(StubProvider::returning(b"png".to_vec())),
            ScreenshotSettings {
                enabled: false,
                ..Default::default()
            },
        );

        assert_eq!(h.service.capture(h.website.id).await, JobOutcome::Skipped);
        assert!(!h.service.is_due(h.website.id).await);
        assert_eq!(
            h.service.presentation(h.website.id).await,
            ScreenshotPresentation::Unavailable
        );
    }

    #[tokio::test]
    async fn a_website_with_no_capture_yet_presents_as_not_captured() {
        let h = harness();
        assert_eq!(
            h.service.presentation(h.website.id).await,
            ScreenshotPresentation::NotCaptured
        );
    }

    #[test]
    fn ages_are_described_in_human_terms() {
        assert_eq!(describe_age(Duration::seconds(30)), "just now");
        assert_eq!(describe_age(Duration::minutes(45)), "45 minutes ago");
        assert_eq!(describe_age(Duration::hours(1)), "1 hour ago");
        assert_eq!(describe_age(Duration::hours(4)), "4 hours ago");
        assert_eq!(describe_age(Duration::days(1)), "1 day ago");
        assert_eq!(describe_age(Duration::days(9)), "9 days ago");
    }

    #[test]
    fn the_content_hash_distinguishes_different_images() {
        assert_eq!(content_hash(b"abc"), content_hash(b"abc"));
        assert_ne!(content_hash(b"abc"), content_hash(b"abd"));
        assert_eq!(content_hash(b"").len(), 16);
    }
}
