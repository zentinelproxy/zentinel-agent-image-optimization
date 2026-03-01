//! Configuration schema for the Image Optimization Agent.

use serde::{Deserialize, Serialize};

/// Supported output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    WebP,
    Avif,
}

impl OutputFormat {
    /// Get the MIME content type for this format.
    pub fn content_type(&self) -> &'static str {
        match self {
            Self::WebP => "image/webp",
            Self::Avif => "image/avif",
        }
    }

    /// Get the format name as a string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::WebP => "webp",
            Self::Avif => "avif",
        }
    }
}

/// Root configuration for the Image Optimization Agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageOptConfig {
    /// Output formats in priority order.
    #[serde(default = "default_formats")]
    pub formats: Vec<OutputFormat>,

    /// Quality settings per format (1-100).
    #[serde(default = "default_quality")]
    pub quality: QualityConfig,

    /// Maximum input image size in bytes (default 10MB).
    #[serde(default = "default_max_input_size")]
    pub max_input_size_bytes: usize,

    /// Maximum pixel count (width * height, default 25M).
    #[serde(default = "default_max_pixel_count")]
    pub max_pixel_count: u64,

    /// Content types eligible for optimization.
    #[serde(default = "default_eligible_content_types")]
    pub eligible_content_types: Vec<String>,

    /// URL patterns to pass through without optimization (regex).
    #[serde(default)]
    pub passthrough_patterns: Vec<String>,

    /// Cache configuration.
    #[serde(default)]
    pub cache: CacheConfig,
}

impl Default for ImageOptConfig {
    fn default() -> Self {
        Self {
            formats: default_formats(),
            quality: default_quality(),
            max_input_size_bytes: default_max_input_size(),
            max_pixel_count: default_max_pixel_count(),
            eligible_content_types: default_eligible_content_types(),
            passthrough_patterns: Vec::new(),
            cache: CacheConfig::default(),
        }
    }
}

/// Quality settings per format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityConfig {
    /// WebP quality (1-100).
    #[serde(default = "default_webp_quality")]
    pub webp: u8,
    /// AVIF quality (1-100).
    #[serde(default = "default_avif_quality")]
    pub avif: u8,
}

impl Default for QualityConfig {
    fn default() -> Self {
        default_quality()
    }
}

/// Cache configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Whether caching is enabled.
    #[serde(default = "default_cache_enabled")]
    pub enabled: bool,
    /// Cache directory path.
    #[serde(default = "default_cache_directory")]
    pub directory: String,
    /// Maximum cache size in bytes (default 1GB).
    #[serde(default = "default_cache_max_size")]
    pub max_size_bytes: u64,
    /// TTL for cached entries in seconds (default 86400 = 24h).
    #[serde(default = "default_cache_ttl")]
    pub ttl_secs: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: default_cache_enabled(),
            directory: default_cache_directory(),
            max_size_bytes: default_cache_max_size(),
            ttl_secs: default_cache_ttl(),
        }
    }
}

// Default value functions

fn default_formats() -> Vec<OutputFormat> {
    vec![OutputFormat::WebP, OutputFormat::Avif]
}

fn default_quality() -> QualityConfig {
    QualityConfig {
        webp: default_webp_quality(),
        avif: default_avif_quality(),
    }
}

fn default_webp_quality() -> u8 {
    80
}

fn default_avif_quality() -> u8 {
    70
}

fn default_max_input_size() -> usize {
    10 * 1024 * 1024 // 10MB
}

fn default_max_pixel_count() -> u64 {
    25_000_000 // 25M pixels
}

fn default_eligible_content_types() -> Vec<String> {
    vec!["image/jpeg".to_string(), "image/png".to_string()]
}

fn default_cache_enabled() -> bool {
    true
}

fn default_cache_directory() -> String {
    // Prefer XDG-style user cache dir, fall back to /tmp for non-root users
    if let Ok(home) = std::env::var("HOME") {
        format!("{}/.cache/zentinel/image-optimization", home)
    } else {
        "/tmp/zentinel-image-optimization-cache".to_string()
    }
}

fn default_cache_max_size() -> u64 {
    1_073_741_824 // 1GB
}

fn default_cache_ttl() -> u64 {
    86_400 // 24 hours
}

/// Validate configuration.
///
/// # Errors
///
/// Returns a descriptive error string if any field is invalid.
pub fn validate_config(config: &ImageOptConfig) -> Result<(), String> {
    // Formats must be non-empty
    if config.formats.is_empty() {
        return Err("formats list cannot be empty".to_string());
    }

    // Quality bounds
    if config.quality.webp == 0 || config.quality.webp > 100 {
        return Err(format!(
            "webp quality must be 1-100, got {}",
            config.quality.webp
        ));
    }
    if config.quality.avif == 0 || config.quality.avif > 100 {
        return Err(format!(
            "avif quality must be 1-100, got {}",
            config.quality.avif
        ));
    }

    // Max input size must be positive
    if config.max_input_size_bytes == 0 {
        return Err("max_input_size_bytes must be greater than 0".to_string());
    }

    // Max pixel count must be positive
    if config.max_pixel_count == 0 {
        return Err("max_pixel_count must be greater than 0".to_string());
    }

    // Eligible content types must be non-empty
    if config.eligible_content_types.is_empty() {
        return Err("eligible_content_types cannot be empty".to_string());
    }

    // Validate passthrough patterns are valid regex
    for (i, pattern) in config.passthrough_patterns.iter().enumerate() {
        if regex::Regex::new(pattern).is_err() {
            return Err(format!(
                "passthrough_patterns[{}]: invalid regex '{}'",
                i, pattern
            ));
        }
    }

    // Cache directory must be non-empty if caching is enabled
    if config.cache.enabled && config.cache.directory.is_empty() {
        return Err("cache directory cannot be empty when caching is enabled".to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = ImageOptConfig::default();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn empty_formats_rejected() {
        let config = ImageOptConfig {
            formats: vec![],
            ..Default::default()
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn invalid_quality_rejected() {
        let mut config = ImageOptConfig::default();
        config.quality.webp = 0;
        assert!(validate_config(&config).is_err());

        config.quality.webp = 80;
        config.quality.avif = 101;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn invalid_regex_rejected() {
        let config = ImageOptConfig {
            passthrough_patterns: vec!["[invalid".to_string()],
            ..Default::default()
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn valid_passthrough_patterns_accepted() {
        let config = ImageOptConfig {
            passthrough_patterns: vec![r"\.gif$".to_string(), r"\.svg$".to_string()],
            ..Default::default()
        };
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn output_format_content_type() {
        assert_eq!(OutputFormat::WebP.content_type(), "image/webp");
        assert_eq!(OutputFormat::Avif.content_type(), "image/avif");
    }

    #[test]
    fn shipped_default_json_is_valid() {
        let json = include_str!("../config/default.json");
        let config: ImageOptConfig = serde_json::from_str(json).unwrap();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn config_deserializes_from_json() {
        let json = r#"{
            "formats": ["webp"],
            "quality": { "webp": 90, "avif": 60 },
            "max_input_size_bytes": 5242880,
            "passthrough_patterns": ["\\.gif$"]
        }"#;
        let config: ImageOptConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.formats, vec![OutputFormat::WebP]);
        assert_eq!(config.quality.webp, 90);
        assert_eq!(config.quality.avif, 60);
        assert_eq!(config.max_input_size_bytes, 5_242_880);
        assert_eq!(config.passthrough_patterns.len(), 1);
    }
}
