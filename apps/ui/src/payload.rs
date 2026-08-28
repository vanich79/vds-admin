//! The thread boundary between the worker and the window.
//!
//! Slint's `ModelRc` is an `Rc` and its `Image` holds a non-atomic handle, so neither is
//! `Send`: they cannot be built on a worker thread and moved to the UI thread. That is a
//! real constraint of the toolkit, and rather than work around it with locks, this module
//! makes the boundary explicit.
//!
//! Everything that crosses the boundary is a **payload**: plain Rust data, fully
//! formatted, carrying a *filename* where an image will go. The conversion to Slint types
//! happens inside `invoke_from_event_loop`, on the UI thread, at the last possible moment.
//!
//! The upshot is a useful discipline: all the interesting work — querying, formatting,
//! computing chart geometry — happens off the UI thread and is testable without a window,
//! and the UI thread does nothing but allocate view objects.

use crate::chart::ChartGeometry;
use crate::view_model::model;
use crate::{ChartData, ServerDetail, StatCard, WebsiteCard, WebsiteDetail};
use slint::{Image, SharedString};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

thread_local! {
    /// Decoded screenshots, owned by the UI thread.
    ///
    /// A thread-local rather than a shared cache precisely because `Image` is not `Send`:
    /// there is no other thread that could hold one. Decoding a PNG per website per
    /// refresh is what would otherwise make a fifty-site grid stutter.
    static THUMBNAILS: RefCell<ThumbnailCache> = RefCell::new(ThumbnailCache::new());
}

/// Loads a thumbnail through the UI thread's cache.
pub fn load_thumbnail(directory: &Path, name: &str) -> Option<Image> {
    THUMBNAILS.with(|cache| cache.borrow_mut().load(directory, name))
}

/// Drops a cached thumbnail so the next read decodes the new capture.
pub fn invalidate_thumbnail(directory: &Path, name: &str) {
    THUMBNAILS.with(|cache| cache.borrow_mut().invalidate(directory, name));
}

/// Decoded screenshots, keyed by the file they came from.
#[derive(Debug, Default)]
pub struct ThumbnailCache {
    entries: HashMap<PathBuf, Image>,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads a thumbnail, caching the result.
    ///
    /// A file that is missing or cannot be decoded yields `None`, and the card shows its
    /// placeholder instead — a half-written PNG must not take the window down.
    pub fn load(&mut self, directory: &Path, name: &str) -> Option<Image> {
        if name.trim().is_empty() {
            return None;
        }
        let path = directory.join(name);

        if let Some(cached) = self.entries.get(&path) {
            return Some(cached.clone());
        }

        match Image::load_from_path(&path) {
            Ok(image) => {
                self.entries.insert(path, image.clone());
                Some(image)
            }
            Err(error) => {
                tracing::debug!(path = ?path, %error, "could not load a screenshot");
                None
            }
        }
    }

    pub fn invalidate(&mut self, directory: &Path, name: &str) {
        self.entries.remove(&directory.join(name));
    }

    /// How many images are held. Exists so the tests can prove that a second read of
    /// the same file does not decode it again.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A chart, before it becomes view objects.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartPayload {
    pub title: String,
    pub geometry: ChartGeometry,
}

impl ChartPayload {
    pub fn new(title: impl Into<String>, geometry: ChartGeometry) -> Self {
        Self {
            title: title.into(),
            geometry,
        }
    }

    pub fn into_view(self) -> ChartData {
        ChartData {
            title: self.title.into(),
            line: self.geometry.line.into(),
            band: self.geometry.band.into(),
            y_labels: model(
                self.geometry
                    .y_labels
                    .into_iter()
                    .map(SharedString::from)
                    .collect(),
            ),
            x_labels: model(
                self.geometry
                    .x_labels
                    .into_iter()
                    .map(SharedString::from)
                    .collect(),
            ),
            has_data: self.geometry.has_data,
            empty_message: "No data has been collected for this period yet.".into(),
        }
    }
}

/// A website card, before its preview is decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct WebsiteCardPayload {
    pub id: String,
    pub name: String,
    pub url: String,
    pub status: String,
    pub status_label: String,
    pub response: String,
    pub ssl: String,
    pub uptime: String,
    pub visitors: String,
    /// Filename within the screenshot directory, if a capture exists.
    pub thumbnail_file: Option<String>,
    pub preview_message: String,
    pub capture_age: String,
}

impl WebsiteCardPayload {
    pub fn into_view(self, directory: &Path) -> WebsiteCard {
        let thumbnail = self
            .thumbnail_file
            .as_deref()
            .and_then(|name| load_thumbnail(directory, name));

        WebsiteCard {
            id: self.id.into(),
            name: self.name.into(),
            url: self.url.into(),
            status: self.status.into(),
            status_label: self.status_label.into(),
            response: self.response.into(),
            ssl: self.ssl.into(),
            uptime: self.uptime.into(),
            visitors: self.visitors.into(),
            thumbnail: thumbnail.clone().unwrap_or_default(),
            has_thumbnail: thumbnail.is_some(),
            preview_message: self.preview_message.into(),
            capture_age: self.capture_age.into(),
        }
    }
}

/// A server's detail header, before its card list becomes a model.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerDetailPayload {
    pub id: String,
    pub name: String,
    pub host: String,
    pub status: String,
    pub status_label: String,
    pub os: String,
    pub kernel: String,
    pub architecture: String,
    pub cpu_model: String,
    pub cores: String,
    pub uptime: String,
    pub last_check: String,
    pub last_error: String,
    pub cards: Vec<StatCard>,
    pub has_docker: bool,
    pub has_systemd: bool,
}

impl ServerDetailPayload {
    pub fn into_view(self) -> ServerDetail {
        ServerDetail {
            id: self.id.into(),
            name: self.name.into(),
            host: self.host.into(),
            status: self.status.into(),
            status_label: self.status_label.into(),
            os: self.os.into(),
            kernel: self.kernel.into(),
            architecture: self.architecture.into(),
            cpu_model: self.cpu_model.into(),
            cores: self.cores.into(),
            uptime: self.uptime.into(),
            last_check: self.last_check.into(),
            last_error: self.last_error.into(),
            cards: model(self.cards),
            has_docker: self.has_docker,
            has_systemd: self.has_systemd,
        }
    }
}

/// A website's detail header.
#[derive(Debug, Clone, PartialEq)]
pub struct WebsiteDetailPayload {
    pub id: String,
    pub name: String,
    pub url: String,
    pub status: String,
    pub status_label: String,
    pub http_status: String,
    pub response: String,
    pub uptime_24h: String,
    pub ssl_issuer: String,
    pub ssl_expiry: String,
    pub ssl_subject: String,
    pub thumbnail_file: Option<String>,
    pub preview_message: String,
    pub capture_age: String,
    pub cards: Vec<StatCard>,
    pub has_analytics: bool,
    pub analytics_updated: String,
}

impl WebsiteDetailPayload {
    pub fn into_view(self, directory: &Path) -> WebsiteDetail {
        let thumbnail = self
            .thumbnail_file
            .as_deref()
            .and_then(|name| load_thumbnail(directory, name));

        WebsiteDetail {
            id: self.id.into(),
            name: self.name.into(),
            url: self.url.into(),
            status: self.status.into(),
            status_label: self.status_label.into(),
            http_status: self.http_status.into(),
            response: self.response.into(),
            uptime_24h: self.uptime_24h.into(),
            ssl_issuer: self.ssl_issuer.into(),
            ssl_expiry: self.ssl_expiry.into(),
            ssl_subject: self.ssl_subject.into(),
            thumbnail: thumbnail.clone().unwrap_or_default(),
            has_thumbnail: thumbnail.is_some(),
            preview_message: self.preview_message.into(),
            capture_age: self.capture_age.into(),
            cards: model(self.cards),
            has_analytics: self.has_analytics,
            analytics_updated: self.analytics_updated.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a real PNG so the cache has something valid to decode.
    fn write_png(path: &Path) {
        let buffer = image::RgbImage::from_fn(8, 8, |x, y| {
            image::Rgb([(x * 30) as u8, (y * 30) as u8, 200])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encodes");
        std::fs::write(path, bytes).expect("written");
    }

    #[test]
    fn every_payload_can_cross_a_thread_boundary() {
        // The whole reason this module exists. If a `ModelRc` or an `Image` ever leaks
        // into one of these structs, this stops compiling.
        fn assert_send<T: Send>() {}

        assert_send::<ChartPayload>();
        assert_send::<WebsiteCardPayload>();
        assert_send::<ServerDetailPayload>();
        assert_send::<WebsiteDetailPayload>();
        assert_send::<Vec<StatCard>>();
        assert_send::<Vec<crate::ServerRow>>();
        assert_send::<Vec<crate::EventRow>>();
        assert_send::<Vec<crate::AlertRow>>();
    }

    #[test]
    fn a_thumbnail_is_decoded_once_and_then_served_from_the_cache() {
        // Decoding a PNG per site per refresh is what makes a fifty-site grid stutter.
        let dir = tempfile::tempdir().expect("temp dir");
        write_png(&dir.path().join("site.png"));

        let mut cache = ThumbnailCache::new();
        assert!(cache.is_empty());

        assert!(cache.load(dir.path(), "site.png").is_some());
        assert_eq!(cache.len(), 1);
        assert!(cache.load(dir.path(), "site.png").is_some());
        assert_eq!(cache.len(), 1, "the second read must not decode again");
    }

    #[test]
    fn a_thumbnail_that_cannot_be_decoded_yields_none_rather_than_failing() {
        // A half-written PNG must not take the window down.
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("broken.png"), b"not a png").expect("written");

        let mut cache = ThumbnailCache::new();
        assert!(cache.load(dir.path(), "broken.png").is_none());
        assert!(cache.is_empty(), "a failed decode must not be cached");
    }

    #[test]
    fn a_missing_file_yields_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cache = ThumbnailCache::new();
        assert!(cache.load(dir.path(), "never-captured.png").is_none());
    }

    #[test]
    fn an_empty_name_is_treated_as_no_image_without_touching_the_disk() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut cache = ThumbnailCache::new();
        assert!(cache.load(dir.path(), "").is_none());
        assert!(cache.load(dir.path(), "   ").is_none());
    }

    #[test]
    fn invalidating_forces_the_next_read_to_decode_again() {
        // A refreshed capture reuses the filename, so without this the window would keep
        // showing the old image forever.
        let dir = tempfile::tempdir().expect("temp dir");
        write_png(&dir.path().join("site.png"));

        let mut cache = ThumbnailCache::new();
        cache.load(dir.path(), "site.png");
        assert_eq!(cache.len(), 1);

        cache.invalidate(dir.path(), "site.png");
        assert!(cache.is_empty());
        assert!(cache.load(dir.path(), "site.png").is_some());
    }

    #[test]
    fn a_card_without_a_capture_reports_no_thumbnail() {
        let dir = tempfile::tempdir().expect("temp dir");
        let payload = WebsiteCardPayload {
            id: "id".into(),
            name: "Example".into(),
            url: "https://example.com/".into(),
            status: "healthy".into(),
            status_label: "Online".into(),
            response: "Response: 142 ms".into(),
            ssl: "SSL: 48 days".into(),
            uptime: "Uptime 24h: 99.98%".into(),
            visitors: String::new(),
            thumbnail_file: None,
            preview_message: "No screenshot yet".into(),
            capture_age: String::new(),
        };

        let card = payload.into_view(dir.path());
        assert!(!card.has_thumbnail);
        assert_eq!(card.preview_message, "No screenshot yet");
    }

    #[test]
    fn a_chart_payload_becomes_a_chart_with_its_labels() {
        let payload = ChartPayload::new(
            "CPU — 24 hours",
            ChartGeometry {
                line: "M 0 0 L 10 10".into(),
                band: String::new(),
                y_labels: vec!["100%".into(), "0%".into()],
                x_labels: vec!["12:00".into()],
                max_value: 100.0,
                has_data: true,
            },
        );

        let chart = payload.into_view();
        assert_eq!(chart.title, "CPU — 24 hours");
        assert_eq!(chart.line, "M 0 0 L 10 10");
        assert!(chart.has_data);
        assert_eq!(slint::Model::row_count(&chart.y_labels), 2);
    }

    #[test]
    fn an_empty_chart_carries_an_explanation_rather_than_being_blank() {
        let chart = ChartPayload::new("CPU", ChartGeometry::default()).into_view();
        assert!(!chart.has_data);
        assert!(!chart.empty_message.is_empty());
    }
}
