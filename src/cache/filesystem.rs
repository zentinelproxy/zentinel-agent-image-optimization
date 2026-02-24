//! Content-addressable filesystem cache with LRU eviction.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::fs;
use tracing::{debug, warn};

use crate::cache::{CacheEntryMeta, CacheStore};
use crate::config::CacheConfig;
use crate::errors::ImageOptError;

/// Filesystem-based cache with content-addressable storage.
///
/// Uses two-level directory sharding: `{dir}/{hash[0..2]}/{hash[2..4]}/{hash}.bin`
/// with a metadata sidecar `{hash}.meta.json`.
pub struct FilesystemCache {
    /// Root cache directory.
    base_dir: PathBuf,
    /// Maximum total cache size in bytes.
    max_size_bytes: u64,
    /// TTL for cache entries in seconds.
    ttl_secs: u64,
    /// Current approximate total cache size (tracked in-memory).
    current_size: Arc<AtomicU64>,
}

impl FilesystemCache {
    /// Create a new filesystem cache.
    ///
    /// Creates the base directory if it doesn't exist.
    pub async fn new(config: &CacheConfig) -> Result<Self, ImageOptError> {
        let base_dir = PathBuf::from(&config.directory);
        fs::create_dir_all(&base_dir)
            .await
            .map_err(|e| ImageOptError::CacheError(format!("failed to create cache dir: {}", e)))?;

        let cache = Self {
            base_dir,
            max_size_bytes: config.max_size_bytes,
            ttl_secs: config.ttl_secs,
            current_size: Arc::new(AtomicU64::new(0)),
        };

        // Scan existing entries to rebuild size index
        cache.scan_size().await;

        Ok(cache)
    }

    /// Build the file path for a cache entry's data file.
    fn data_path(&self, key: &str) -> PathBuf {
        let shard1 = &key[..2];
        let shard2 = &key[2..4];
        self.base_dir
            .join(shard1)
            .join(shard2)
            .join(format!("{}.bin", key))
    }

    /// Build the file path for a cache entry's metadata sidecar.
    fn meta_path(&self, key: &str) -> PathBuf {
        let shard1 = &key[..2];
        let shard2 = &key[2..4];
        self.base_dir
            .join(shard1)
            .join(shard2)
            .join(format!("{}.meta.json", key))
    }

    /// Get the current Unix timestamp.
    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Scan all entries to compute total cache size.
    async fn scan_size(&self) {
        let mut total: u64 = 0;
        if let Ok(mut entries) = fs::read_dir(&self.base_dir).await {
            while let Ok(Some(shard1)) = entries.next_entry().await {
                if !shard1.path().is_dir() {
                    continue;
                }
                if let Ok(mut sub) = fs::read_dir(shard1.path()).await {
                    while let Ok(Some(shard2)) = sub.next_entry().await {
                        if !shard2.path().is_dir() {
                            continue;
                        }
                        if let Ok(mut files) = fs::read_dir(shard2.path()).await {
                            while let Ok(Some(file)) = files.next_entry().await {
                                if file.path().extension().is_some_and(|ext| ext == "bin") {
                                    if let Ok(meta) = file.metadata().await {
                                        total += meta.len();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        self.current_size.store(total, Ordering::Relaxed);
        debug!(total_bytes = total, "Cache size scan complete");
    }

    /// Evict oldest entries if total size exceeds the limit.
    ///
    /// This is called after each put operation. It collects all entries,
    /// sorts by creation time, and removes the oldest until under the limit.
    pub async fn maybe_evict(&self) {
        let current = self.current_size.load(Ordering::Relaxed);
        if current <= self.max_size_bytes {
            return;
        }

        debug!(
            current_bytes = current,
            max_bytes = self.max_size_bytes,
            "Starting cache eviction"
        );

        // Collect all data files with their metadata
        let mut entries: Vec<(PathBuf, u64, u64)> = Vec::new(); // (path, size, created_at)

        if let Ok(mut shard1_iter) = fs::read_dir(&self.base_dir).await {
            while let Ok(Some(shard1)) = shard1_iter.next_entry().await {
                if !shard1.path().is_dir() {
                    continue;
                }
                if let Ok(mut shard2_iter) = fs::read_dir(shard1.path()).await {
                    while let Ok(Some(shard2)) = shard2_iter.next_entry().await {
                        if !shard2.path().is_dir() {
                            continue;
                        }
                        if let Ok(mut files) = fs::read_dir(shard2.path()).await {
                            while let Ok(Some(file)) = files.next_entry().await {
                                let path = file.path();
                                if path.extension().is_some_and(|ext| ext == "bin") {
                                    let size =
                                        file.metadata().await.map(|m| m.len()).unwrap_or(0);
                                    // Read the metadata sidecar for created_at
                                    let meta_path = path.with_extension("meta.json");
                                    let created_at = read_created_at(&meta_path).await;
                                    entries.push((path, size, created_at));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Sort by created_at ascending (oldest first)
        entries.sort_by_key(|e| e.2);

        let mut freed: u64 = 0;
        let target = current.saturating_sub(self.max_size_bytes);

        for (data_path, size, _) in &entries {
            if freed >= target {
                break;
            }
            // Remove data file and metadata sidecar
            let meta_path = data_path.with_extension("meta.json");
            if let Err(e) = fs::remove_file(data_path).await {
                warn!(path = ?data_path, error = %e, "Failed to evict cache entry");
                continue;
            }
            let _ = fs::remove_file(&meta_path).await;
            freed += size;
        }

        self.current_size
            .fetch_sub(freed, Ordering::Relaxed);

        debug!(freed_bytes = freed, "Cache eviction complete");
    }
}

/// Read the `created_at` timestamp from a metadata sidecar file.
async fn read_created_at(path: &Path) -> u64 {
    match fs::read(path).await {
        Ok(data) => serde_json::from_slice::<CacheEntryMeta>(&data)
            .map(|m| m.created_at)
            .unwrap_or(0),
        Err(_) => 0,
    }
}

#[async_trait]
impl CacheStore for FilesystemCache {
    async fn get(&self, key: &str) -> Result<Option<(Vec<u8>, CacheEntryMeta)>, ImageOptError> {
        let data_path = self.data_path(key);
        let meta_path = self.meta_path(key);

        // Read metadata first (cheaper than reading the full image)
        let meta_bytes = match fs::read(&meta_path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(ImageOptError::CacheError(format!(
                    "failed to read cache metadata: {}",
                    e
                )));
            }
        };

        let meta: CacheEntryMeta = serde_json::from_slice(&meta_bytes).map_err(|e| {
            ImageOptError::CacheError(format!("failed to parse cache metadata: {}", e))
        })?;

        // Check TTL
        let age = Self::now_secs().saturating_sub(meta.created_at);
        if age > self.ttl_secs {
            // Expired — remove lazily
            let _ = fs::remove_file(&data_path).await;
            let _ = fs::remove_file(&meta_path).await;
            self.current_size
                .fetch_sub(meta.optimized_size as u64, Ordering::Relaxed);
            return Ok(None);
        }

        // Read data file
        let data = match fs::read(&data_path).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(ImageOptError::CacheError(format!(
                    "failed to read cache data: {}",
                    e
                )));
            }
        };

        Ok(Some((data, meta)))
    }

    async fn put(
        &self,
        key: &str,
        data: &[u8],
        meta: &CacheEntryMeta,
    ) -> Result<(), ImageOptError> {
        let data_path = self.data_path(key);
        let meta_path = self.meta_path(key);

        // Ensure shard directories exist
        if let Some(parent) = data_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                ImageOptError::CacheError(format!("failed to create cache shard dir: {}", e))
            })?;
        }

        // Write data file
        fs::write(&data_path, data).await.map_err(|e| {
            ImageOptError::CacheError(format!("failed to write cache data: {}", e))
        })?;

        // Write metadata sidecar
        let meta_bytes = serde_json::to_vec(meta).map_err(|e| {
            ImageOptError::CacheError(format!("failed to serialize cache metadata: {}", e))
        })?;
        fs::write(&meta_path, &meta_bytes).await.map_err(|e| {
            ImageOptError::CacheError(format!("failed to write cache metadata: {}", e))
        })?;

        // Update size tracking
        self.current_size
            .fetch_add(data.len() as u64, Ordering::Relaxed);

        // Trigger eviction if needed (in background)
        self.maybe_evict().await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_cache(dir: &Path) -> FilesystemCache {
        let config = CacheConfig {
            enabled: true,
            directory: dir.to_string_lossy().to_string(),
            max_size_bytes: 1_000_000,
            ttl_secs: 3600,
        };
        FilesystemCache::new(&config).await.unwrap()
    }

    fn test_meta() -> CacheEntryMeta {
        CacheEntryMeta {
            content_type: "image/webp".to_string(),
            original_size: 1000,
            optimized_size: 500,
            created_at: FilesystemCache::now_secs(),
        }
    }

    #[tokio::test]
    async fn put_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let cache = test_cache(dir.path()).await;

        let meta = test_meta();
        cache.put("abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890", b"image data", &meta).await.unwrap();

        let result = cache.get("abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890").await.unwrap();
        assert!(result.is_some());
        let (data, cached_meta) = result.unwrap();
        assert_eq!(data, b"image data");
        assert_eq!(cached_meta.content_type, "image/webp");
        assert_eq!(cached_meta.original_size, 1000);
    }

    #[tokio::test]
    async fn get_missing_key_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = test_cache(dir.path()).await;

        let result = cache.get("0000000000000000000000000000000000000000000000000000000000000000").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn expired_entry_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let config = CacheConfig {
            enabled: true,
            directory: dir.path().to_string_lossy().to_string(),
            max_size_bytes: 1_000_000,
            ttl_secs: 0, // Immediate expiry
        };
        let cache = FilesystemCache::new(&config).await.unwrap();

        let meta = CacheEntryMeta {
            content_type: "image/webp".to_string(),
            original_size: 1000,
            optimized_size: 500,
            created_at: 0, // Very old
        };
        let key = "1111111111111111111111111111111111111111111111111111111111111111";
        cache.put(key, b"old data", &meta).await.unwrap();

        let result = cache.get(key).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn eviction_removes_oldest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let config = CacheConfig {
            enabled: true,
            directory: dir.path().to_string_lossy().to_string(),
            max_size_bytes: 100, // Very small limit
            ttl_secs: 3600,
        };
        let cache = FilesystemCache::new(&config).await.unwrap();

        // Insert several entries that exceed the limit
        let now = FilesystemCache::now_secs();
        for i in 0..5 {
            let key = format!("{:064x}", i);
            let meta = CacheEntryMeta {
                content_type: "image/webp".to_string(),
                original_size: 100,
                optimized_size: 50,
                created_at: now - (5 - i), // Oldest first
            };
            cache.put(&key, &vec![0u8; 50], &meta).await.unwrap();
        }

        // After eviction, total size should be under the limit
        let current = cache.current_size.load(Ordering::Relaxed);
        assert!(current <= config.max_size_bytes, "Cache size {} exceeds limit {}", current, config.max_size_bytes);
    }
}
