//! Filesystem storage for captured images.
//!
//! Images are files, not database rows — see `crates/infra-db/src/screenshots.rs` for
//! why. Each website gets a stable pair of filenames, so a refresh replaces its
//! predecessor instead of accumulating.

use async_trait::async_trait;
use std::path::{Path, PathBuf};
use vds_application::screenshots::ScreenshotStore;
use vds_domain::ids::WebsiteId;
use vds_domain::ports::ScreenshotError;
use vds_domain::screenshot::CapturedImage;

/// Writes captures into a cache directory.
#[derive(Debug, Clone)]
pub struct FilesystemScreenshotStore {
    directory: PathBuf,
}

impl FilesystemScreenshotStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Filename for a website's full capture.
    fn full_name(website_id: WebsiteId) -> String {
        format!("{website_id}.png")
    }

    /// Filename for a website's thumbnail.
    fn thumbnail_name(website_id: WebsiteId) -> String {
        format!("{website_id}.thumb.png")
    }

    async fn ensure_directory(&self) -> Result<(), ScreenshotError> {
        tokio::fs::create_dir_all(&self.directory)
            .await
            .map_err(|e| {
                ScreenshotError::Backend(format!(
                    "could not create {}: {e}",
                    self.directory.display()
                ))
            })
    }

    /// Writes a file atomically, so a crash cannot leave a half-written image that the
    /// UI would then try to decode.
    async fn write_atomically(&self, name: &str, bytes: &[u8]) -> Result<(), ScreenshotError> {
        let destination = self.directory.join(name);
        let temporary = self.directory.join(format!("{name}.tmp"));

        tokio::fs::write(&temporary, bytes)
            .await
            .map_err(|e| ScreenshotError::Backend(format!("could not write {name}: {e}")))?;
        tokio::fs::rename(&temporary, &destination)
            .await
            .map_err(|e| ScreenshotError::Backend(format!("could not replace {name}: {e}")))
    }
}

#[async_trait]
impl ScreenshotStore for FilesystemScreenshotStore {
    async fn write(
        &self,
        website_id: WebsiteId,
        image: &CapturedImage,
        thumbnail_max_edge: u32,
    ) -> Result<(String, Option<String>), ScreenshotError> {
        self.ensure_directory().await?;

        let full_name = Self::full_name(website_id);
        self.write_atomically(&full_name, &image.png).await?;

        // A thumbnail that fails to generate is not worth failing the capture over: the
        // full image is still perfectly usable, and mobile simply loads more bytes.
        let thumbnail_name = match crate::image_ops::thumbnail(&image.png, thumbnail_max_edge) {
            Ok(bytes) => {
                let name = Self::thumbnail_name(website_id);
                match self.write_atomically(&name, &bytes).await {
                    Ok(()) => Some(name),
                    Err(err) => {
                        tracing::warn!(error = %err, "could not write the thumbnail");
                        None
                    }
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "could not generate a thumbnail");
                None
            }
        };

        Ok((full_name, thumbnail_name))
    }

    async fn remove(&self, website_id: WebsiteId) -> Result<(), ScreenshotError> {
        for name in [
            Self::full_name(website_id),
            Self::thumbnail_name(website_id),
        ] {
            // A file that is already gone is a success.
            match tokio::fs::remove_file(self.directory.join(&name)).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(ScreenshotError::Backend(format!(
                        "could not remove {name}: {err}"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32) -> CapturedImage {
        let buffer =
            image::RgbImage::from_fn(width, height, |x, _| image::Rgb([(x % 256) as u8, 64, 128]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encodes");
        CapturedImage { png, width, height }
    }

    #[tokio::test]
    async fn a_capture_writes_both_a_full_image_and_a_thumbnail() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FilesystemScreenshotStore::new(dir.path());
        let website = WebsiteId::new();

        let (full, thumbnail) = store
            .write(website, &image(1_280, 800), 480)
            .await
            .expect("writes");

        assert!(dir.path().join(&full).exists());
        let thumbnail = thumbnail.expect("thumbnail generated");
        assert!(dir.path().join(&thumbnail).exists());

        // And the thumbnail really is smaller.
        let full_size = std::fs::metadata(dir.path().join(&full))
            .expect("stat")
            .len();
        let thumb_size = std::fs::metadata(dir.path().join(&thumbnail))
            .expect("stat")
            .len();
        assert!(thumb_size < full_size);
    }

    #[tokio::test]
    async fn the_directory_is_created_if_it_does_not_exist() {
        let dir = tempfile::tempdir().expect("temp dir");
        let nested = dir.path().join("cache").join("screenshots");
        let store = FilesystemScreenshotStore::new(&nested);

        store
            .write(WebsiteId::new(), &image(100, 100), 50)
            .await
            .expect("writes");
        assert!(nested.exists());
    }

    #[tokio::test]
    async fn a_refresh_replaces_the_previous_capture_rather_than_accumulating() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FilesystemScreenshotStore::new(dir.path());
        let website = WebsiteId::new();

        store
            .write(website, &image(400, 300), 200)
            .await
            .expect("writes");
        store
            .write(website, &image(800, 600), 200)
            .await
            .expect("writes");

        let files: Vec<_> = std::fs::read_dir(dir.path())
            .expect("readable")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();

        // Exactly two: the full image and its thumbnail. No temporaries left behind.
        assert_eq!(files.len(), 2, "files were {files:?}");
        assert!(files.iter().all(|f| !f.ends_with(".tmp")));
    }

    #[tokio::test]
    async fn each_website_gets_its_own_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FilesystemScreenshotStore::new(dir.path());

        let (first, _) = store
            .write(WebsiteId::new(), &image(100, 100), 50)
            .await
            .expect("writes");
        let (second, _) = store
            .write(WebsiteId::new(), &image(100, 100), 50)
            .await
            .expect("writes");

        assert_ne!(first, second);
        assert!(dir.path().join(&first).exists());
        assert!(dir.path().join(&second).exists());
    }

    #[tokio::test]
    async fn removing_a_website_deletes_both_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FilesystemScreenshotStore::new(dir.path());
        let website = WebsiteId::new();

        store
            .write(website, &image(400, 300), 200)
            .await
            .expect("writes");
        store.remove(website).await.expect("removes");

        let remaining = std::fs::read_dir(dir.path()).expect("readable").count();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn removing_a_website_with_no_capture_is_harmless() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FilesystemScreenshotStore::new(dir.path());
        assert!(store.remove(WebsiteId::new()).await.is_ok());
    }

    #[tokio::test]
    async fn an_unthumbnailable_image_still_stores_the_full_capture() {
        // The full image is the thing that matters; a missing thumbnail only costs
        // bandwidth on mobile.
        let dir = tempfile::tempdir().expect("temp dir");
        let store = FilesystemScreenshotStore::new(dir.path());

        let broken = CapturedImage {
            png: b"not a real png".to_vec(),
            width: 10,
            height: 10,
        };
        let (full, thumbnail) = store
            .write(WebsiteId::new(), &broken, 100)
            .await
            .expect("writes");

        assert!(dir.path().join(&full).exists());
        assert_eq!(thumbnail, None);
    }
}
