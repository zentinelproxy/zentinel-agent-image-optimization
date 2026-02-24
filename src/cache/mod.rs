//! Image cache layer.

pub mod filesystem;
pub mod key;

use crate::errors::ImageOptError;
use async_trait::async_trait;

/// Metadata stored alongside cached images.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CacheEntryMeta {
    /// Output content type (e.g., "image/webp").
    pub content_type: String,
    /// Original image size in bytes.
    pub original_size: usize,
    /// Optimized image size in bytes.
    pub optimized_size: usize,
    /// Unix timestamp when this entry was created.
    pub created_at: u64,
}

/// Trait for cache storage backends.
#[async_trait]
pub trait CacheStore: Send + Sync {
    /// Retrieve a cached image by key.
    ///
    /// Returns the image bytes and metadata, or `None` if not found or expired.
    async fn get(&self, key: &str) -> Result<Option<(Vec<u8>, CacheEntryMeta)>, ImageOptError>;

    /// Store an image in the cache.
    async fn put(&self, key: &str, data: &[u8], meta: &CacheEntryMeta)
        -> Result<(), ImageOptError>;
}
