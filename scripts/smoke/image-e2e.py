#!/usr/bin/env python3
"""Response-phase smoke test for the image optimization agent.

The shared harness (agent-smoke/smoke.py) checks requests: what the client
gets back as a status and what the upstream receives. This agent works on
responses, so this script checks the other half: behind a real Zentinel, a
PNG from the upstream reaches a client that accepts WebP as WebP, with the
headers the agent sets, and reaches a client that does not as the original
PNG.

    image-e2e.py <agent-binary> <zentinel-binary>

Exit status 0 on success, 1 on a failed check. Standard library only.
"""

import http.client
import os
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def png(width: int = 32, height: int = 32) -> bytes:
    """A small RGB gradient PNG."""
    rows = b"".join(
        b"\x00" + bytes(c for x in range(width) for c in (x * 8 % 256, y * 8 % 256, 128))
        for y in range(height)
    )

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


PNG = png()


class Upstream(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        self.send_header("Content-Length", str(len(PNG)))
        self.end_headers()
        self.wfile.write(PNG)

    def log_message(self, *args) -> None:
        pass


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def get(port: int, path: str, accept: str):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
    try:
        conn.request("GET", path, headers={"Accept": accept})
        resp = conn.getresponse()
        return resp.status, {k.lower(): v for k, v in resp.getheaders()}, resp.read()
    except (OSError, http.client.HTTPException):
        return 0, {}, b""
    finally:
        conn.close()


def main() -> int:
    agent_bin, zentinel_bin = (os.path.abspath(p) for p in sys.argv[1:3])
    work = tempfile.mkdtemp(prefix="io-e2e.", dir=os.environ.get("SMOKE_SOCKET_DIR", "/tmp"))
    sock = os.path.join(work, "agent.sock")
    proxy_port = free_port()
    upstream = ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
    threading.Thread(target=upstream.serve_forever, daemon=True).start()
    procs = []
    failures = []
    try:
        cache = os.path.join(work, "cache")
        config = os.path.join(work, "config.json")
        with open(config, "w") as f:
            f.write('{"cache": {"directory": "%s"}}' % cache)
        procs.append(
            subprocess.Popen(
                [agent_bin, "--socket", sock, "--config", config],
                stdout=open(os.path.join(work, "agent.log"), "wb"),
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        )
        for _ in range(100):
            if os.path.exists(sock):
                break
            time.sleep(0.1)
        kdl = os.path.join(work, "zentinel.kdl")
        with open(kdl, "w") as f:
            f.write(
                f"""
system {{
    worker-threads 1
    graceful-shutdown-timeout-secs 1
}}
listeners {{
    listener "http" {{
        address "127.0.0.1:{proxy_port}"
        protocol "http"
    }}
}}
routes {{
    route "images" {{
        matches {{
            path-prefix "/"
        }}
        upstream "backend"
        filters "image-optimization"
        policies {{
            failure-mode "closed"
        }}
    }}
}}
agents {{
    agent "image-optimization" type="custom" {{
        unix-socket path="{sock}"
        events "request_headers" "response_headers" "response_body"
        timeout-ms 5000
        failure-mode "closed"
    }}
}}
filters {{
    filter "image-optimization" {{
        type "agent"
        agent "image-optimization"
        timeout-ms 5000
        failure-mode "closed"
    }}
}}
upstreams {{
    upstream "backend" {{
        target "127.0.0.1:{upstream.server_address[1]}"
    }}
}}
"""
            )
        procs.append(
            subprocess.Popen(
                [zentinel_bin, "--config", kdl],
                stdout=open(os.path.join(work, "zentinel.log"), "wb"),
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        )
        deadline = time.monotonic() + 60
        status = 0
        while time.monotonic() < deadline and status == 0:
            status, _, _ = get(proxy_port, "/ready.png", "image/png")
            if status == 0:
                time.sleep(0.3)

        def check(name: str, ok: bool, detail: str) -> None:
            print(("ok   " if ok else "FAIL ") + name + ("" if ok else f": {detail}"))
            if not ok:
                failures.append(name)

        status, headers, body = get(proxy_port, "/photo.png", "image/webp,image/*;q=0.8")
        check("WebP client: 200", status == 200, f"got {status or 'no response'}")
        check(
            "WebP client: Content-Type image/webp",
            headers.get("content-type") == "image/webp",
            f"Content-Type {headers.get('content-type')!r}",
        )
        check(
            "WebP client: X-Image-Optimized: webp",
            headers.get("x-image-optimized") == "webp",
            f"X-Image-Optimized {headers.get('x-image-optimized')!r}",
        )
        check(
            "WebP client: body is WebP",
            body[:4] == b"RIFF" and body[8:12] == b"WEBP",
            f"body starts {body[:12]!r}",
        )

        status, headers, body = get(proxy_port, "/photo.png", "image/png")
        check("PNG-only client: 200", status == 200, f"got {status or 'no response'}")
        check(
            "PNG-only client: original PNG",
            headers.get("content-type") == "image/png" and body == PNG,
            f"Content-Type {headers.get('content-type')!r}, {len(body)} bytes",
        )
        if failures:
            for name in ("agent", "zentinel"):
                print(f"--- {name}.log (tail)", file=sys.stderr)
                with open(os.path.join(work, f"{name}.log"), "rb") as f:
                    sys.stderr.write(f.read()[-3000:].decode(errors="replace"))
        return 1 if failures else 0
    finally:
        for p in procs:
            try:
                p.send_signal(signal.SIGTERM)
                p.wait(timeout=5)
            except (subprocess.TimeoutExpired, ProcessLookupError):
                p.kill()
        upstream.shutdown()
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
