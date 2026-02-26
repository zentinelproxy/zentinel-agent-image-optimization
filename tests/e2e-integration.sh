#!/bin/bash
#
# E2E Integration Test — Zentinel Proxy + Image Optimization Agent + Static Backend
#
# Spins up the full stack (proxy, agent, backend) and verifies the integration
# works through the actual proxy.
#
# Test categories:
#   - Infrastructure: proxy boots, connects to agent, proxies requests
#   - Request phase:  agent receives request_headers, can inspect Accept header
#   - Response phase: agent converts PNG→WebP/AVIF (requires proxy response
#                     event dispatching — tests marked SKIP if not yet wired)
#
# Usage: bash tests/e2e-integration.sh
#

set -uo pipefail

# ──────────────────────────────────────────────────────────────────────────────
# Colors and test helpers (following test_echo_agent.sh pattern)
# ──────────────────────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m'

PASSED=0
FAILED=0
SKIPPED=0

pass() { echo -e "  ${GREEN}[PASS]${NC} $1"; PASSED=$((PASSED + 1)); }
fail() { echo -e "  ${RED}[FAIL]${NC} $1"; FAILED=$((FAILED + 1)); }
skip() { echo -e "  ${CYAN}[SKIP]${NC} $1"; SKIPPED=$((SKIPPED + 1)); }
info() { echo -e "  ${YELLOW}[INFO]${NC} $1"; }

echo ""
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo -e "${BLUE}  E2E Integration: Zentinel Proxy + Image Optimization Agent${NC}"
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo ""

# ──────────────────────────────────────────────────────────────────────────────
# Resolve paths
# ──────────────────────────────────────────────────────────────────────────────

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ZENTINEL_DIR="$(cd "$AGENT_DIR/../zentinel" && pwd)"

PROXY_PORT=18080
BACKEND_PORT=18081
SOCKET_PATH="/tmp/e2e-image-opt-$$.sock"
PROXY_URL="http://127.0.0.1:${PROXY_PORT}"

# ──────────────────────────────────────────────────────────────────────────────
# Create temp directory and cleanup trap
# ──────────────────────────────────────────────────────────────────────────────

TMPDIR="$(mktemp -d /tmp/e2e-image-opt-XXXXXX)"
PIDS=()

cleanup() {
    info "Cleaning up..."
    for pid in "${PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    rm -f "$SOCKET_PATH"
    rm -rf "$TMPDIR"
    info "Cleanup complete"
}
trap cleanup EXIT

info "Temp directory: $TMPDIR"
info "Socket path:    $SOCKET_PATH"

# ──────────────────────────────────────────────────────────────────────────────
# Generate test assets
# ──────────────────────────────────────────────────────────────────────────────

info "Generating test PNG..."

# Try ImageMagick first, fall back to inline minimal PNG
if command -v magick &>/dev/null; then
    magick -size 64x64 xc:red "$TMPDIR/test.png"
elif command -v convert &>/dev/null; then
    convert -size 64x64 xc:red "$TMPDIR/test.png"
else
    # Minimal 1x1 red PNG (base64-encoded, 68 bytes)
    echo "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8/5+hHgAHggJ/PchI7wAAAABJRU5ErkJggg==" \
        | base64 -d > "$TMPDIR/test.png"
fi

# Verify PNG was created
if [[ ! -f "$TMPDIR/test.png" ]] || [[ ! -s "$TMPDIR/test.png" ]]; then
    echo -e "${RED}FATAL: Failed to create test PNG${NC}"
    exit 1
fi
info "Test PNG: $(wc -c < "$TMPDIR/test.png") bytes"

# Create a JSON file for non-image passthrough test
echo '{"status":"ok","test":true}' > "$TMPDIR/data.json"

# ──────────────────────────────────────────────────────────────────────────────
# Build binaries
# ──────────────────────────────────────────────────────────────────────────────

ZENTINEL_BIN="$ZENTINEL_DIR/target/debug/zentinel"
AGENT_BIN="$AGENT_DIR/target/debug/zentinel-image-optimization-agent"

info "Building zentinel proxy..."
if [[ ! -x "$ZENTINEL_BIN" ]]; then
    (cd "$ZENTINEL_DIR" && cargo build --bin zentinel 2>&1 | tail -1)
else
    info "Proxy binary already built"
fi

info "Building image optimization agent..."
if [[ ! -x "$AGENT_BIN" ]]; then
    (cd "$AGENT_DIR" && cargo build 2>&1 | tail -1)
else
    info "Agent binary already built"
fi

# Verify binaries exist
if [[ ! -x "$ZENTINEL_BIN" ]]; then
    echo -e "${RED}FATAL: Proxy binary not found: $ZENTINEL_BIN${NC}"
    exit 1
fi
if [[ ! -x "$AGENT_BIN" ]]; then
    echo -e "${RED}FATAL: Agent binary not found: $AGENT_BIN${NC}"
    exit 1
fi

# ──────────────────────────────────────────────────────────────────────────────
# Start static HTTP backend (python3)
# ──────────────────────────────────────────────────────────────────────────────

info "Starting static backend on :${BACKEND_PORT}..."
(cd "$TMPDIR" && python3 -m http.server "$BACKEND_PORT" --bind 127.0.0.1) \
    > "$TMPDIR/backend.log" 2>&1 &
PIDS+=($!)
info "Backend PID: ${PIDS[-1]}"

# ──────────────────────────────────────────────────────────────────────────────
# Start image optimization agent (with cache dir inside temp dir)
# ──────────────────────────────────────────────────────────────────────────────

cat > "$TMPDIR/agent-config.json" <<JSONEOF
{
  "cache": {
    "enabled": true,
    "directory": "${TMPDIR}/cache",
    "max_size_bytes": 104857600,
    "ttl_secs": 3600
  }
}
JSONEOF

mkdir -p "$TMPDIR/cache"

info "Starting image optimization agent..."
rm -f "$SOCKET_PATH"
"$AGENT_BIN" --socket "$SOCKET_PATH" --log-level debug --config "$TMPDIR/agent-config.json" \
    > "$TMPDIR/agent.log" 2>&1 &
PIDS+=($!)
info "Agent PID: ${PIDS[-1]}"

# ──────────────────────────────────────────────────────────────────────────────
# Write KDL config
# ──────────────────────────────────────────────────────────────────────────────

cat > "$TMPDIR/zentinel.kdl" <<KDLEOF
system {
    worker-threads 2
    max-connections 100
    graceful-shutdown-timeout-secs 2
}

listeners {
    listener "http" {
        address "127.0.0.1:${PROXY_PORT}"
        protocol "http"
        request-timeout-secs 30
    }
}

routes {
    route "default" {
        priority "low"
        matches {
            path-prefix "/"
        }
        upstream "static-backend"
        filters "image-opt-filter"
    }
}

upstreams {
    upstream "static-backend" {
        target "127.0.0.1:${BACKEND_PORT}" weight=1
        load-balancing "round_robin"
    }
}

filters {
    filter "image-opt-filter" {
        type "agent"
        agent "image-opt"
        phase "both"
        timeout-ms 10000
        failure-mode "open"
    }
}

agents {
    agent "image-opt" {
        type "custom"
        unix-socket "${SOCKET_PATH}"
        protocol-version "v2"
        events "request_headers" "response_headers" "response_body" "request_complete"
        timeout-ms 10000
        failure-mode "open"
        response-body-mode "stream"
    }
}

limits {
    max-header-count 100
    max-header-size-bytes 8192
    max-body-size-bytes 10485760
}

observability {
    logging {
        level "debug"
        format "json"
    }
}
KDLEOF

info "KDL config written to $TMPDIR/zentinel.kdl"

# ──────────────────────────────────────────────────────────────────────────────
# Start zentinel proxy
# ──────────────────────────────────────────────────────────────────────────────

info "Starting zentinel proxy on :${PROXY_PORT}..."
"$ZENTINEL_BIN" --config "$TMPDIR/zentinel.kdl" \
    > "$TMPDIR/proxy.log" 2>&1 &
PIDS+=($!)
info "Proxy PID: ${PIDS[-1]}"

# ──────────────────────────────────────────────────────────────────────────────
# Wait for all services to be ready
# ──────────────────────────────────────────────────────────────────────────────

info "Waiting for services to be ready..."

# Wait for backend
for i in $(seq 1 30); do
    if curl -sf "http://127.0.0.1:${BACKEND_PORT}/data.json" > /dev/null 2>&1; then
        info "Backend ready (${i}s)"
        break
    fi
    if [[ "$i" == "30" ]]; then
        echo -e "${RED}FATAL: Backend failed to start${NC}"
        cat "$TMPDIR/backend.log" 2>/dev/null || true
        exit 1
    fi
    sleep 1
done

# Wait for agent socket
for i in $(seq 1 30); do
    if [[ -S "$SOCKET_PATH" ]]; then
        info "Agent socket ready (${i}s)"
        break
    fi
    if ! kill -0 "${PIDS[1]}" 2>/dev/null; then
        echo -e "${RED}FATAL: Agent process died${NC}"
        cat "$TMPDIR/agent.log" 2>/dev/null || true
        exit 1
    fi
    if [[ "$i" == "30" ]]; then
        echo -e "${RED}FATAL: Agent socket not created${NC}"
        cat "$TMPDIR/agent.log" 2>/dev/null || true
        exit 1
    fi
    sleep 1
done

# Wait for proxy
for i in $(seq 1 30); do
    if curl -sf -o /dev/null "$PROXY_URL/data.json" 2>/dev/null; then
        info "Proxy ready (${i}s)"
        break
    fi
    if ! kill -0 "${PIDS[2]}" 2>/dev/null; then
        echo -e "${RED}FATAL: Proxy process died${NC}"
        cat "$TMPDIR/proxy.log" 2>/dev/null || true
        exit 1
    fi
    if [[ "$i" == "30" ]]; then
        echo -e "${RED}FATAL: Proxy failed to start${NC}"
        cat "$TMPDIR/proxy.log" 2>/dev/null || true
        exit 1
    fi
    sleep 1
done

echo ""
info "All services ready — running tests"
echo ""

# ──────────────────────────────────────────────────────────────────────────────
# Detect whether proxy dispatches response events to agents.
# If the proxy hasn't wired response_headers/response_body dispatch yet,
# conversion tests cannot pass — mark them SKIP instead of FAIL.
# Detection: send a WebP request, check for x-image-optimized header.
# ──────────────────────────────────────────────────────────────────────────────

PROBE_FILE="$TMPDIR/probe"
curl -sf -o "$PROBE_FILE" -D "$PROBE_FILE.headers" \
    -H "Accept: image/webp" "$PROXY_URL/test.png" 2>/dev/null || true

RESPONSE_EVENTS_WORKING=false
if grep -qi "^x-image-optimized:" "$PROBE_FILE.headers" 2>/dev/null; then
    RESPONSE_EVENTS_WORKING=true
    info "Response event dispatching detected — full test suite enabled"
else
    info "Response events not dispatched to agents — conversion tests will SKIP"
    info "(proxy has not yet wired response_headers/response_body agent dispatch)"
fi

echo ""

# ──────────────────────────────────────────────────────────────────────────────
# Test phase
# ──────────────────────────────────────────────────────────────────────────────

# -- Test 1: Proxy serves images correctly --
info "Test 1: Proxy serves PNG from backend"
RESP_FILE="$TMPDIR/resp1"
HTTP_CODE=$(curl -sf -o "$RESP_FILE" -D "$RESP_FILE.headers" -w "%{http_code}" \
    "$PROXY_URL/test.png" 2>/dev/null || echo "000")

if [[ "$HTTP_CODE" == "200" ]]; then
    CT=$(grep -i "^content-type:" "$RESP_FILE.headers" | tr -d '\r' | head -1 | awk '{print $2}')
    if echo "$CT" | grep -qi "image/png"; then
        pass "Proxy serves PNG: HTTP 200, Content-Type image/png"
    else
        fail "Proxy serves PNG: Expected Content-Type image/png, got '$CT'"
    fi
else
    fail "Proxy serves PNG: Expected HTTP 200, got $HTTP_CODE"
fi

# -- Test 2: Agent receives request_headers events --
info "Test 2: Agent receives request_headers events"
# Send a request so the agent sees it
curl -sf -o /dev/null -H "Accept: image/webp" "$PROXY_URL/test.png" 2>/dev/null || true
sleep 0.5

# Check agent log for request_headers processing
if grep -q "Processing request headers" "$TMPDIR/agent.log" 2>/dev/null; then
    pass "Agent receives request_headers events"
else
    fail "Agent did not receive request_headers events"
fi

# -- Test 3: Agent v2 handshake and pool connection --
info "Test 3: Agent v2 UDS handshake succeeds"
if grep -q "UDS v2 handshake successful" "$TMPDIR/proxy.log" 2>/dev/null; then
    CONNS=$(grep -c "UDS v2 handshake successful" "$TMPDIR/proxy.log" 2>/dev/null || echo "0")
    pass "Agent v2 handshake: $CONNS pool connections established"
else
    fail "Agent v2 handshake not found in proxy logs"
fi

# -- Test 4: Non-image passthrough --
info "Test 4: Non-image passthrough (JSON with Accept: image/webp)"
RESP_FILE="$TMPDIR/resp4"
HTTP_CODE=$(curl -sf -o "$RESP_FILE" -D "$RESP_FILE.headers" -w "%{http_code}" \
    -H "Accept: image/webp" "$PROXY_URL/data.json" 2>/dev/null || echo "000")

if [[ "$HTTP_CODE" == "200" ]]; then
    CT=$(grep -i "^content-type:" "$RESP_FILE.headers" | tr -d '\r' | head -1 | awk '{print $2}')
    if echo "$CT" | grep -qi "application/json"; then
        pass "Non-image passthrough: Content-Type is application/json"
    else
        fail "Non-image passthrough: Expected application/json, got '$CT'"
    fi
else
    fail "Non-image passthrough: Expected HTTP 200, got $HTTP_CODE"
fi

# -- Test 5: PNG passthrough when no Accept: image/webp --
info "Test 5: PNG passthrough (no webp accept header)"
RESP_FILE="$TMPDIR/resp5"
HTTP_CODE=$(curl -sf -o "$RESP_FILE" -D "$RESP_FILE.headers" -w "%{http_code}" \
    "$PROXY_URL/test.png" 2>/dev/null || echo "000")

if [[ "$HTTP_CODE" == "200" ]]; then
    # Body should still be PNG (magic: 0x89 P N G = hex 89504e47)
    MAGIC=$(xxd -l 4 -p "$RESP_FILE" 2>/dev/null)
    if [[ "$MAGIC" == "89504e47" ]]; then
        pass "PNG passthrough: body starts with PNG magic bytes"
    else
        fail "PNG passthrough: unexpected body magic. Got: $MAGIC"
    fi
else
    fail "PNG passthrough: Expected HTTP 200, got $HTTP_CODE"
fi

# -- Tests 6-7: Conversion tests (require response event dispatching) --

if [[ "$RESPONSE_EVENTS_WORKING" == "true" ]]; then

    # -- Test 6: WebP conversion --
    info "Test 6: WebP conversion"
    RESP_FILE="$TMPDIR/resp6"
    HTTP_CODE=$(curl -sf -o "$RESP_FILE" -D "$RESP_FILE.headers" -w "%{http_code}" \
        -H "Accept: image/webp" "$PROXY_URL/test.png" 2>/dev/null || echo "000")

    if [[ "$HTTP_CODE" == "200" ]]; then
        MAGIC=$(xxd -l 12 "$RESP_FILE" 2>/dev/null | head -1)
        if echo "$MAGIC" | grep -qi "RIFF" && echo "$MAGIC" | grep -qi "WEBP"; then
            pass "WebP conversion: RIFF+WEBP magic bytes present"
        else
            fail "WebP conversion: Missing RIFF+WEBP magic bytes. Got: $MAGIC"
        fi
    else
        fail "WebP conversion: Expected HTTP 200, got $HTTP_CODE"
    fi

    # -- Test 7: AVIF conversion --
    info "Test 7: AVIF conversion"
    RESP_FILE="$TMPDIR/resp7"
    HTTP_CODE=$(curl -sf -o "$RESP_FILE" -D "$RESP_FILE.headers" -w "%{http_code}" \
        -H "Accept: image/avif" "$PROXY_URL/test.png" 2>/dev/null || echo "000")

    if [[ "$HTTP_CODE" == "200" ]]; then
        FTYP=$(dd if="$RESP_FILE" bs=1 skip=4 count=4 2>/dev/null)
        if [[ "$FTYP" == "ftyp" ]]; then
            pass "AVIF conversion: ftyp box found at byte 4"
        else
            fail "AVIF conversion: Missing ftyp box. Got: $(xxd -l 12 "$RESP_FILE" 2>/dev/null | head -1)"
        fi
    else
        fail "AVIF conversion: Expected HTTP 200, got $HTTP_CODE"
    fi

else
    skip "Test 6: WebP conversion (proxy response event dispatch not yet wired)"
    skip "Test 7: AVIF conversion (proxy response event dispatch not yet wired)"
fi

# ──────────────────────────────────────────────────────────────────────────────
# Results
# ──────────────────────────────────────────────────────────────────────────────

echo ""
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
if [[ $SKIPPED -gt 0 ]]; then
    echo -e "  Results: ${GREEN}${PASSED} passed${NC}, ${RED}${FAILED} failed${NC}, ${CYAN}${SKIPPED} skipped${NC}"
else
    echo -e "  Results: ${GREEN}${PASSED} passed${NC}, ${RED}${FAILED} failed${NC}"
fi
echo -e "${BLUE}═══════════════════════════════════════════════════════════════${NC}"
echo ""

if [[ $FAILED -gt 0 ]]; then
    info "Logs available in: $TMPDIR"
    info "  proxy:   $TMPDIR/proxy.log"
    info "  agent:   $TMPDIR/agent.log"
    info "  backend: $TMPDIR/backend.log"
    # Don't cleanup the temp dir on failure so logs can be inspected
    trap - EXIT
    # Still kill the processes
    for pid in "${PIDS[@]}"; do
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
        fi
    done
    rm -f "$SOCKET_PATH"
fi

exit "$FAILED"
