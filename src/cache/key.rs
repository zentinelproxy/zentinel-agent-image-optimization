//! Cache key generation using SHA-256.

use sha2::{Digest, Sha256};

use crate::config::OutputFormat;

/// Generate a deterministic cache key from the request URI, output format, and quality.
///
/// The key is the hex-encoded SHA-256 hash of `"{uri}:{format}:{quality}"`.
pub fn cache_key(uri: &str, format: OutputFormat, quality: u8) -> String {
    let input = format!("{}:{}:{}", uri, format.as_str(), quality);
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_deterministic() {
        let k1 = cache_key("/img/photo.jpg", OutputFormat::WebP, 80);
        let k2 = cache_key("/img/photo.jpg", OutputFormat::WebP, 80);
        assert_eq!(k1, k2);
    }

    #[test]
    fn different_inputs_produce_different_keys() {
        let k1 = cache_key("/img/photo.jpg", OutputFormat::WebP, 80);
        let k2 = cache_key("/img/photo.jpg", OutputFormat::Avif, 80);
        let k3 = cache_key("/img/photo.jpg", OutputFormat::WebP, 90);
        let k4 = cache_key("/img/other.jpg", OutputFormat::WebP, 80);
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
        assert_ne!(k1, k4);
    }

    #[test]
    fn key_is_valid_hex_sha256() {
        let key = cache_key("/test", OutputFormat::WebP, 80);
        assert_eq!(key.len(), 64); // SHA-256 = 32 bytes = 64 hex chars
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
