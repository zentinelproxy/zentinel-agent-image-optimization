//! Image Optimization Agent for Zentinel
//!
//! This agent converts JPEG/PNG responses to WebP/AVIF on the fly,
//! caches the results, and serves optimized images on subsequent requests.

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use tracing::info;

use zentinel_agent_image_optimization::{ImageOptAgent, ImageOptConfig};
use zentinel_agent_protocol::{AgentServer, GrpcAgentServer};

/// Image Optimization Agent command-line arguments.
#[derive(Parser, Debug)]
#[command(
    name = "zentinel-image-optimization-agent",
    author,
    version,
    about = "Image optimization agent for Zentinel - on-the-fly WebP/AVIF conversion with caching"
)]
struct Args {
    /// Unix socket path to listen on (mutually exclusive with --grpc).
    #[arg(short, long, env = "IMAGE_OPT_SOCKET", conflicts_with = "grpc")]
    socket: Option<PathBuf>,

    /// gRPC address to listen on (e.g., "0.0.0.0:50060").
    #[arg(short, long, env = "IMAGE_OPT_GRPC", conflicts_with = "socket")]
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

    // Determine transport mode
    match (&args.socket, &args.grpc) {
        (Some(socket), None) => {
            info!(
                version = env!("CARGO_PKG_VERSION"),
                socket = ?socket,
                "Starting image optimization agent (Unix socket)"
            );

            let server = AgentServer::new("image-optimization-agent", socket, agent);

            info!("Image optimization agent ready and listening on Unix socket");

            server
                .run()
                .await
                .context("Failed to run image optimization agent server")?;
        }
        (None, Some(grpc_addr)) => {
            info!(
                version = env!("CARGO_PKG_VERSION"),
                grpc = %grpc_addr,
                "Starting image optimization agent (gRPC)"
            );

            let server = GrpcAgentServer::new("image-optimization-agent", agent);
            let addr = grpc_addr
                .parse()
                .context("Invalid gRPC address format (expected host:port)")?;

            info!("Image optimization agent ready and listening on gRPC");

            server
                .run(addr)
                .await
                .context("Failed to run image optimization agent gRPC server")?;
        }
        (None, None) => {
            // Default to Unix socket if neither specified
            let socket = PathBuf::from("/tmp/image-optimization-agent.sock");
            info!(
                version = env!("CARGO_PKG_VERSION"),
                socket = ?socket,
                "Starting image optimization agent (Unix socket, default)"
            );

            let server = AgentServer::new("image-optimization-agent", socket, agent);

            info!("Image optimization agent ready and listening on Unix socket");

            server
                .run()
                .await
                .context("Failed to run image optimization agent server")?;
        }
        (Some(_), Some(_)) => {
            // This shouldn't happen due to clap's conflicts_with
            unreachable!("Cannot specify both --socket and --grpc");
        }
    }

    Ok(())
}
