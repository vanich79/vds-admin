//! A provider that draws a placeholder instead of loading a page. Development only.
//!
//! # Why this is fenced off
//!
//! A screenshot is a claim about what a website looked like at a moment in time. A
//! generated placeholder that reached a production screen would be a false one, and the
//! user would have no way to tell. So this module sits behind the `demo-providers` Cargo
//! feature, which is off by default and is never enabled for a release build — see
//! `docs/adr/004-screenshot-architecture.md`.
//!
//! The image it produces is deliberately unlike a real page: flat colour bands and no
//! text, so that nobody mistakes one for a capture even in a screenshot of the app.
//!
//! It exists so that the website grid, the thumbnail cache, the refresh policy and the
//! "captured N hours ago" wording can all be exercised on a machine with no browser
//! installed.

use async_trait::async_trait;
use std::io::Cursor;
use vds_domain::ids::ProviderId;
use vds_domain::ports::{ScreenshotError, ScreenshotProvider};
use vds_domain::screenshot::{CaptureRequest, CapturedImage, ScreenshotCapabilities};

/// The provider's stable identifier.
pub const PROVIDER_ID: &str = "demo";

/// Draws a placeholder image. Development builds only.
#[derive(Debug, Default, Clone, Copy)]
pub struct DemoScreenshotProvider;

impl DemoScreenshotProvider {
    pub fn new() -> Self {
        Self
    }
}

/// A colour derived from the URL, so each site's placeholder is distinguishable.
fn hue_for(url: &str) -> [u8; 3] {
    let mut hash: u32 = 2_166_136_261;
    for byte in url.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    // Kept in the upper half of the range so the bands stay light enough for the darker
    // stripe below to read as a contrast rather than as a black box.
    [
        128 + (hash & 0x3f) as u8,
        128 + ((hash >> 8) & 0x3f) as u8,
        128 + ((hash >> 16) & 0x3f) as u8,
    ]
}

#[async_trait]
impl ScreenshotProvider for DemoScreenshotProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(PROVIDER_ID)
    }

    fn display_name(&self) -> &'static str {
        // Says so out loud: if this ever reaches a picker, it must be unmistakable.
        "Demo (generated placeholder)"
    }

    fn capabilities(&self) -> ScreenshotCapabilities {
        ScreenshotCapabilities {
            supports_full_page: false,
            supports_custom_viewport: true,
            max_viewport_width: 3_840,
            max_viewport_height: 2_160,
        }
    }

    async fn is_available(&self) -> bool {
        // Always: needing no browser is the entire point.
        true
    }

    async fn capture(&self, request: &CaptureRequest) -> Result<CapturedImage, ScreenshotError> {
        let width = request.viewport_width.clamp(1, 3_840);
        let height = request.viewport_height.clamp(1, 2_160);
        let [r, g, b] = hue_for(&request.url);

        let buffer = image::RgbImage::from_fn(width, height, |_, y| {
            // A header band and a body, which is enough structure to tell thumbnails
            // apart at a glance without resembling an actual page.
            let header = y < height / 6;
            if header {
                image::Rgb([r / 2, g / 2, b / 2])
            } else {
                image::Rgb([r, g, b])
            }
        });

        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|err| ScreenshotError::InvalidImage(err.to_string()))?;

        Ok(CapturedImage { png, width, height })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vds_domain::ids::WebsiteId;

    fn request(url: &str) -> CaptureRequest {
        CaptureRequest::new(WebsiteId::new(), url)
    }

    #[tokio::test]
    async fn a_capture_produces_a_decodable_png_of_the_requested_size() {
        let provider = DemoScreenshotProvider::new();
        let mut req = request("https://example.com/");
        req.viewport_width = 320;
        req.viewport_height = 200;

        let captured = provider.capture(&req).await.expect("an image");
        assert_eq!(captured.width, 320);
        assert_eq!(captured.height, 200);

        let decoded = image::load_from_memory(&captured.png).expect("valid png");
        assert_eq!(decoded.width(), 320);
    }

    #[tokio::test]
    async fn two_sites_get_visibly_different_placeholders() {
        // Identical placeholders would make a grid of demo sites look like a caching bug.
        let provider = DemoScreenshotProvider::new();
        let one = provider
            .capture(&request("https://one.example/"))
            .await
            .expect("an image");
        let two = provider
            .capture(&request("https://two.example/"))
            .await
            .expect("an image");
        assert_ne!(one.png, two.png);
    }

    #[tokio::test]
    async fn the_same_url_always_gets_the_same_placeholder() {
        let provider = DemoScreenshotProvider::new();
        let first = provider
            .capture(&request("https://example.com/"))
            .await
            .expect("an image");
        let second = provider
            .capture(&request("https://example.com/"))
            .await
            .expect("an image");
        assert_eq!(first.png, second.png);
    }

    #[tokio::test]
    async fn a_zero_sized_viewport_is_clamped_rather_than_failing() {
        // `image` panics on a zero dimension, and a configuration slip must not take the
        // capture scheduler down with it.
        let provider = DemoScreenshotProvider::new();
        let mut req = request("https://example.com/");
        req.viewport_width = 0;
        req.viewport_height = 0;

        let captured = provider.capture(&req).await.expect("an image");
        assert_eq!(captured.width, 1);
        assert_eq!(captured.height, 1);
    }

    #[tokio::test]
    async fn an_absurd_viewport_is_clamped_to_the_advertised_maximum() {
        let provider = DemoScreenshotProvider::new();
        let mut req = request("https://example.com/");
        req.viewport_width = 100_000;
        req.viewport_height = 100_000;

        let captured = provider.capture(&req).await.expect("an image");
        assert_eq!(captured.width, provider.capabilities().max_viewport_width);
        assert_eq!(captured.height, provider.capabilities().max_viewport_height);
    }

    #[tokio::test]
    async fn it_names_itself_as_generated_and_needs_no_browser() {
        let provider = DemoScreenshotProvider::new();
        assert!(provider.display_name().to_lowercase().contains("demo"));
        assert!(provider.is_available().await);
    }
}
