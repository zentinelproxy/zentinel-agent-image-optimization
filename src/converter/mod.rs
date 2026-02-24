//! Image conversion traits and factory.

pub mod avif;
pub mod webp;

use crate::config::OutputFormat;
use crate::errors::ImageOptError;

/// Trait for image format converters.
pub trait ImageConverter: Send + Sync {
    /// The output format this converter produces.
    fn format(&self) -> OutputFormat;

    /// The MIME content type of the output.
    fn content_type(&self) -> &'static str;

    /// Convert an input image (JPEG or PNG bytes) to the output format.
    ///
    /// # Errors
    ///
    /// Returns `ImageOptError::DecodeError` if the input cannot be decoded.
    /// Returns `ImageOptError::EncodeError` if encoding fails.
    /// Returns `ImageOptError::ImageTooLarge` if the pixel count exceeds the limit.
    fn convert(
        &self,
        input: &[u8],
        quality: u8,
        max_pixel_count: u64,
    ) -> Result<Vec<u8>, ImageOptError>;
}

/// Create a converter for the specified output format.
pub fn create_converter(format: OutputFormat) -> Box<dyn ImageConverter> {
    match format {
        OutputFormat::WebP => Box::new(webp::WebPConverter),
        OutputFormat::Avif => Box::new(avif::AvifConverter),
    }
}
