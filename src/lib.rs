//! Image Optimization Agent for Zentinel
//!
//! This agent provides automatic image optimization:
//! - On-the-fly JPEG/PNG to WebP/AVIF conversion
//! - Content negotiation based on client Accept header
//! - Content-addressable filesystem caching with LRU eviction
//! - Graceful fallback to original on any error

pub mod buffer;
pub mod cache;
pub mod config;
pub mod converter;
pub mod errors;
pub mod handler;
pub mod negotiation;

pub use config::ImageOptConfig;
pub use errors::{ImageOptError, ImageOptResult};
pub use handler::ImageOptAgent;
