<p align="center">
  <a href="https://zentinelproxy.io">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/zentinelproxy/design/main/marks/lockup/zentinel-lockup-dark.svg">
      <img alt="Zentinel" src="https://raw.githubusercontent.com/zentinelproxy/design/main/marks/lockup/zentinel-lockup-light.svg" height="52">
    </picture>
  </a>
</p>

<h3 align="center">Image optimization agent</h3>

<p align="center">Converts JPEG and PNG responses to WebP or AVIF in the response phase, chosen from the client's <code>Accept</code> header, and caches the result on disk.</p>

<p align="center">
  <a href="https://github.com/zentinelproxy/zentinel-agent-image-optimization/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/zentinelproxy/zentinel-agent-image-optimization/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-9b4d8a?style=flat&labelColor=1f161e">
  <img alt="Agent protocol: v2" src="https://img.shields.io/badge/agent%20protocol-v2-f68d31?style=flat&labelColor=1f161e">
</p>

<p align="center">
  <a href="https://registry.zentinelproxy.io/image-optimization/">Registry</a> &middot;
  <a href="https://docs.zentinelproxy.io">Docs</a> &middot;
  <a href="https://github.com/zentinelproxy">All repositories</a>
</p>

The agent reads the client's `Accept` header on the request, checks the
upstream's `Content-Type` on the response, and, when the client accepts one of
the configured formats, buffers the image body, converts it and hands the converted bytes back to
the proxy. Results are cached in a content-addressable directory, keyed by URI,
format and quality. Images that cannot be decoded or encoded, or that exceed the
size limits, pass through unchanged. Run it with `failure-mode "open"`, so a
crashed or slow agent costs you the optimization, not the image.

## Quick start

```bash
zentinel bundle install image-optimization
zentinel-image-optimization-agent --socket /var/run/zentinel/image-optimization.sock
```

```kdl
system {
    worker-threads 0
}

listeners {
    listener "http" {
        address "127.0.0.1:8080"
        protocol "http"
    }
}

agents {
    agent "image-optimization" type="custom" {
        unix-socket "/var/run/zentinel/image-optimization.sock"
        events "request_headers" "response_headers" "response_body"
        timeout-ms 5000
        failure-mode "open"
        response-body-mode "buffer"
        max-response-body-bytes 10485760
    }
}

filters {
    filter "image-optimization" {
        type "agent"
        agent "image-optimization"
        failure-mode "open"
    }
}

routes {
    route "images" {
        matches {
            path-prefix "/images/"
        }
        upstream "backend"
        filters "image-optimization"
    }
}

upstreams {
    upstream "backend" {
        target "127.0.0.1:3000"
    }
}
```

## Install

**Bundle (recommended).** Downloads the release binary for your platform:

```bash
zentinel bundle install image-optimization
```

See [the bundle documentation](https://docs.zentinelproxy.io/deployment/bundle/).

**Release binaries.** Each [release](https://github.com/zentinelproxy/zentinel-agent-image-optimization/releases)
ships `linux-x86_64`, `linux-aarch64` and `darwin-aarch64` archives:

```bash
VERSION=0.2.0
PLATFORM=linux-x86_64   # or linux-aarch64, darwin-aarch64
curl -fsSL -o zentinel-image-optimization-agent.tar.gz \
  "https://github.com/zentinelproxy/zentinel-agent-image-optimization/releases/download/v${VERSION}/zentinel-image-optimization-agent-${VERSION}-${PLATFORM}.tar.gz"
tar -xzf zentinel-image-optimization-agent.tar.gz
sudo install -m 0755 zentinel-image-optimization-agent /usr/local/bin/
```

**From source.** The crate takes `zentinel-agent-protocol` as a path dependency
on a sibling `zentinel` checkout, so `cargo install --git` does not work, and the
crate is not on crates.io:

```bash
git clone https://github.com/zentinelproxy/zentinel
git clone https://github.com/zentinelproxy/zentinel-agent-image-optimization
cd zentinel-agent-image-optimization
cargo build --release
```

A hardened systemd unit is in
[`zentinel-image-optimization-agent.service`](zentinel-image-optimization-agent.service);
it runs the agent on `/var/run/zentinel/image-optimization.sock` with
`--config /etc/zentinel/agents/image-optimization.json`.

## Configuration

Command-line flags:

| Flag | Env | Default | Meaning |
|---|---|---|---|
| `-s, --socket <PATH>` | `IMAGE_OPT_SOCKET` | `/tmp/image-optimization-agent.sock` (if neither transport is given) | Unix socket to listen on |
| `-g, --grpc <ADDR>` | `IMAGE_OPT_GRPC` | none | gRPC address, e.g. `0.0.0.0:50060`; excludes `--socket` |
| `-c, --config <FILE>` | `IMAGE_OPT_CONFIG` | built-in defaults | JSON configuration file (below) |
| `-l, --log-level <LEVEL>` | `IMAGE_OPT_LOG_LEVEL` | `info` | Log level; `RUST_LOG` takes precedence |

The JSON file ([`config/default.json`](config/default.json) has every key at its default):

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

| Option | Default | Meaning |
|---|---|---|
| `formats` | `["webp", "avif"]` | Output formats in priority order; must not be empty |
| `quality.avif` | `70` | AVIF quality, 1 to 100 |
| `quality.webp` | `80` | Validated (1 to 100) and part of the cache key, but not applied: WebP output is always lossless (VP8L) |
| `max_input_size_bytes` | `10485760` (10 MiB) | Larger images pass through |
| `max_pixel_count` | `25000000` | Images with more pixels pass through |
| `eligible_content_types` | `["image/jpeg", "image/png"]` | Response content types to convert |
| `passthrough_patterns` | `[]` | Regexes on the request URI to skip |
| `cache.enabled` | `true` | Enable the filesystem cache |
| `cache.directory` | `/var/cache/zentinel/image-optimization` | Cache root |
| `cache.max_size_bytes` | `1073741824` (1 GiB) | Size at which the oldest entries are evicted |
| `cache.ttl_secs` | `86400` | Age after which an entry counts as a miss |

A file that fails to parse, or has a key not listed here, stops the agent at
startup.

### The `config { }` block

Zentinel releases that include zentinelproxy/zentinel#603 send the agent
block's `config { }` to the agent on every connection: once per pooled
connection at startup and again on every reconnect. It uses the same keys as
the file, and the agent applies it with these rules:

- **Merge, don't replace.** The block is laid over the active settings (the
  file, or the defaults). A key the block leaves out keeps its value, and
  `quality { }` / `cache { }` only change the keys they name.
- **Idempotent.** If the result is the configuration already active, compared
  by a hash of its canonical JSON, nothing happens. When it differs, only the
  changed parts are rebuilt: the cache (which rescans its directory) only when
  a `cache` key changed, the converters only when `formats` changed. Requests
  in flight are not affected.
- **Strict.** An unknown or misspelled key, a value of the wrong type, or a
  value the checks above refuse (an empty `formats`, a quality outside 1 to
  100, an invalid regex) rejects the configure: the agent answers `false`, logs
  why and keeps its active configuration, and the proxy does not use that
  connection.
- **Not logged in full.** The log shows the config's hash and the names of the
  keys that changed.

A list with one item may be written as a single value:

```kdl
agent "image-optimization" type="custom" {
    unix-socket "/tmp/image-optimization-agent.sock"
    config {
        formats "webp"
        max_pixel_count 16000000
        quality {
            avif 60
        }
    }
}
```

Proxy-side options (`events`, `timeout-ms`, `failure-mode`, body modes):
<https://docs.zentinelproxy.io/configuration/agents/>. Agent reference:
<https://registry.zentinelproxy.io/image-optimization/>.

## How it works

```
Client                  Zentinel                    Image optimization agent
  │  GET /photo.jpg        │                               │
  │  Accept: image/webp    │  request_headers              │
  │───────────────────────►│──────────────────────────────►│  remember Accept + URI
  │                        │  response_headers             │
  │                        │──────────────────────────────►│  check Content-Type,
  │                        │◄──────────────────────────────│  negotiate, check cache,
  │                        │  (Content-Type, Vary set)     │  set response headers
  │                        │  response_body                │
  │                        │──────────────────────────────►│  convert (or cache hit),
  │                        │◄──────────────────────────────│  replace body, cache
  │  200 image/webp        │                               │
  │◄───────────────────────│                               │
```

- **Negotiation.** The first entry of `formats` that the `Accept` header
  accepts with a non-zero quality wins; quality values do not rank formats
  against each other, and `image/*` or `*/*` count as accepting. A request
  with no `Accept` header gets the first format.
  Clients that accept none of the formats get the original.
- **Headers.** The agent sets `Content-Type` to the target format and
  `Vary: Accept`, and removes `Content-Length`, in the response-headers phase,
  before conversion. If conversion then fails, the original bytes are sent with
  the new `Content-Type`.
- **Not sent to clients.** The handler also sets `X-Image-Optimized` and
  `X-Image-Original-Size` with the converted body, but the proxy does not apply
  header changes returned with a body chunk, so these headers do not reach the
  client today.
- **Encoders.** WebP is lossless, via the pure-Rust `image` crate. AVIF is lossy,
  via `ravif`, with `quality.avif`. Conversion runs on a blocking thread.
- **Cache.** Two-level sharded directories under `cache.directory`, a `.bin` file
  and a `.meta.json` sidecar per entry, named by the SHA-256 of
  `"{uri}:{format}:{quality}"`. Read errors count as misses; write errors are
  logged and the converted image is still served.

## Development

The build needs the `zentinel` repository checked out next to this one (CI
clones it to `../zentinel`), and AVIF encoding needs `nasm`.

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test
```

`tests/e2e-integration.sh` runs the agent behind a real proxy.

## Related

- [Registry page](https://registry.zentinelproxy.io/image-optimization/)
- [Agents configuration](https://docs.zentinelproxy.io/configuration/agents/)
- [Agent protocol v2](https://docs.zentinelproxy.io/agents/v2/)
- [Zentinel](https://github.com/zentinelproxy/zentinel)
- [Transform agent](https://github.com/zentinelproxy/zentinel-agent-transform), for header and body rewrites

## License

MIT OR Apache-2.0, see [`LICENSE-MIT`](LICENSE-MIT) or [`LICENSE-APACHE`](LICENSE-APACHE), at your option.

---

<p align="center">
  <sub>Part of <a href="https://zentinelproxy.io">Zentinel</a>, the security-first reverse proxy. The shisa guards the gate.</sub>
</p>
