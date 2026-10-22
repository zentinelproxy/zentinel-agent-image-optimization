#!/usr/bin/env bash
# Smoke test: run the agent binary behind a real Zentinel (fail-closed) with
# the shared harness from zentinelproxy/.github (agent-smoke/smoke.py), then
# check the response phase with scripts/smoke/image-e2e.py.
#
#   AGENT_SMOKE=<path to smoke.py> scripts/smoke-test.sh [<agent-binary> <zentinel-binary> [harness args...]]
#
# The binaries default to $AGENT_BIN and $ZENTINEL_BIN, which the shared
# release workflow sets. Run by that workflow on every platform before any
# asset is published, and by CI on every push and pull request. Extra
# arguments go to the harness (e.g. `--transport grpc`).
#
# Harness, with no configuration file (the built-in defaults):
#   * the v2 handshake works and an ordinary (non-image) request from a
#     client that accepts WebP is forwarded (200), untouched;
#   * with the agent stopped the ordinary request is refused.
# image-e2e.py (the harness checks requests only, and its upstream serves
# text): a PNG from the upstream reaches a client that accepts WebP as WebP,
# with Content-Type: image/webp and X-Image-Optimized: webp, and a client
# that accepts only PNG gets the original bytes.

set -euo pipefail

if [ $# -ge 2 ]; then AGENT_BIN=$1 ZENTINEL_BIN=$2; shift 2; fi
AGENT_BIN=${AGENT_BIN:?usage: smoke-test.sh <agent-binary> <zentinel-binary>}
ZENTINEL_BIN=${ZENTINEL_BIN:?usage: smoke-test.sh <agent-binary> <zentinel-binary>}
AGENT_SMOKE=${AGENT_SMOKE:?set AGENT_SMOKE to agent-smoke/smoke.py from zentinelproxy/.github}
HERE="$(cd "$(dirname "$0")" && pwd)"

python3 "$AGENT_SMOKE" --agent "$AGENT_BIN" --zentinel "$ZENTINEL_BIN" --name image-optimization \
    --events request_headers,response_headers,response_body \
    --clean '{"name": "ordinary request", "path": "/smoke", "headers": {"Accept": "image/webp,*/*"}}' \
    --case '{"name": "page request", "path": "/index.html", "headers": {"Accept": "text/html"}, "expect": 200}' \
    "$@"

echo "--- response phase (scripts/smoke/image-e2e.py)"
python3 "$HERE/smoke/image-e2e.py" "$AGENT_BIN" "$ZENTINEL_BIN"
