# Image Optimization Agent

On-the-fly JPEG/PNG to WebP/AVIF conversion agent for [Zentinel](https://zentinelproxy.io) reverse proxy. Negotiates the best format from the client's `Accept` header, converts images via `spawn_blocking`, and caches results in a content-addressable filesystem store. Falls back to the original image on any error — the agent never makes a response worse.

## Features

- **Format Negotiation**: Parses `Accept` header quality values (RFC 7231) to pick WebP or AVIF
- **WebP Conversion**: Lossless VP8L encoding via the `image` crate
- **AVIF Conversion**: Lossy encoding with configurable quality via `ravif`
- **Filesystem Cache**: Content-addressable, two-level directory sharding, LRU eviction, configurable TTL
- **Passthrough Patterns**: Regex-based URL exclusions (e.g. skip `.gif`, `.svg`)
- **Size Guards**: Max input bytes and max pixel count to avoid OOM on huge images
- **Graceful Degradation**: Decode errors, encode failures, oversized images, and unsupported clients all pass through the original

## Installation

```bash
cargo install zentinel-agent-image-optimization
```

Or build from source:

```bash
cargo build --release
```

## Quick Start

Run the agent with default settings:

```bash
zentinel-image-optimization-agent --socket /tmp/image-optimization.sock
```

Configure Zentinel to use the agent:

```kdl
agents {
    agent "image-optimization" {
        unix-socket "/tmp/image-optimization.sock"
        events "request_headers" "response_headers" "response_body" "request_complete"
        protocol-version "v2"
        timeout-ms 5000
        failure-mode "open"

        config {
            "formats" ["webp", "avif"]
            "quality" { "webp" 80; "avif" 70 }
            "max_input_size_bytes" 10485760
            "max_pixel_count" 25000000
            "eligible_content_types" ["image/jpeg", "image/png"]
            "passthrough_patterns" ["\\.gif$", "\\.svg$"]
            "cache" {
                "enabled" true
                "directory" "/var/cache/zentinel/image-optimization"
                "max_size_bytes" 1073741824
                "ttl_secs" 86400
            }
        }
    }
}
```

## Configuration

Pass a JSON config file with `--config`, or let the proxy send configuration via the `configure` event.

```json
{
  "formats": ["webp", "avif"],
  "quality": { "webp": 80, "avif": 70 },
  "max_input_size_bytes": 10485760,
  "max_pixel_count": 25000000,
  "eligible_content_types": ["image/jpeg", "image/png"],
  "passthrough_patterns": ["\\.gif$", "\\.svg$"],
  "cache": {
    "enabled": true,
    "directory": "/var/cache/zentinel/image-optimization",
    "max_size_bytes": 1073741824,
    "ttl_secs": 86400
  }
}
```

### Options

| Field | Default | Description |
|-------|---------|-------------|
| `formats` | `["webp", "avif"]` | Output formats in priority order |
| `quality.webp` | `80` | WebP quality (1–100) |
| `quality.avif` | `70` | AVIF quality (1–100) |
| `max_input_size_bytes` | `10485760` (10 MB) | Skip images larger than this |
| `max_pixel_count` | `25000000` (25 MP) | Skip images with more pixels than this |
| `eligible_content_types` | `["image/jpeg", "image/png"]` | Response content types to optimize |
| `passthrough_patterns` | `[]` | Regex patterns for URLs to skip |
| `cache.enabled` | `true` | Enable filesystem cache |
| `cache.directory` | `/var/cache/zentinel/image-optimization` | Cache root directory |
| `cache.max_size_bytes` | `1073741824` (1 GB) | Max total cache size (LRU eviction) |
| `cache.ttl_secs` | `86400` (24 h) | Time-to-live for cached entries |

## How It Works

```
Client                  Zentinel Proxy              Image Optimization Agent
  │                         │                               │
  │  GET /photo.jpg         │                               │
  │  Accept: image/webp     │                               │
  │────────────────────────►│  request_headers              │
  │                         │──────────────────────────────►│
  │                         │  (extract Accept + URI)       │
  │                         │◄──────────────────────────────│
  │                         │                               │
  │                         │  response_headers             │
  │                         │──────────────────────────────►│
  │                         │  (check content-type,         │
  │                         │   negotiate format,           │
  │                         │   check cache)                │
  │                         │◄──────────────────────────────│
  │                         │                               │
  │                         │  response_body (chunks)       │
  │                         │──────────────────────────────►│
  │                         │  (buffer → convert → cache)   │
  │                         │◄──────────────────────────────│
  │                         │                               │
  │  200 OK                 │                               │
  │  Content-Type: image/webp                               │
  │  X-Image-Optimized: webp                                │
  │◄────────────────────────│                               │
```

## Response Headers

On successful conversion, the agent sets these headers:

| Header | Value | Description |
|--------|-------|-------------|
| `Content-Type` | `image/webp` or `image/avif` | The optimized format |
| `Content-Length` | `<bytes>` | Optimized image size |
| `Vary` | `Accept` | Downstream caches vary by format |
| `X-Image-Optimized` | `webp`, `avif`, or `cache-hit` | Which format was served |
| `X-Image-Original-Size` | `<bytes>` | Original image size before conversion |

## Failure Modes

All failures result in pass-through of the original image:

| Scenario | Behavior |
|----------|----------|
| Corrupt image / decode error | Pass through original |
| Encode failure | Pass through original |
| Image too large (bytes or pixels) | Pass through original, skip processing |
| Client doesn't support WebP/AVIF | Pass through original (no conversion) |
| Cache write failure | Serve converted image, log error |
| Cache read failure | Treat as miss, convert normally |
| Config parse error | Reject config with 500 (operator error) |

The proxy-level `failure-mode "open"` setting ensures that if the agent process crashes or times out, the original response passes through unmodified.

## CLI Options

```
zentinel-image-optimization-agent [OPTIONS]

Options:
  -s, --socket <PATH>     Unix socket path [env: IMAGE_OPT_SOCKET]
  -g, --grpc <ADDR>       gRPC address (e.g. 0.0.0.0:50060) [env: IMAGE_OPT_GRPC]
  -c, --config <FILE>     Configuration file (JSON) [env: IMAGE_OPT_CONFIG]
  -l, --log-level <LEVEL> Log level [default: info] [env: IMAGE_OPT_LOG_LEVEL]
  -h, --help              Print help
  -V, --version           Print version
```

If neither `--socket` nor `--grpc` is specified, defaults to `/tmp/image-optimization-agent.sock`.

## Cache Layout

The filesystem cache uses content-addressable storage with two-level directory sharding:

```
/var/cache/zentinel/image-optimization/
├── a3/
│   └── 7f/
│       ├── a37f...c4e2.bin          # Optimized image bytes
│       └── a37f...c4e2.meta.json    # Metadata sidecar
└── b1/
    └── 02/
        ├── b102...9af1.bin
        └── b102...9af1.meta.json
```

Cache keys are SHA-256 hashes of `"{uri}:{format}:{quality}"`.

## License

MIT OR Apache-2.0
