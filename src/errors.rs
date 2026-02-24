//! Error types for the image optimization agent.

use thiserror::Error;

/// Result type for image optimization operations.
pub type ImageOptResult<T> = Result<T, ImageOptError>;

/// Errors that can occur during image optimization operations.
#[derive(Debug, Error)]
pub enum ImageOptError {
    /// Body exceeds the maximum buffer size.
    #[error("buffer overflow: body exceeds {max_bytes} bytes")]
    BufferOverflow { max_bytes: usize },

    /// Failed to decode the source image.
    #[error("image decode error: {0}")]
    DecodeError(String),

    /// Failed to encode the output image.
    #[error("image encode error: {0}")]
    EncodeError(String),

    /// Image exceeds the maximum pixel count.
    #[error("image too large: {pixels} pixels exceeds limit of {max_pixels}")]
    ImageTooLarge { pixels: u64, max_pixels: u64 },

    /// Cache I/O error.
    #[error("cache error: {0}")]
    CacheError(String),

    /// No format supported by the client.
    #[error("no supported output format for client Accept header")]
    NoSupportedFormat,

    /// Image conversion timed out.
    #[error("conversion timed out after {timeout_ms}ms")]
    ConversionTimeout { timeout_ms: u64 },

    /// Base64 decoding error.
    #[error("base64 decode error: {0}")]
    Base64Decode(String),

    /// Configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
