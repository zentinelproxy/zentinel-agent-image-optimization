//! AVIF image converter using `ravif` + `rgb` crates.

use crate::config::OutputFormat;
use crate::converter::ImageConverter;
use crate::errors::ImageOptError;

/// AVIF encoder.
pub struct AvifConverter;

impl ImageConverter for AvifConverter {
    fn format(&self) -> OutputFormat {
        OutputFormat::Avif
    }

    fn content_type(&self) -> &'static str {
        "image/avif"
    }

    fn convert(
        &self,
        input: &[u8],
        quality: u8,
        max_pixel_count: u64,
    ) -> Result<Vec<u8>, ImageOptError> {
        // Decode the source image
        let img = image::load_from_memory(input)
            .map_err(|e| ImageOptError::DecodeError(e.to_string()))?;

        // Check pixel count
        let width = img.width() as usize;
        let height = img.height() as usize;
        let pixels = width as u64 * height as u64;
        if pixels > max_pixel_count {
            return Err(ImageOptError::ImageTooLarge {
                pixels,
                max_pixels: max_pixel_count,
            });
        }

        // Convert to RGBA8
        let rgba = img.to_rgba8();
        let raw_pixels = rgba.as_raw();

        // Build an rgb::Img from the raw RGBA pixels
        let pixels_slice: &[rgb::RGBA8] = bytemuck_cast(raw_pixels, width * height);

        let img_ref = ravif::Img::new(pixels_slice, width, height);

        // Encode with ravif
        let encoder = ravif::Encoder::new()
            .with_quality(quality.into())
            .with_speed(6); // Balanced speed/quality

        let result = encoder
            .encode_rgba(img_ref)
            .map_err(|e| ImageOptError::EncodeError(format!("AVIF encode error: {}", e)))?;

        Ok(result.avif_file)
    }
}

/// Cast a `&[u8]` slice of RGBA bytes to `&[rgb::RGBA8]`.
///
/// This is safe because `rgb::RGBA8` has the same memory layout as `[u8; 4]`.
fn bytemuck_cast(bytes: &[u8], expected_pixels: usize) -> &[rgb::RGBA8] {
    assert_eq!(bytes.len(), expected_pixels * 4);
    // SAFETY: rgb::RGBA8 is repr(C) with 4 u8 fields, same layout as [u8; 4].
    unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const rgb::RGBA8, expected_pixels) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a minimal valid JPEG image for testing.
    fn test_jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_fn(4, 4, |x, y| {
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
        let img = image::RgbaImage::from_fn(4, 4, |_, _| image::Rgba([0, 255, 0, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn converts_jpeg_to_avif() {
        let converter = AvifConverter;
        let jpeg = test_jpeg();
        let result = converter.convert(&jpeg, 70, 25_000_000);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn converts_png_to_avif() {
        let converter = AvifConverter;
        let png = test_png();
        let result = converter.convert(&png, 70, 25_000_000);
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn rejects_oversized_image() {
        let converter = AvifConverter;
        let jpeg = test_jpeg(); // 4x4 = 16 pixels
        let result = converter.convert(&jpeg, 70, 10); // max 10 pixels
        assert!(matches!(result, Err(ImageOptError::ImageTooLarge { .. })));
    }

    #[test]
    fn rejects_invalid_input() {
        let converter = AvifConverter;
        let result = converter.convert(b"not an image", 70, 25_000_000);
        assert!(matches!(result, Err(ImageOptError::DecodeError(_))));
    }

    #[test]
    fn format_and_content_type() {
        let converter = AvifConverter;
        assert_eq!(converter.format(), OutputFormat::Avif);
        assert_eq!(converter.content_type(), "image/avif");
    }
}
