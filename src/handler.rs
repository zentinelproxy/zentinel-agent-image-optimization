//! AgentHandler implementation for the Image Optimization Agent.

use crate::buffer::ChunkBuffer;
use crate::cache::filesystem::FilesystemCache;
use crate::cache::key::cache_key;
use crate::cache::{CacheEntryMeta, CacheStore};
use crate::config::{validate_config, ImageOptConfig, OutputFormat};
use crate::converter::{self, ImageConverter};
use crate::negotiation::negotiate_format;
use async_trait::async_trait;
use base64::Engine as _;
use dashmap::DashMap;
use regex::Regex;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use zentinel_agent_protocol::{
    AgentHandler, AgentResponse, BodyMutation, ConfigureEvent, HeaderOp, RequestCompleteEvent,
    RequestHeadersEvent, ResponseBodyChunkEvent, ResponseHeadersEvent,
};

/// Per-request state.
struct RequestState {
    /// Request URI.
    uri: String,
    /// Client Accept header value.
    accept_header: Option<String>,
    /// Response Content-Type.
    response_content_type: Option<String>,
    /// Response body buffer.
    response_buffer: ChunkBuffer,
    /// Whether this request is eligible for optimization.
    eligible: bool,
    /// Negotiated target output format.
    target_format: Option<OutputFormat>,
    /// Cached image bytes (set on cache hit).
    cache_hit: Option<Vec<u8>>,
    /// Cached content type (set on cache hit).
    cache_hit_content_type: Option<String>,
}

/// Image Optimization Agent handler.
pub struct ImageOptAgent {
    /// Configuration.
    config: Arc<RwLock<ImageOptConfig>>,
    /// Compiled passthrough patterns.
    passthrough_patterns: Arc<RwLock<Vec<Regex>>>,
    /// Converters indexed by format.
    converters: Arc<DashMap<String, Box<dyn ImageConverter>>>,
    /// Filesystem cache (None if caching disabled).
    cache: Arc<RwLock<Option<FilesystemCache>>>,
    /// Per-request state (correlation_id -> state).
    request_state: DashMap<String, RequestState>,
}

impl ImageOptAgent {
    /// Create a new image optimization agent with the given configuration.
    pub async fn new(config: ImageOptConfig) -> Result<Self, anyhow::Error> {
        // Validate config
        validate_config(&config).map_err(|e| anyhow::anyhow!("invalid config: {}", e))?;

        // Compile passthrough patterns
        let patterns = compile_patterns(&config.passthrough_patterns)?;

        // Create converters for configured formats
        let converters = DashMap::new();
        for &format in &config.formats {
            converters.insert(format.as_str().to_string(), converter::create_converter(format));
        }

        // Initialize cache if enabled
        let cache = if config.cache.enabled {
            Some(FilesystemCache::new(&config.cache).await.map_err(|e| {
                anyhow::anyhow!("failed to initialize cache: {}", e)
            })?)
        } else {
            None
        };

        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            passthrough_patterns: Arc::new(RwLock::new(patterns)),
            converters: Arc::new(converters),
            cache: Arc::new(RwLock::new(cache)),
            request_state: DashMap::new(),
        })
    }

    /// Get the quality setting for a format.
    async fn quality_for(&self, format: OutputFormat) -> u8 {
        let config = self.config.read().await;
        match format {
            OutputFormat::WebP => config.quality.webp,
            OutputFormat::Avif => config.quality.avif,
        }
    }
}

/// Compile regex patterns from string slices.
fn compile_patterns(patterns: &[String]) -> Result<Vec<Regex>, anyhow::Error> {
    patterns
        .iter()
        .map(|p| Regex::new(p).map_err(|e| anyhow::anyhow!("invalid regex '{}': {}", p, e)))
        .collect()
}

#[async_trait]
impl AgentHandler for ImageOptAgent {
    async fn on_configure(&self, event: ConfigureEvent) -> AgentResponse {
        info!(agent_id = %event.agent_id, "Received configuration");

        match serde_json::from_value::<ImageOptConfig>(event.config.clone()) {
            Ok(new_config) => {
                if let Err(e) = validate_config(&new_config) {
                    error!(error = %e, "Invalid configuration");
                    return AgentResponse::block(
                        500,
                        Some(format!("Invalid configuration: {}", e)),
                    );
                }

                // Recompile patterns
                let patterns = match compile_patterns(&new_config.passthrough_patterns) {
                    Ok(p) => p,
                    Err(e) => {
                        error!(error = %e, "Failed to compile passthrough patterns");
                        return AgentResponse::block(500, Some(format!("Pattern error: {}", e)));
                    }
                };

                // Update converters
                self.converters.clear();
                for &format in &new_config.formats {
                    self.converters.insert(
                        format.as_str().to_string(),
                        converter::create_converter(format),
                    );
                }

                // Update cache
                let new_cache = if new_config.cache.enabled {
                    match FilesystemCache::new(&new_config.cache).await {
                        Ok(c) => Some(c),
                        Err(e) => {
                            error!(error = %e, "Failed to initialize cache");
                            return AgentResponse::block(
                                500,
                                Some(format!("Cache error: {}", e)),
                            );
                        }
                    }
                } else {
                    None
                };

                *self.passthrough_patterns.write().await = patterns;
                *self.cache.write().await = new_cache;
                *self.config.write().await = new_config;

                info!("Configuration updated successfully");
                AgentResponse::default_allow()
            }
            Err(e) => {
                warn!(error = %e, "Failed to parse configuration, using defaults");
                AgentResponse::default_allow()
            }
        }
    }

    async fn on_request_headers(&self, event: RequestHeadersEvent) -> AgentResponse {
        let correlation_id = &event.metadata.correlation_id;

        debug!(
            correlation_id = %correlation_id,
            method = %event.method,
            uri = %event.uri,
            "Processing request headers"
        );

        // Extract Accept header
        let accept_header = event
            .headers
            .get("accept")
            .and_then(|v| v.first())
            .cloned();

        // Check passthrough patterns
        let patterns = self.passthrough_patterns.read().await;
        let passthrough = patterns.iter().any(|p| p.is_match(&event.uri));
        drop(patterns);

        let config = self.config.read().await;
        let max_size = config.max_input_size_bytes;
        drop(config);

        self.request_state.insert(
            correlation_id.to_string(),
            RequestState {
                uri: event.uri.clone(),
                accept_header,
                response_content_type: None,
                response_buffer: ChunkBuffer::new(max_size),
                eligible: !passthrough,
                target_format: None,
                cache_hit: None,
                cache_hit_content_type: None,
            },
        );

        AgentResponse::default_allow()
    }

    async fn on_response_headers(&self, event: ResponseHeadersEvent) -> AgentResponse {
        let correlation_id = &event.correlation_id;

        debug!(
            correlation_id = %correlation_id,
            status = event.status,
            "Processing response headers"
        );

        let mut state = match self.request_state.get_mut(correlation_id) {
            Some(s) => s,
            None => return AgentResponse::default_allow(),
        };

        // Only process successful responses
        if event.status != 200 {
            state.eligible = false;
            return AgentResponse::default_allow();
        }

        // Check Content-Type eligibility
        let content_type = event
            .headers
            .get("content-type")
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_default();

        let config = self.config.read().await;
        let eligible_ct = config
            .eligible_content_types
            .iter()
            .any(|ct| content_type.starts_with(ct));

        if !state.eligible || !eligible_ct {
            state.eligible = false;
            return AgentResponse::default_allow();
        }

        state.response_content_type = Some(content_type);

        // Negotiate output format
        let target_format =
            negotiate_format(state.accept_header.as_deref(), &config.formats);

        let target_format = match target_format {
            Some(f) => f,
            None => {
                debug!(correlation_id = %correlation_id, "No supported format for client");
                state.eligible = false;
                return AgentResponse::default_allow();
            }
        };

        state.target_format = Some(target_format);

        // Check cache
        let quality = match target_format {
            OutputFormat::WebP => config.quality.webp,
            OutputFormat::Avif => config.quality.avif,
        };
        let key = cache_key(&state.uri, target_format, quality);
        drop(config);

        let cache_guard = self.cache.read().await;
        if let Some(ref cache) = *cache_guard {
            match cache.get(&key).await {
                Ok(Some((data, meta))) => {
                    debug!(
                        correlation_id = %correlation_id,
                        format = target_format.as_str(),
                        "Cache hit"
                    );
                    state.cache_hit_content_type = Some(meta.content_type.clone());
                    state.cache_hit = Some(data);
                }
                Ok(None) => {
                    debug!(correlation_id = %correlation_id, "Cache miss");
                }
                Err(e) => {
                    warn!(
                        correlation_id = %correlation_id,
                        error = %e,
                        "Cache read error, treating as miss"
                    );
                }
            }
        }
        drop(cache_guard);

        // Remove Content-Length since the body size will change
        AgentResponse::default_allow().add_response_header(HeaderOp::Remove {
            name: "content-length".to_string(),
        })
    }

    async fn on_response_body_chunk(&self, event: ResponseBodyChunkEvent) -> AgentResponse {
        let correlation_id = &event.correlation_id;

        debug!(
            correlation_id = %correlation_id,
            chunk_index = event.chunk_index,
            is_last = event.is_last,
            data_len = event.data.len(),
            "Processing response body chunk"
        );

        let mut state = match self.request_state.get_mut(correlation_id) {
            Some(s) => s,
            None => {
                return AgentResponse::default_allow()
                    .with_response_body_mutation(BodyMutation::pass_through(event.chunk_index));
            }
        };

        // If not eligible, pass through
        if !state.eligible {
            return AgentResponse::default_allow()
                .with_response_body_mutation(BodyMutation::pass_through(event.chunk_index));
        }

        // Handle cache hit: drop non-last chunks, replace last with cached data
        if let Some(ref cached_data) = state.cache_hit {
            if !event.is_last {
                return AgentResponse::default_allow()
                    .set_needs_more(true)
                    .with_response_body_mutation(BodyMutation::drop_chunk(event.chunk_index));
            }

            let encoded = base64::engine::general_purpose::STANDARD.encode(cached_data);
            let ct = state
                .cache_hit_content_type
                .clone()
                .unwrap_or_else(|| "image/webp".to_string());
            let size = cached_data.len();

            return AgentResponse::default_allow()
                .with_response_body_mutation(BodyMutation::replace(event.chunk_index, encoded))
                .add_response_header(HeaderOp::Set {
                    name: "content-type".to_string(),
                    value: ct,
                })
                .add_response_header(HeaderOp::Set {
                    name: "content-length".to_string(),
                    value: size.to_string(),
                })
                .add_response_header(HeaderOp::Set {
                    name: "vary".to_string(),
                    value: "Accept".to_string(),
                })
                .add_response_header(HeaderOp::Set {
                    name: "x-image-optimized".to_string(),
                    value: "cache-hit".to_string(),
                });
        }

        // Decode base64 body chunk
        let chunk_data = match base64::engine::general_purpose::STANDARD.decode(&event.data) {
            Ok(data) => data,
            Err(e) => {
                error!(error = %e, "Failed to decode body chunk");
                state.eligible = false;
                return AgentResponse::default_allow()
                    .with_response_body_mutation(BodyMutation::pass_through(event.chunk_index));
            }
        };

        // Buffer the chunk
        if let Err(e) = state.response_buffer.append(&chunk_data) {
            warn!(error = %e, "Buffer overflow, passing through original");
            state.eligible = false;
            return AgentResponse::default_allow()
                .with_response_body_mutation(BodyMutation::pass_through(event.chunk_index));
        }

        // If not the last chunk, wait for more
        if !event.is_last {
            return AgentResponse::default_allow()
                .set_needs_more(true)
                .with_response_body_mutation(BodyMutation::drop_chunk(event.chunk_index));
        }

        // Last chunk — perform the conversion
        let target_format = match state.target_format {
            Some(f) => f,
            None => {
                let data = state.response_buffer.take();
                let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                return AgentResponse::default_allow()
                    .with_response_body_mutation(BodyMutation::replace(event.chunk_index, encoded));
            }
        };

        let complete_body = state.response_buffer.take();
        let original_size = complete_body.len();
        let uri = state.uri.clone();

        // Release the state lock before the blocking conversion
        drop(state);

        let quality = self.quality_for(target_format).await;
        let config = self.config.read().await;
        let max_pixel_count = config.max_pixel_count;
        drop(config);

        // Verify we have a converter for the target format
        if !self.converters.contains_key(target_format.as_str()) {
            let encoded = base64::engine::general_purpose::STANDARD.encode(&complete_body);
            return AgentResponse::default_allow()
                .with_response_body_mutation(BodyMutation::replace(event.chunk_index, encoded));
        }

        let format_str = target_format.as_str().to_string();
        let content_type = target_format.content_type().to_string();

        // Run the CPU-bound conversion in a blocking task
        // We need to get the converter's format info before spawning
        let body_for_conversion = complete_body.clone();
        let conversion_result = tokio::task::spawn_blocking(move || {
            // We need a converter inside the blocking task
            let conv = converter::create_converter(target_format);
            conv.convert(&body_for_conversion, quality, max_pixel_count)
        })
        .await;

        let optimized = match conversion_result {
            Ok(Ok(data)) => data,
            Ok(Err(e)) => {
                warn!(
                    correlation_id = %correlation_id,
                    error = %e,
                    format = %format_str,
                    "Conversion failed, passing through original"
                );
                let encoded = base64::engine::general_purpose::STANDARD.encode(&complete_body);
                return AgentResponse::default_allow()
                    .with_response_body_mutation(BodyMutation::replace(event.chunk_index, encoded));
            }
            Err(e) => {
                error!(
                    correlation_id = %correlation_id,
                    error = %e,
                    "Conversion task panicked, passing through original"
                );
                let encoded = base64::engine::general_purpose::STANDARD.encode(&complete_body);
                return AgentResponse::default_allow()
                    .with_response_body_mutation(BodyMutation::replace(event.chunk_index, encoded));
            }
        };

        let optimized_size = optimized.len();

        info!(
            correlation_id = %correlation_id,
            format = %format_str,
            original_size = original_size,
            optimized_size = optimized_size,
            "Image converted successfully"
        );

        // Store in cache (best-effort, don't fail the response)
        let cache_guard = self.cache.read().await;
        if let Some(ref cache) = *cache_guard {
            let key = cache_key(&uri, target_format, quality);
            let meta = CacheEntryMeta {
                content_type: content_type.clone(),
                original_size,
                optimized_size,
                created_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            };
            if let Err(e) = cache.put(&key, &optimized, &meta).await {
                warn!(
                    correlation_id = %correlation_id,
                    error = %e,
                    "Failed to cache optimized image"
                );
            }
        }
        drop(cache_guard);

        let encoded = base64::engine::general_purpose::STANDARD.encode(&optimized);

        AgentResponse::default_allow()
            .with_response_body_mutation(BodyMutation::replace(event.chunk_index, encoded))
            .add_response_header(HeaderOp::Set {
                name: "content-type".to_string(),
                value: content_type,
            })
            .add_response_header(HeaderOp::Set {
                name: "content-length".to_string(),
                value: optimized_size.to_string(),
            })
            .add_response_header(HeaderOp::Set {
                name: "vary".to_string(),
                value: "Accept".to_string(),
            })
            .add_response_header(HeaderOp::Set {
                name: "x-image-optimized".to_string(),
                value: format_str,
            })
            .add_response_header(HeaderOp::Set {
                name: "x-image-original-size".to_string(),
                value: original_size.to_string(),
            })
    }

    async fn on_request_complete(&self, event: RequestCompleteEvent) -> AgentResponse {
        let correlation_id = &event.correlation_id;

        debug!(
            correlation_id = %correlation_id,
            status = event.status,
            duration_ms = event.duration_ms,
            "Request completed"
        );

        // Clean up request state
        self.request_state.remove(correlation_id);

        AgentResponse::default_allow()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zentinel_agent_protocol::{RequestMetadata, ResponseBodyChunkEvent};

    fn test_metadata(correlation_id: &str) -> RequestMetadata {
        RequestMetadata {
            correlation_id: correlation_id.to_string(),
            request_id: "req-1".to_string(),
            client_ip: "127.0.0.1".to_string(),
            client_port: 12345,
            server_name: Some("example.com".to_string()),
            protocol: "HTTP/1.1".to_string(),
            tls_version: None,
            tls_cipher: None,
            route_id: Some("default".to_string()),
            upstream_id: Some("backend".to_string()),
            timestamp: "2026-01-01T00:00:00Z".to_string(),
            traceparent: None,
        }
    }

    /// Create a minimal JPEG for testing.
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

    async fn test_agent() -> ImageOptAgent {
        let mut config = ImageOptConfig::default();
        config.cache.enabled = false; // Disable cache for unit tests
        ImageOptAgent::new(config).await.unwrap()
    }

    #[tokio::test]
    async fn eligible_jpeg_gets_converted_to_webp() {
        let agent = test_agent().await;
        let cid = "test-eligible";

        // 1. Request headers with Accept: image/webp
        let mut headers = HashMap::new();
        headers.insert("accept".to_string(), vec!["image/webp, image/jpeg".to_string()]);

        agent
            .on_request_headers(RequestHeadersEvent {
                metadata: test_metadata(cid),
                method: "GET".to_string(),
                uri: "/photos/cat.jpg".to_string(),
                headers,
            })
            .await;

        // 2. Response headers with Content-Type: image/jpeg
        let mut resp_headers = HashMap::new();
        resp_headers.insert(
            "content-type".to_string(),
            vec!["image/jpeg".to_string()],
        );

        let response = agent
            .on_response_headers(ResponseHeadersEvent {
                correlation_id: cid.to_string(),
                status: 200,
                headers: resp_headers,
            })
            .await;

        // Should remove content-length
        assert!(response.response_headers.iter().any(|h| matches!(h, HeaderOp::Remove { name } if name == "content-length")));

        // 3. Response body (single chunk with JPEG data)
        let jpeg_data = test_jpeg();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&jpeg_data);

        let response = agent
            .on_response_body_chunk(ResponseBodyChunkEvent {
                correlation_id: cid.to_string(),
                data: encoded,
                is_last: true,
                total_size: Some(jpeg_data.len()),
                chunk_index: 0,
                bytes_sent: jpeg_data.len(),
            })
            .await;

        // Should have replaced body and set headers
        assert!(response.response_body_mutation.is_some());
        let mutation = response.response_body_mutation.unwrap();
        assert!(!mutation.is_pass_through());
        assert!(!mutation.is_drop());

        // Check output headers
        assert!(response.response_headers.iter().any(
            |h| matches!(h, HeaderOp::Set { name, value } if name == "content-type" && value == "image/webp")
        ));
        assert!(response.response_headers.iter().any(
            |h| matches!(h, HeaderOp::Set { name, value } if name == "x-image-optimized" && value == "webp")
        ));
        assert!(response.response_headers.iter().any(
            |h| matches!(h, HeaderOp::Set { name, value } if name == "vary" && value == "Accept")
        ));
    }

    #[tokio::test]
    async fn ineligible_content_type_passes_through() {
        let agent = test_agent().await;
        let cid = "test-ineligible-ct";

        // Request headers
        let mut headers = HashMap::new();
        headers.insert("accept".to_string(), vec!["image/webp, */*".to_string()]);
        agent
            .on_request_headers(RequestHeadersEvent {
                metadata: test_metadata(cid),
                method: "GET".to_string(),
                uri: "/api/data.json".to_string(),
                headers,
            })
            .await;

        // Response headers with non-image content type
        let mut resp_headers = HashMap::new();
        resp_headers.insert(
            "content-type".to_string(),
            vec!["application/json".to_string()],
        );
        agent
            .on_response_headers(ResponseHeadersEvent {
                correlation_id: cid.to_string(),
                status: 200,
                headers: resp_headers,
            })
            .await;

        // Body chunk should pass through
        let response = agent
            .on_response_body_chunk(ResponseBodyChunkEvent {
                correlation_id: cid.to_string(),
                data: base64::engine::general_purpose::STANDARD.encode(b"{}"),
                is_last: true,
                total_size: Some(2),
                chunk_index: 0,
                bytes_sent: 2,
            })
            .await;

        assert!(response
            .response_body_mutation
            .as_ref()
            .is_some_and(|m| m.is_pass_through()));
    }

    #[tokio::test]
    async fn no_supported_format_passes_through() {
        let agent = test_agent().await;
        let cid = "test-no-format";

        // Request headers with no WebP/AVIF support
        let mut headers = HashMap::new();
        headers.insert("accept".to_string(), vec!["image/jpeg, image/png".to_string()]);
        agent
            .on_request_headers(RequestHeadersEvent {
                metadata: test_metadata(cid),
                method: "GET".to_string(),
                uri: "/photos/cat.jpg".to_string(),
                headers,
            })
            .await;

        // Response headers
        let mut resp_headers = HashMap::new();
        resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
        agent
            .on_response_headers(ResponseHeadersEvent {
                correlation_id: cid.to_string(),
                status: 200,
                headers: resp_headers,
            })
            .await;

        // Body chunk should pass through
        let jpeg_data = test_jpeg();
        let encoded = base64::engine::general_purpose::STANDARD.encode(&jpeg_data);
        let response = agent
            .on_response_body_chunk(ResponseBodyChunkEvent {
                correlation_id: cid.to_string(),
                data: encoded,
                is_last: true,
                total_size: Some(jpeg_data.len()),
                chunk_index: 0,
                bytes_sent: jpeg_data.len(),
            })
            .await;

        assert!(response
            .response_body_mutation
            .as_ref()
            .is_some_and(|m| m.is_pass_through()));
    }

    #[tokio::test]
    async fn passthrough_pattern_skips_optimization() {
        let mut config = ImageOptConfig::default();
        config.cache.enabled = false;
        config.passthrough_patterns = vec![r"\.gif$".to_string()];
        let agent = ImageOptAgent::new(config).await.unwrap();
        let cid = "test-passthrough";

        let mut headers = HashMap::new();
        headers.insert("accept".to_string(), vec!["image/webp, */*".to_string()]);
        agent
            .on_request_headers(RequestHeadersEvent {
                metadata: test_metadata(cid),
                method: "GET".to_string(),
                uri: "/images/animation.gif".to_string(),
                headers,
            })
            .await;

        let mut resp_headers = HashMap::new();
        resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
        agent
            .on_response_headers(ResponseHeadersEvent {
                correlation_id: cid.to_string(),
                status: 200,
                headers: resp_headers,
            })
            .await;

        let response = agent
            .on_response_body_chunk(ResponseBodyChunkEvent {
                correlation_id: cid.to_string(),
                data: base64::engine::general_purpose::STANDARD.encode(b"fake"),
                is_last: true,
                total_size: Some(4),
                chunk_index: 0,
                bytes_sent: 4,
            })
            .await;

        assert!(response
            .response_body_mutation
            .as_ref()
            .is_some_and(|m| m.is_pass_through()));
    }

    #[tokio::test]
    async fn non_200_response_passes_through() {
        let agent = test_agent().await;
        let cid = "test-non-200";

        let mut headers = HashMap::new();
        headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
        agent
            .on_request_headers(RequestHeadersEvent {
                metadata: test_metadata(cid),
                method: "GET".to_string(),
                uri: "/photos/cat.jpg".to_string(),
                headers,
            })
            .await;

        let mut resp_headers = HashMap::new();
        resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
        agent
            .on_response_headers(ResponseHeadersEvent {
                correlation_id: cid.to_string(),
                status: 304,
                headers: resp_headers,
            })
            .await;

        let response = agent
            .on_response_body_chunk(ResponseBodyChunkEvent {
                correlation_id: cid.to_string(),
                data: base64::engine::general_purpose::STANDARD.encode(b""),
                is_last: true,
                total_size: Some(0),
                chunk_index: 0,
                bytes_sent: 0,
            })
            .await;

        assert!(response
            .response_body_mutation
            .as_ref()
            .is_some_and(|m| m.is_pass_through()));
    }

    #[tokio::test]
    async fn request_complete_cleans_up_state() {
        let agent = test_agent().await;
        let cid = "test-cleanup";

        let headers = HashMap::new();
        agent
            .on_request_headers(RequestHeadersEvent {
                metadata: test_metadata(cid),
                method: "GET".to_string(),
                uri: "/test".to_string(),
                headers,
            })
            .await;

        assert!(agent.request_state.contains_key(cid));

        agent
            .on_request_complete(RequestCompleteEvent {
                correlation_id: cid.to_string(),
                status: 200,
                duration_ms: 100,
                request_body_size: 0,
                response_body_size: 1000,
                upstream_attempts: 1,
                error: None,
            })
            .await;

        assert!(!agent.request_state.contains_key(cid));
    }

    #[tokio::test]
    async fn corrupt_image_falls_back_to_original() {
        let agent = test_agent().await;
        let cid = "test-corrupt";

        let mut headers = HashMap::new();
        headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
        agent
            .on_request_headers(RequestHeadersEvent {
                metadata: test_metadata(cid),
                method: "GET".to_string(),
                uri: "/photos/bad.jpg".to_string(),
                headers,
            })
            .await;

        let mut resp_headers = HashMap::new();
        resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
        agent
            .on_response_headers(ResponseHeadersEvent {
                correlation_id: cid.to_string(),
                status: 200,
                headers: resp_headers,
            })
            .await;

        // Send corrupt image data
        let corrupt = b"this is not a valid jpeg";
        let encoded = base64::engine::general_purpose::STANDARD.encode(corrupt);
        let response = agent
            .on_response_body_chunk(ResponseBodyChunkEvent {
                correlation_id: cid.to_string(),
                data: encoded,
                is_last: true,
                total_size: Some(corrupt.len()),
                chunk_index: 0,
                bytes_sent: corrupt.len(),
            })
            .await;

        // Should still return a body (the original, passed through)
        let mutation = response.response_body_mutation.unwrap();
        assert!(!mutation.is_pass_through());
        assert!(!mutation.is_drop());
        // The data should be the original (base64 re-encoded)
        let returned = base64::engine::general_purpose::STANDARD
            .decode(mutation.data.unwrap())
            .unwrap();
        assert_eq!(returned, corrupt);
    }
}
