//! The proxy sends the agent's `config { }` block on every connection (each
//! pooled connection at startup, and again on every reconnect). These tests
//! pin down what applying it repeatedly or partially must do.

use std::sync::Arc;

use serde_json::json;
use tempfile::tempdir;
use zentinel_agent_image_optimization::config::OutputFormat;
use zentinel_agent_image_optimization::{ConfigureOutcome, ImageOptAgent, ImageOptConfig};
use zentinel_agent_protocol::v2::AgentHandlerV2;

/// Puts a 1000-byte entry into the cache directory behind the agent's back,
/// so a rescan of the directory shows up in the cache's size counter.
fn plant_entry(dir: &std::path::Path) {
    let shard = dir.join("ab").join("cd");
    std::fs::create_dir_all(&shard).unwrap();
    std::fs::write(shard.join("abcdef.bin"), vec![0u8; 1000]).unwrap();
}

#[tokio::test]
async fn identical_config_twice_does_not_rebuild_the_cache() {
    let dir = tempdir().unwrap();
    let mut config = ImageOptConfig::default();
    config.cache.directory = dir.path().to_string_lossy().into_owned();
    let agent = ImageOptAgent::new(config).await.unwrap();
    assert_eq!(agent.cache_size_bytes().await, Some(0));

    plant_entry(dir.path());

    let block = json!({"quality": {"avif": 60}});
    assert!(agent.on_configure(block.clone(), None).await);
    // `cache` did not change, so the cache was kept, not rebuilt and rescanned.
    assert_eq!(agent.cache_size_bytes().await, Some(0));

    // The proxy reconnects and sends the same block again.
    assert!(agent.on_configure(block.clone(), None).await);
    assert_eq!(
        agent.apply_config(&block).await.unwrap(),
        ConfigureOutcome::Unchanged
    );
    assert_eq!(
        agent.cache_size_bytes().await,
        Some(0),
        "a repeated configure rebuilt the cache"
    );

    // Changing a cache setting does rebuild it (and so rescans the directory).
    assert!(
        agent
            .on_configure(json!({"cache": {"ttl_secs": 60}}), None)
            .await
    );
    assert_eq!(agent.cache_size_bytes().await, Some(1000));
}

#[tokio::test]
async fn concurrent_identical_configures_apply_once() {
    let mut config = ImageOptConfig::default();
    config.cache.enabled = false;
    let agent = Arc::new(ImageOptAgent::new(config).await.unwrap());
    let block = json!({"max_pixel_count": 1234});

    let tasks: Vec<_> = (0..4)
        .map(|_| {
            let agent = agent.clone();
            let block = block.clone();
            tokio::spawn(async move { agent.apply_config(&block).await.unwrap() })
        })
        .collect();
    let mut applied = 0;
    for t in tasks {
        if matches!(t.await.unwrap(), ConfigureOutcome::Applied { .. }) {
            applied += 1;
        }
    }
    assert_eq!(applied, 1);
}

#[tokio::test]
async fn partial_config_keeps_file_values() {
    let mut config = ImageOptConfig::default();
    config.cache.enabled = false;
    config.formats = vec![OutputFormat::WebP];
    config.max_pixel_count = 1000;
    config.quality.webp = 90;
    let agent = ImageOptAgent::new(config).await.unwrap();

    assert!(
        agent
            .on_configure(json!({"quality": {"avif": 50}}), None)
            .await
    );
    let active = agent.active_config().await;
    assert_eq!(active["quality"]["avif"], json!(50));
    assert_eq!(active["quality"]["webp"], json!(90));
    assert_eq!(active["formats"], json!(["webp"]));
    assert_eq!(active["max_pixel_count"], json!(1000));
    assert_eq!(active["cache"]["enabled"], json!(false));
}

#[tokio::test]
async fn single_item_list_arrives_as_a_scalar() {
    // `formats "avif"` in KDL reaches the agent as a string.
    let mut config = ImageOptConfig::default();
    config.cache.enabled = false;
    let agent = ImageOptAgent::new(config).await.unwrap();
    assert!(agent.on_configure(json!({"formats": "avif"}), None).await);
    assert_eq!(agent.active_config().await["formats"], json!(["avif"]));
}

#[tokio::test]
async fn bad_configs_are_rejected_and_change_nothing() {
    let mut config = ImageOptConfig::default();
    config.cache.enabled = false;
    let agent = ImageOptAgent::new(config).await.unwrap();
    let before = agent.active_config().await;

    for bad in [
        json!({"max_pixel_cont": 5}),
        json!({"cache": {"ttl": 5}}),
        json!({"formats": ["gif"]}),
        json!({"formats": []}),
        json!({"quality": {"avif": 0}}),
        json!({"passthrough_patterns": "[unclosed"}),
        json!("not an object"),
    ] {
        assert!(
            !agent.on_configure(bad.clone(), None).await,
            "accepted {bad}"
        );
    }
    assert_eq!(agent.active_config().await, before);
}
