//! Deciding what a file *is*, from the file rather than from its name.
//!
//! An extension is a claim, not a fact. `logo.png` may be a JPEG someone renamed, and a
//! `.bak` may be a perfectly readable configuration file. A preview that trusted the name
//! would show the wrong thing in both directions, so the decision is made from the bytes
//! that actually arrived.
//!
//! The extension still does one job: it picks how many bytes to ask for. An image needs a
//! larger budget than a configuration file, and asking for the larger one every time
//! would pull eight megabytes of a log across a link that may be a phone's.

use vds_domain::ports::{FileBytes, FileContents};

/// How much of a text file is read. See [`vds_domain::ports::DEFAULT_MAX_READ_BYTES`].
pub use vds_domain::ports::DEFAULT_MAX_READ_BYTES as MAX_TEXT_BYTES;

/// How much of an image is read.
///
/// Comfortably more than a photograph from a phone, and far less than a video someone
/// left in the uploads folder. An image larger than this is reported by size rather than
/// shown: pulling it would take long enough that the application would look frozen.
pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// What was found at a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// Text, editable.
    Text(FileContents),
    /// An image, with the bytes to decode and the format they are in.
    Image(ImageFile),
    /// Neither. Reported by size and kind, because "this is a 4 MB ELF binary" is a
    /// useful answer and a window full of mojibake is not.
    Binary { size_bytes: u64 },
}

impl Preview {
    /// A stable code for the interface to switch on.
    pub fn kind(&self) -> &'static str {
        match self {
            Preview::Text(_) => "text",
            Preview::Image(_) => "image",
            Preview::Binary { .. } => "binary",
        }
    }

    /// The file's full size on disk, whatever it turned out to be.
    pub fn size_bytes(&self) -> u64 {
        match self {
            Preview::Text(contents) => contents.size_bytes,
            Preview::Image(image) => image.size_bytes,
            Preview::Binary { size_bytes } => *size_bytes,
        }
    }
}

/// An image, still encoded.
///
/// Decoding happens in the presentation layer, where the toolkit's image type lives and
/// where a malformed file can fail without taking anything else with it.
#[derive(Clone, PartialEq, Eq)]
pub struct ImageFile {
    pub bytes: Vec<u8>,
    /// `png`, `jpeg`, `gif`, `bmp`, `webp` or `ico` — and only ever a format the
    /// presentation layer's decoder can actually read. `apps/ui` has a test that holds
    /// the two lists together, because recognising a format nobody can decode turns a
    /// clear "not a text file" into a broken picture.
    pub format: &'static str,
    pub size_bytes: u64,
    /// True when the file was larger than the budget, so these bytes are only its
    /// beginning and will not decode. Reported rather than attempted.
    pub truncated: bool,
}

impl std::fmt::Debug for ImageFile {
    /// Without this, one debug line is eight megabytes of numbers.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImageFile")
            .field("format", &self.format)
            .field("size_bytes", &self.size_bytes)
            .field("truncated", &self.truncated)
            .field("bytes", &format_args!("{} read", self.bytes.len()))
            .finish()
    }
}

/// Extensions that justify the larger read budget.
///
/// Only a budget. What the file turns out to be is decided by [`image_format`] from the
/// bytes themselves, so a `.png` that is really a zip archive is reported as binary.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico"];

/// How many bytes to ask for, given only the path.
pub fn read_budget(path: &str) -> u64 {
    match extension(path) {
        Some(extension) if IMAGE_EXTENSIONS.contains(&extension.as_str()) => MAX_IMAGE_BYTES,
        _ => MAX_TEXT_BYTES,
    }
}

/// The lower-cased extension, if there is one.
fn extension(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    // A leading dot is the whole name of a dotfile, not a separator: `.env` has no
    // extension, and treating `env` as one would be wrong in a way that matters here.
    let (_, extension) = name.trim_start_matches('.').rsplit_once('.')?;
    Some(extension.to_ascii_lowercase())
}

/// Identifies an image from its leading bytes, or `None`.
///
/// These signatures are fixed by their formats and have not changed in decades. Checking
/// them here rather than asking a decoder means an eight-megabyte file is rejected in
/// nanoseconds instead of being handed to a parser.
pub fn image_format(bytes: &[u8]) -> Option<&'static str> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    if bytes.starts_with(PNG) {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.starts_with(b"BM") {
        Some("bmp")
    } else if bytes.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        Some("ico")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        // A RIFF container holds several formats; only the WebP one is an image.
        Some("webp")
    } else {
        None
    }
}

/// Classifies bytes that have already arrived.
///
/// One fetch decides everything: whether this is a picture, prose, or neither. No second
/// round trip, and no guess from the filename.
pub fn classify(raw: FileBytes, path: &str) -> Preview {
    if let Some(format) = image_format(&raw.bytes) {
        return Preview::Image(ImageFile {
            format,
            size_bytes: raw.size_bytes,
            truncated: raw.truncated,
            bytes: raw.bytes,
        });
    }

    let size_bytes = raw.size_bytes;
    match raw.into_text(path) {
        Ok(contents) => Preview::Text(contents),
        Err(_) => Preview::Binary { size_bytes },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(bytes: &[u8]) -> FileBytes {
        FileBytes {
            bytes: bytes.to_vec(),
            truncated: false,
            size_bytes: bytes.len() as u64,
        }
    }

    #[test]
    fn a_picture_is_recognised_by_its_bytes_not_its_name() {
        // The whole point: `notes.txt` holding a PNG previews as a picture, and a zip
        // named `logo.png` does not.
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        assert_eq!(classify(raw(&png), "/tmp/notes.txt").kind(), "image");

        let zip = b"PK\x03\x04\x14\x00\x00\x00\x08\x00";
        assert_eq!(classify(raw(zip), "/var/www/logo.png").kind(), "binary");
    }

    #[test]
    fn every_format_the_decoder_handles_is_recognised() {
        let cases: [(&[u8], &str); 6] = [
            (&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a], "png"),
            (&[0xff, 0xd8, 0xff, 0xe0], "jpeg"),
            (b"GIF89a...", "gif"),
            (b"BM______", "bmp"),
            (&[0x00, 0x00, 0x01, 0x00], "ico"),
            (b"RIFF____WEBPVP8 ", "webp"),
        ];
        for (bytes, expected) in cases {
            assert_eq!(image_format(bytes), Some(expected), "for {expected}");
        }
    }

    #[test]
    fn a_riff_container_that_is_not_webp_is_not_an_image() {
        // RIFF also carries audio. Handing a WAV to an image decoder would fail slowly
        // rather than being refused immediately.
        assert_eq!(image_format(b"RIFF____WAVEfmt "), None);
    }

    #[test]
    fn text_is_still_text() {
        let preview = classify(raw(b"server { root /srv/x; }"), "/etc/nginx/x.conf");
        assert_eq!(preview.kind(), "text");
        match preview {
            Preview::Text(contents) => assert!(contents.text.contains("root")),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn something_that_is_neither_is_reported_by_size_rather_than_shown() {
        // A window full of mojibake tells the user nothing; "4 MB, not text" does.
        let preview = classify(
            FileBytes {
                bytes: vec![0x7f, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00],
                truncated: true,
                size_bytes: 4_000_000,
            },
            "/bin/ls",
        );
        assert_eq!(
            preview,
            Preview::Binary {
                size_bytes: 4_000_000
            }
        );
    }

    #[test]
    fn only_an_image_extension_earns_the_larger_budget() {
        // The extension picks how much to fetch and nothing else. Asking for eight
        // megabytes of every log would be paid for on every slow link.
        assert_eq!(read_budget("/var/www/logo.PNG"), MAX_IMAGE_BYTES);
        assert_eq!(read_budget("/var/www/photo.jpeg"), MAX_IMAGE_BYTES);
        assert_eq!(read_budget("/var/log/syslog"), MAX_TEXT_BYTES);
        assert_eq!(read_budget("/etc/nginx/nginx.conf"), MAX_TEXT_BYTES);
    }

    #[test]
    fn a_dotfile_has_no_extension() {
        // `.env` is a name, not an extension. Reading `env` as one would be wrong here in
        // a way that decides how much is fetched.
        assert_eq!(extension("/var/www/.env"), None);
        assert_eq!(extension("/var/www/.htaccess"), None);
        assert_eq!(extension("/var/www/.env.local"), Some("local".to_owned()));
        assert_eq!(extension("/var/www/index.php"), Some("php".to_owned()));
        assert_eq!(extension("/var/www/noextension"), None);
    }

    #[test]
    fn an_image_too_large_to_have_arrived_whole_says_so_instead_of_failing_to_decode() {
        // Truncated bytes will not decode. Reporting that is better than an image widget
        // that silently shows nothing.
        let preview = classify(
            FileBytes {
                bytes: vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
                truncated: true,
                size_bytes: 50_000_000,
            },
            "/var/www/huge.png",
        );
        match preview {
            Preview::Image(image) => {
                assert!(image.truncated);
                assert_eq!(image.size_bytes, 50_000_000);
            }
            other => panic!("expected an image, got {other:?}"),
        }
    }
}
