//! WebP image converter using the `image` crate.

use crate::config::OutputFormat;
use crate::converter::ImageConverter;
use crate::errors::ImageOptError;

/// WebP encoder.
pub struct WebPConverter;

impl ImageConverter for WebPConverter {
    fn format(&self) -> OutputFormat {
        OutputFormat::WebP
    }

    fn content_type(&self) -> &'static str {
        "image/webp"
    }

    fn convert(
        &self,
        input: &[u8],
        _quality: u8,
        max_pixel_count: u64,
    ) -> Result<Vec<u8>, ImageOptError> {
        // Decode the source image
        let img = image::load_from_memory(input)
            .map_err(|e| ImageOptError::DecodeError(e.to_string()))?;

        // Check pixel count
        let pixels = img.width() as u64 * img.height() as u64;
        if pixels > max_pixel_count {
            return Err(ImageOptError::ImageTooLarge {
                pixels,
                max_pixels: max_pixel_count,
            });
        }

        // Encode to WebP lossless (VP8L). The pure-Rust image-webp encoder only supports
        // lossless encoding. Lossy WebP would require native libwebp bindings. The quality
        // parameter is unused for WebP — use AVIF for quality-controlled lossy compression.
        let mut output = std::io::Cursor::new(Vec::new());
        let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut output);

        img.write_with_encoder(encoder)
            .map_err(|e| ImageOptError::EncodeError(format!("WebP encode error: {}", e)))?;

        Ok(output.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a minimal valid JPEG image for testing.
    fn test_jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_fn(2, 2, |x, y| {
            if (x + y) % 2 == 0 {
                image::Rgb([255, 0, 0])
            } else {
                image::Rgb([0, 0, 255])
            }
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
        buf.into_inner()
    }

    /// Create a minimal valid PNG image for testing.
    fn test_png() -> Vec<u8> {
        let img = image::RgbImage::from_fn(2, 2, |_, _| image::Rgb([0, 255, 0]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn converts_jpeg_to_webp() {
        let converter = WebPConverter;
        let jpeg = test_jpeg();
        let result = converter.convert(&jpeg, 80, 25_000_000);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(!output.is_empty());
        // WebP files start with "RIFF" magic bytes
        assert_eq!(&output[..4], b"RIFF");
    }

    #[test]
    fn converts_png_to_webp() {
        let converter = WebPConverter;
        let png = test_png();
        let result = converter.convert(&png, 80, 25_000_000);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(!output.is_empty());
        assert_eq!(&output[..4], b"RIFF");
    }

    #[test]
    fn rejects_oversized_image() {
        let converter = WebPConverter;
        let jpeg = test_jpeg(); // 2x2 = 4 pixels
        let result = converter.convert(&jpeg, 80, 3); // max 3 pixels
        assert!(matches!(result, Err(ImageOptError::ImageTooLarge { .. })));
    }

    #[test]
    fn rejects_invalid_input() {
        let converter = WebPConverter;
        let result = converter.convert(b"not an image", 80, 25_000_000);
        assert!(matches!(result, Err(ImageOptError::DecodeError(_))));
    }

    #[test]
    fn format_and_content_type() {
        let converter = WebPConverter;
        assert_eq!(converter.format(), OutputFormat::WebP);
        assert_eq!(converter.content_type(), "image/webp");
    }
}
