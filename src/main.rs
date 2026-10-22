//! Image Optimization Agent for Zentinel
//!
//! This agent converts JPEG/PNG responses to WebP/AVIF on the fly,
//! caches the results, and serves optimized images on subsequent requests.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use tracing::info;

use zentinel_agent_image_optimization::{ImageOptAgent, ImageOptConfig};
use zentinel_agent_protocol::v2::{GrpcAgentServerV2, UdsAgentServerV2};

/// Image Optimization Agent command-line arguments.
#[derive(Parser, Debug)]
#[command(
    name = "zentinel-image-optimization-agent",
    author,
    version,
    about = "Image optimization agent for Zentinel - on-the-fly WebP/AVIF conversion with caching"
)]
struct Args {
    /// Unix socket to listen on (agent protocol v2 over UDS). Used unless
    /// `--grpc-address` is given.
    #[arg(
        short,
        long,
        default_value = "/tmp/image-optimization-agent.sock",
        env = "IMAGE_OPT_SOCKET"
    )]
    socket: PathBuf,

    /// gRPC address to listen on (e.g. "127.0.0.1:50060"). When given, the
    /// agent serves agent protocol v2 over gRPC instead of the Unix socket.
    /// `--grpc` is accepted as an alias.
    #[arg(
        short,
        long = "grpc-address",
        visible_alias = "grpc",
        env = "IMAGE_OPT_GRPC"
    )]
    grpc: Option<String>,

    /// Log level (trace, debug, info, warn, error).
    #[arg(short, long, env = "IMAGE_OPT_LOG_LEVEL", default_value = "info")]
    log_level: String,

    /// Path to configuration file (JSON).
    #[arg(short, long, env = "IMAGE_OPT_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse command-line arguments
    let args = Args::parse();

    // Initialize tracing
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&args.log_level));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .json()
        .init();

    // Load configuration
    let config = if let Some(ref config_path) = args.config {
        let content = std::fs::read_to_string(config_path)
            .with_context(|| format!("Failed to read config file: {:?}", config_path))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {:?}", config_path))?
    } else {
        ImageOptConfig::default()
    };

    // Create agent
    let agent = Box::new(
        ImageOptAgent::new(config)
            .await
            .context("Failed to create image optimization agent")?,
    );

    if let Some(grpc_addr) = &args.grpc {
        let addr = grpc_addr
            .parse()
            .context("Invalid gRPC address format (expected host:port)")?;
        info!(
            version = env!("CARGO_PKG_VERSION"),
            grpc = %grpc_addr,
            "Listening on gRPC (agent protocol v2)"
        );
        GrpcAgentServerV2::new("image-optimization-agent", agent)
            .run(addr)
            .await
            .context("Failed to run image optimization agent gRPC server")?;
    } else {
        info!(
            version = env!("CARGO_PKG_VERSION"),
            socket = %args.socket.display(),
            "Listening on Unix socket (agent protocol v2)"
        );
        UdsAgentServerV2::new("image-optimization-agent", &args.socket, agent)
            .run()
            .await
            .context("Failed to run image optimization agent server")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_names_the_binary() {
        let err = Args::try_parse_from(["x", "--version"]).unwrap_err();
        assert_eq!(
            err.to_string().trim(),
            format!(
                "zentinel-image-optimization-agent {}",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn transport_flags() {
        let args = Args::try_parse_from(["x"]).unwrap();
        assert!(args.grpc.is_none());
        assert_eq!(
            args.socket,
            PathBuf::from("/tmp/image-optimization-agent.sock")
        );
        for flag in ["--grpc-address", "--grpc"] {
            let args = Args::try_parse_from(["x", flag, "127.0.0.1:1"]).unwrap();
            assert_eq!(args.grpc.as_deref(), Some("127.0.0.1:1"));
        }
    }
}
