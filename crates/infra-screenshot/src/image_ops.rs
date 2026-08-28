//! Image handling: dimensions and thumbnails.

use vds_domain::ports::ScreenshotError;

/// PNG magic bytes.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Reads a PNG's dimensions from its header.
///
/// The IHDR chunk is always the first chunk and always at a fixed offset, so this needs
/// no decoder — which matters because it runs on every capture, including on a phone.
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // 8-byte signature, 4-byte length, 4-byte "IHDR", then width and height.
    if bytes.len() < 24 || bytes[..8] != PNG_SIGNATURE || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    (width > 0 && height > 0).then_some((width, height))
}

/// Whether the bytes look like a PNG at all.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[..8] == PNG_SIGNATURE
}

/// Produces a downscaled PNG whose longest edge is at most `max_edge`.
///
/// Mobile loads thumbnails first, so a card grid costs kilobytes rather than megabytes.
/// An image already within the limit is returned unchanged rather than re-encoded — that
/// would cost CPU to produce a slightly worse image.
pub fn thumbnail(png: &[u8], max_edge: u32) -> Result<Vec<u8>, ScreenshotError> {
    let max_edge = max_edge.max(16);

    if let Some((width, height)) = png_dimensions(png)
        && width <= max_edge
        && height <= max_edge
    {
        return Ok(png.to_vec());
    }

    let image = image::load_from_memory(png)
        .map_err(|e| ScreenshotError::InvalidImage(format!("could not decode: {e}")))?;

    // `thumbnail` uses a cheaper filter than `resize`, which is the right trade for a
    // preview that is about to be drawn at a fraction of its size anyway.
    let resized = image.thumbnail(max_edge, max_edge);

    let mut encoded = Vec::new();
    resized
        .write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )
        .map_err(|e| ScreenshotError::InvalidImage(format!("could not encode: {e}")))?;

    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a real PNG of the given size.
    fn png_of(width: u32, height: u32) -> Vec<u8> {
        let buffer = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(buffer)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encodes");
        bytes
    }

    #[test]
    fn dimensions_are_read_from_the_header_without_decoding() {
        let png = png_of(1_280, 800);
        assert_eq!(png_dimensions(&png), Some((1_280, 800)));
        assert!(is_png(&png));
    }

    #[test]
    fn non_png_bytes_are_rejected_rather_than_misread() {
        assert_eq!(png_dimensions(b"this is not a png"), None);
        assert_eq!(png_dimensions(&[]), None);
        assert!(!is_png(b"GIF89a"));
    }

    #[test]
    fn a_truncated_png_header_is_rejected() {
        let png = png_of(100, 100);
        assert_eq!(png_dimensions(&png[..20]), None);
    }

    #[test]
    fn a_large_capture_is_scaled_down_to_the_limit() {
        let png = png_of(1_280, 800);
        let small = thumbnail(&png, 480).expect("resizes");

        let (width, height) = png_dimensions(&small).expect("valid png");
        assert!(
            width <= 480 && height <= 480,
            "thumbnail was {width}x{height}"
        );
        // The aspect ratio is preserved, so the longest edge is the one at the limit.
        assert_eq!(width, 480);
        assert!(small.len() < png.len(), "the thumbnail should be smaller");
    }

    #[test]
    fn a_tall_image_is_bounded_by_its_height() {
        let png = png_of(400, 1_600);
        let small = thumbnail(&png, 200).expect("resizes");
        let (width, height) = png_dimensions(&small).expect("valid png");
        assert_eq!(height, 200);
        assert!(width <= 200);
    }

    #[test]
    fn an_image_already_small_enough_is_returned_untouched() {
        // Re-encoding would spend CPU to produce a slightly worse image.
        let png = png_of(200, 150);
        let result = thumbnail(&png, 480).expect("passes through");
        assert_eq!(result, png);
    }

    #[test]
    fn an_absurdly_small_limit_is_clamped_rather_than_producing_a_zero_pixel_image() {
        let png = png_of(1_000, 1_000);
        let small = thumbnail(&png, 0).expect("resizes");
        let (width, height) = png_dimensions(&small).expect("valid png");
        assert!(width > 0 && height > 0);
    }

    #[test]
    fn garbage_input_is_an_invalid_image_not_a_panic() {
        let err = thumbnail(b"definitely not an image", 480).expect_err("must fail");
        assert!(matches!(err, ScreenshotError::InvalidImage(_)));
    }

    #[test]
    fn a_square_image_scales_on_both_edges() {
        let png = png_of(1_000, 1_000);
        let small = thumbnail(&png, 100).expect("resizes");
        assert_eq!(png_dimensions(&small), Some((100, 100)));
    }
}
