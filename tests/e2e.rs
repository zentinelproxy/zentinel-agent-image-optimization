//! End-to-end tests for the Image Optimization Agent.
//!
//! These tests exercise the full v2 wire protocol: start a `UdsAgentServerV2`
//! on a Unix socket, connect an `AgentClientV2Uds`, send events, and verify
//! responses — exactly as the proxy would interact with the agent in production.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use chrono::Utc;
use tempfile::tempdir;
use tokio::task::JoinHandle;

use zentinel_agent_image_optimization::{ImageOptAgent, ImageOptConfig};
use zentinel_agent_protocol::v2::{AgentClientV2Uds, UdsAgentServerV2};
use zentinel_agent_protocol::{
    Decision, HeaderOp, RequestCompleteEvent, RequestHeadersEvent, RequestMetadata,
    ResponseBodyChunkEvent, ResponseHeadersEvent,
};

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Start an agent server on a temporary Unix socket.
async fn start_agent(
    config: ImageOptConfig,
) -> (JoinHandle<()>, PathBuf, AgentClientV2Uds, tempfile::TempDir) {
    let dir = tempdir().expect("failed to create temp dir");
    let socket_path = dir.path().join("agent.sock");

    let agent = ImageOptAgent::new(config)
        .await
        .expect("failed to create agent");
    let server = UdsAgentServerV2::new("e2e-image-opt", socket_path.clone(), Box::new(agent));

    let handle = tokio::spawn(async move {
        server.run().await.unwrap();
    });

    // Give server time to bind
    tokio::time::sleep(Duration::from_millis(50)).await;

    let client = AgentClientV2Uds::new(
        "e2e-client",
        socket_path.to_string_lossy().to_string(),
        Duration::from_secs(5),
    )
    .await
    .expect("failed to create client");
    client.connect().await.expect("failed to connect to agent");

    (handle, socket_path, client, dir)
}

/// Build realistic request metadata.
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
        timestamp: Utc::now().to_rfc3339(),
        traceparent: None,
    }
}

/// Generate a minimal 4x4 JPEG test image.
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

/// Generate a minimal 4x4 PNG test image.
fn test_png() -> Vec<u8> {
    let img = image::RgbImage::from_fn(4, 4, |x, y| {
        if (x + y) % 2 == 0 {
            image::Rgb([0, 255, 0])
        } else {
            image::Rgb([255, 255, 0])
        }
    });
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
    buf.into_inner()
}

/// Config with caching disabled (used by most tests).
fn config_no_cache() -> ImageOptConfig {
    let mut config = ImageOptConfig::default();
    config.cache.enabled = false;
    config
}

/// Check that response headers contain a Set with the given name and value.
fn has_header(headers: &[HeaderOp], name: &str, value: &str) -> bool {
    headers
        .iter()
        .any(|h| matches!(h, HeaderOp::Set { name: n, value: v } if n == name && v == value))
}

/// Check that response headers contain a Set with the given name (any value).
fn has_header_name(headers: &[HeaderOp], name: &str) -> bool {
    headers
        .iter()
        .any(|h| matches!(h, HeaderOp::Set { name: n, .. } if n == name))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jpeg_to_webp_conversion() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "jpeg-webp";

    // 1. Request headers — client supports WebP
    let mut headers = HashMap::new();
    headers.insert(
        "accept".to_string(),
        vec!["image/webp, image/jpeg".to_string()],
    );
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/cat.jpg".to_string(),
        headers,
    };
    let resp = client.send_request_headers(cid, &event).await.unwrap();
    assert_eq!(resp.decision, Decision::Allow);

    // 2. Response headers — origin sends JPEG
    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    let resp = client.send_response_headers(cid, &event).await.unwrap();
    assert!(resp
        .response_headers
        .iter()
        .any(|h| matches!(h, HeaderOp::Remove { name } if name == "content-length")));

    // 3. Response body — single JPEG chunk
    let jpeg_data = test_jpeg();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    // Verify body contains valid WebP
    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(!mutation.is_pass_through());
    assert!(!mutation.is_drop());
    let body = B64
        .decode(mutation.data.expect("expected body data"))
        .unwrap();
    assert_eq!(&body[0..4], b"RIFF", "WebP must start with RIFF");
    assert_eq!(&body[8..12], b"WEBP", "WebP RIFF subtype must be WEBP");

    // Verify response headers
    assert!(has_header(
        &resp.response_headers,
        "content-type",
        "image/webp"
    ));
    assert!(has_header(
        &resp.response_headers,
        "x-image-optimized",
        "webp"
    ));
    assert!(has_header(&resp.response_headers, "vary", "Accept"));
    assert!(has_header_name(
        &resp.response_headers,
        "x-image-original-size"
    ));
    assert!(has_header_name(&resp.response_headers, "content-length"));

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn png_to_webp_conversion() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "png-webp";

    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/icons/logo.png".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/png".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let png_data = test_png();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&png_data),
        is_last: true,
        total_size: Some(png_data.len()),
        chunk_index: 0,
        bytes_sent: png_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    let mutation = resp.response_body_mutation.expect("expected body mutation");
    let body = B64
        .decode(mutation.data.expect("expected body data"))
        .unwrap();
    assert_eq!(&body[0..4], b"RIFF");
    assert_eq!(&body[8..12], b"WEBP");
    assert!(has_header(
        &resp.response_headers,
        "content-type",
        "image/webp"
    ));
    assert!(has_header(
        &resp.response_headers,
        "x-image-optimized",
        "webp"
    ));

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jpeg_to_avif_conversion() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "jpeg-avif";

    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/avif".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/sunset.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let jpeg_data = test_jpeg();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    let mutation = resp.response_body_mutation.expect("expected body mutation");
    let body = B64
        .decode(mutation.data.expect("expected body data"))
        .unwrap();
    // AVIF: ftyp box at bytes 4-7
    assert_eq!(&body[4..8], b"ftyp", "AVIF must contain ftyp box");
    assert!(has_header(
        &resp.response_headers,
        "content-type",
        "image/avif"
    ));
    assert!(has_header(
        &resp.response_headers,
        "x-image-optimized",
        "avif"
    ));

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_hit_on_second_request() {
    let cache_dir = tempdir().expect("failed to create cache dir");
    let mut config = ImageOptConfig::default();
    config.cache.enabled = true;
    config.cache.directory = cache_dir.path().to_str().unwrap().to_string();

    let (handle, _socket_path, client, _dir) = start_agent(config).await;

    let uri = "/photos/cached.jpg";
    let jpeg_data = test_jpeg();

    // ── First request (cache miss → conversion) ──
    let cid1 = "cache-1";
    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid1),
        method: "GET".to_string(),
        uri: uri.to_string(),
        headers,
    };
    client.send_request_headers(cid1, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid1.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid1, &event).await.unwrap();

    let event = ResponseBodyChunkEvent {
        correlation_id: cid1.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp1 = client.send_response_body_chunk(cid1, &event).await.unwrap();

    let mutation1 = resp1
        .response_body_mutation
        .expect("expected body mutation");
    let first_image = B64.decode(mutation1.data.expect("expected data")).unwrap();
    assert!(has_header(
        &resp1.response_headers,
        "x-image-optimized",
        "webp"
    ));

    // Clean up first request state
    let event = RequestCompleteEvent {
        correlation_id: cid1.to_string(),
        status: 200,
        duration_ms: 50,
        request_body_size: 0,
        response_body_size: first_image.len(),
        upstream_attempts: 1,
        error: None,
    };
    client.send_request_complete(cid1, &event).await.unwrap();

    // ── Second request (cache hit) ──
    let cid2 = "cache-2";
    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid2),
        method: "GET".to_string(),
        uri: uri.to_string(),
        headers,
    };
    client.send_request_headers(cid2, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid2.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid2, &event).await.unwrap();

    let event = ResponseBodyChunkEvent {
        correlation_id: cid2.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp2 = client.send_response_body_chunk(cid2, &event).await.unwrap();

    // Should be a cache hit
    assert!(has_header(
        &resp2.response_headers,
        "x-image-optimized",
        "cache-hit"
    ));

    let mutation2 = resp2
        .response_body_mutation
        .expect("expected body mutation");
    let second_image = B64.decode(mutation2.data.expect("expected data")).unwrap();

    // Both responses should produce identical image bytes
    assert_eq!(
        first_image, second_image,
        "cached image should match first conversion"
    );

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ineligible_content_type_passes_through() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "ineligible-ct";

    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp, */*".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/api/data.json".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert(
        "content-type".to_string(),
        vec!["application/json".to_string()],
    );
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(b"{}"),
        is_last: true,
        total_size: Some(2),
        chunk_index: 0,
        bytes_sent: 2,
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        mutation.is_pass_through(),
        "non-image content should pass through"
    );
    assert!(!has_header_name(
        &resp.response_headers,
        "x-image-optimized"
    ));

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn client_without_webp_support_passes_through() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "no-webp";

    let mut headers = HashMap::new();
    headers.insert(
        "accept".to_string(),
        vec!["image/jpeg, image/png".to_string()],
    );
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/cat.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let jpeg_data = test_jpeg();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        mutation.is_pass_through(),
        "should pass through without supported format"
    );

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_200_status_passes_through() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "non-200";

    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/cat.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 304,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(b""),
        is_last: true,
        total_size: Some(0),
        chunk_index: 0,
        bytes_sent: 0,
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(mutation.is_pass_through(), "non-200 should pass through");

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn passthrough_pattern_skips_conversion() {
    let mut config = config_no_cache();
    config.passthrough_patterns = vec![r"\.gif$".to_string()];

    let (handle, _socket_path, client, _dir) = start_agent(config).await;
    let cid = "passthrough";

    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp, */*".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/images/animation.gif".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(b"GIF89a..."),
        is_last: true,
        total_size: Some(9),
        chunk_index: 0,
        bytes_sent: 9,
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        mutation.is_pass_through(),
        "passthrough pattern should skip conversion"
    );

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupt_image_graceful_fallback() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "corrupt";

    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/bad.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let corrupt = b"this is not a valid jpeg image";
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(corrupt),
        is_last: true,
        total_size: Some(corrupt.len()),
        chunk_index: 0,
        bytes_sent: corrupt.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    // Should return original bytes (replace mutation, not pass-through)
    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(!mutation.is_pass_through());
    assert!(!mutation.is_drop());
    let returned = B64.decode(mutation.data.expect("expected data")).unwrap();
    assert_eq!(returned, corrupt, "should fall back to original bytes");

    // No conversion headers
    assert!(!has_header_name(
        &resp.response_headers,
        "x-image-optimized"
    ));

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_chunk_body_assembly() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "multi-chunk";

    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/big.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    // Split JPEG into 3 chunks
    let jpeg_data = test_jpeg();
    let chunk_size = jpeg_data.len() / 3;
    let chunks: Vec<&[u8]> = vec![
        &jpeg_data[..chunk_size],
        &jpeg_data[chunk_size..chunk_size * 2],
        &jpeg_data[chunk_size * 2..],
    ];

    // Chunk 0 — not last
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(chunks[0]),
        is_last: false,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: chunks[0].len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();
    assert!(resp.needs_more, "chunk 0 should request more data");
    let m = resp.response_body_mutation.expect("expected mutation");
    assert!(m.is_drop(), "non-last chunk should be dropped");

    // Chunk 1 — not last
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(chunks[1]),
        is_last: false,
        total_size: Some(jpeg_data.len()),
        chunk_index: 1,
        bytes_sent: chunks[0].len() + chunks[1].len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();
    assert!(resp.needs_more, "chunk 1 should request more data");
    let m = resp.response_body_mutation.expect("expected mutation");
    assert!(m.is_drop(), "non-last chunk should be dropped");

    // Chunk 2 — last
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(chunks[2]),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 2,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();
    assert!(!resp.needs_more, "last chunk should not request more");

    // Verify converted output
    let mutation = resp.response_body_mutation.expect("expected body mutation");
    let body = B64.decode(mutation.data.expect("expected data")).unwrap();
    assert_eq!(&body[0..4], b"RIFF");
    assert_eq!(&body[8..12], b"WEBP");
    assert!(has_header(
        &resp.response_headers,
        "content-type",
        "image/webp"
    ));
    assert!(has_header(
        &resp.response_headers,
        "x-image-optimized",
        "webp"
    ));

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests() {
    let (handle, _socket_path, _client, _dir) = start_agent(config_no_cache()).await;

    let mut tasks = Vec::new();
    for i in 0..5u32 {
        let path = _socket_path.clone();
        tasks.push(tokio::spawn(async move {
            let client = AgentClientV2Uds::new(
                &format!("e2e-client-{}", i),
                path.to_string_lossy().to_string(),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
            client.connect().await.unwrap();
            let cid = format!("concurrent-{}", i);

            let mut headers = HashMap::new();
            headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
            let event = RequestHeadersEvent {
                metadata: test_metadata(&cid),
                method: "GET".to_string(),
                uri: format!("/img/{}.jpg", i),
                headers,
            };
            client.send_request_headers(&cid, &event).await.unwrap();

            let mut resp_headers = HashMap::new();
            resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
            let event = ResponseHeadersEvent {
                correlation_id: cid.clone(),
                status: 200,
                headers: resp_headers,
            };
            client.send_response_headers(&cid, &event).await.unwrap();

            let jpeg_data = test_jpeg();
            let event = ResponseBodyChunkEvent {
                correlation_id: cid.clone(),
                data: B64.encode(&jpeg_data),
                is_last: true,
                total_size: Some(jpeg_data.len()),
                chunk_index: 0,
                bytes_sent: jpeg_data.len(),
            };
            let resp = client.send_response_body_chunk(&cid, &event).await.unwrap();

            client.close().await.unwrap();
            resp
        }));
    }

    for task in tasks {
        let resp = task.await.expect("task should not panic");
        assert_eq!(resp.decision, Decision::Allow);
        let mutation = resp.response_body_mutation.expect("expected body mutation");
        assert!(!mutation.is_pass_through());
        assert!(!mutation.is_drop());
        let body = B64.decode(mutation.data.expect("expected data")).unwrap();
        assert_eq!(&body[0..4], b"RIFF");
        assert_eq!(&body[8..12], b"WEBP");
    }

    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_complete_cleanup() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "cleanup";

    // Full request cycle
    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/cat.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let jpeg_data = test_jpeg();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    client.send_response_body_chunk(cid, &event).await.unwrap();

    // Complete the request — state should be cleaned up
    let event = RequestCompleteEvent {
        correlation_id: cid.to_string(),
        status: 200,
        duration_ms: 50,
        request_body_size: 0,
        response_body_size: 1000,
        upstream_attempts: 1,
        error: None,
    };
    client.send_request_complete(cid, &event).await.unwrap();

    // Send another body chunk with the same correlation_id — should get pass-through
    // since state was cleaned up
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(b"stale-data"),
        is_last: true,
        total_size: Some(10),
        chunk_index: 0,
        bytes_sent: 10,
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        mutation.is_pass_through(),
        "after cleanup, body should pass through (no state found)"
    );

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buffer_overflow_passes_through() {
    let mut config = config_no_cache();
    config.max_input_size_bytes = 50; // tiny limit
    let (handle, _socket_path, client, _dir) = start_agent(config).await;
    let cid = "buf-overflow";

    // Request headers — eligible JPEG flow
    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/big.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    // Response headers — origin sends JPEG
    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    // Send body chunk > 50 bytes (test JPEG is ~600 bytes)
    let jpeg_data = test_jpeg();
    assert!(jpeg_data.len() > 50, "test JPEG must exceed 50 byte limit");
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        mutation.is_pass_through(),
        "buffer overflow should pass through"
    );
    assert!(
        !has_header_name(&resp.response_headers, "x-image-optimized"),
        "no optimization header on overflow"
    );

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn base64_decode_failure_passes_through() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;
    let cid = "bad-b64";

    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/cat.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    // Send invalid base64 data directly (not encoded via B64.encode)
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: "!!!not-valid-base64!!!".to_string(),
        is_last: true,
        total_size: Some(21),
        chunk_index: 0,
        bytes_sent: 21,
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        mutation.is_pass_through(),
        "base64 decode failure should pass through"
    );

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configure_valid_update_changes_behavior() {
    // Start with default config (formats=[WebP, Avif])
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;

    // Reconfigure to only support WebP
    let cid_cfg = "configure-0";
    let config_payload = serde_json::json!({
        "correlation_id": cid_cfg,
        "config": serde_json::json!({
            "formats": ["webp"],
            "cache": { "enabled": false }
        }),
        "config_version": "2",
    });
    let resp = client
        .send_configure(cid_cfg, &config_payload)
        .await
        .unwrap();
    assert_eq!(resp.decision, Decision::Allow, "valid config should Allow");

    // Request with Accept: image/avif — should pass through (AVIF removed)
    let cid_avif = "cfg-avif";
    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/avif".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid_avif),
        method: "GET".to_string(),
        uri: "/photos/a.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid_avif, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid_avif.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client
        .send_response_headers(cid_avif, &event)
        .await
        .unwrap();

    let jpeg_data = test_jpeg();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid_avif.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client
        .send_response_body_chunk(cid_avif, &event)
        .await
        .unwrap();
    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        mutation.is_pass_through(),
        "AVIF should pass through after removing it from formats"
    );

    // Request with Accept: image/webp — should still convert
    let cid_webp = "cfg-webp";
    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid_webp),
        method: "GET".to_string(),
        uri: "/photos/b.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid_webp, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid_webp.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client
        .send_response_headers(cid_webp, &event)
        .await
        .unwrap();

    let event = ResponseBodyChunkEvent {
        correlation_id: cid_webp.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client
        .send_response_body_chunk(cid_webp, &event)
        .await
        .unwrap();
    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(!mutation.is_pass_through(), "WebP should still convert");
    let body = B64
        .decode(mutation.data.expect("expected body data"))
        .unwrap();
    assert_eq!(&body[0..4], b"RIFF");
    assert_eq!(&body[8..12], b"WEBP");
    assert!(has_header(
        &resp.response_headers,
        "x-image-optimized",
        "webp"
    ));

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configure_invalid_config_rejects() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;

    // Send invalid config: empty formats list fails validation
    let cid_cfg = "configure-0";
    let config_payload = serde_json::json!({
        "correlation_id": cid_cfg,
        "config": serde_json::json!({
            "formats": [],
            "cache": { "enabled": false }
        }),
        "config_version": "2",
    });
    let resp = client
        .send_configure(cid_cfg, &config_payload)
        .await
        .unwrap();
    assert!(
        matches!(resp.decision, Decision::Block { status: 500, .. }),
        "empty formats should be rejected with Block 500, got {:?}",
        resp.decision
    );

    // Original config should still work — send a normal WebP conversion
    let cid = "cfg-reject";
    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/still-works.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let jpeg_data = test_jpeg();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();
    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        !mutation.is_pass_through(),
        "conversion should still work after rejected config"
    );
    assert!(has_header(
        &resp.response_headers,
        "x-image-optimized",
        "webp"
    ));

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configure_malformed_json_preserves_config() {
    let (handle, _socket_path, client, _dir) = start_agent(config_no_cache()).await;

    // Send malformed config: formats is a string instead of an array (type mismatch)
    let cid_cfg = "configure-0";
    let config_payload = serde_json::json!({
        "correlation_id": cid_cfg,
        "config": serde_json::json!({
            "formats": "not-an-array"
        }),
        "config_version": "2",
    });
    let resp = client
        .send_configure(cid_cfg, &config_payload)
        .await
        .unwrap();
    assert_eq!(
        resp.decision,
        Decision::Allow,
        "malformed config should Allow (parse failure keeps existing config)"
    );

    // Original config should still work
    let cid = "cfg-malformed";
    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/still-works.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let jpeg_data = test_jpeg();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();
    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        !mutation.is_pass_through(),
        "conversion should still work after malformed config"
    );
    assert!(has_header(
        &resp.response_headers,
        "x-image-optimized",
        "webp"
    ));

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_write_failure_still_converts() {
    use std::os::unix::fs::PermissionsExt;

    let cache_dir = tempdir().expect("failed to create cache dir");
    let mut config = ImageOptConfig::default();
    config.cache.enabled = true;
    config.cache.directory = cache_dir.path().to_str().unwrap().to_string();

    let (handle, _socket_path, client, _dir) = start_agent(config).await;

    // Make cache dir read-only to trigger write failures
    std::fs::set_permissions(cache_dir.path(), std::fs::Permissions::from_mode(0o444)).unwrap();

    let cid = "cache-write-fail";
    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/cat.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let jpeg_data = test_jpeg();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    // Conversion should still succeed despite cache write failure
    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(!mutation.is_pass_through());
    assert!(!mutation.is_drop());
    let body = B64
        .decode(mutation.data.expect("expected body data"))
        .unwrap();
    assert_eq!(&body[0..4], b"RIFF", "output should be valid WebP");
    assert_eq!(&body[8..12], b"WEBP");
    assert!(has_header(
        &resp.response_headers,
        "x-image-optimized",
        "webp"
    ));

    // Restore permissions for cleanup
    std::fs::set_permissions(cache_dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    client.close().await.unwrap();
    handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_pixel_count_fallback() {
    let mut config = config_no_cache();
    config.max_pixel_count = 10; // test JPEG is 4x4 = 16 pixels > 10

    let (handle, _socket_path, client, _dir) = start_agent(config).await;
    let cid = "pixel-limit";

    let mut headers = HashMap::new();
    headers.insert("accept".to_string(), vec!["image/webp".to_string()]);
    let event = RequestHeadersEvent {
        metadata: test_metadata(cid),
        method: "GET".to_string(),
        uri: "/photos/big.jpg".to_string(),
        headers,
    };
    client.send_request_headers(cid, &event).await.unwrap();

    let mut resp_headers = HashMap::new();
    resp_headers.insert("content-type".to_string(), vec!["image/jpeg".to_string()]);
    let event = ResponseHeadersEvent {
        correlation_id: cid.to_string(),
        status: 200,
        headers: resp_headers,
    };
    client.send_response_headers(cid, &event).await.unwrap();

    let jpeg_data = test_jpeg();
    let event = ResponseBodyChunkEvent {
        correlation_id: cid.to_string(),
        data: B64.encode(&jpeg_data),
        is_last: true,
        total_size: Some(jpeg_data.len()),
        chunk_index: 0,
        bytes_sent: jpeg_data.len(),
    };
    let resp = client.send_response_body_chunk(cid, &event).await.unwrap();

    // Converter fails with ImageTooLarge → handler returns original bytes via replace
    let mutation = resp.response_body_mutation.expect("expected body mutation");
    assert!(
        !mutation.is_pass_through(),
        "should be a replace mutation, not pass-through"
    );
    assert!(!mutation.is_drop());
    let returned = B64
        .decode(mutation.data.expect("expected body data"))
        .unwrap();
    assert_eq!(
        returned, jpeg_data,
        "should fall back to original JPEG bytes"
    );
    assert!(
        !has_header_name(&resp.response_headers, "x-image-optimized"),
        "no optimization header on pixel count fallback"
    );

    client.close().await.unwrap();
    handle.abort();
}
