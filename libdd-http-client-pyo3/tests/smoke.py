# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

"""Smoke test for the libdd_http_client Python wheel.

Run after `maturin develop --features python-extension` from the parent
directory:

    pip install maturin
    cd libdd-http-client-pyo3
    maturin develop --features python-extension
    python tests/smoke.py

The test issues a GET and a JSON POST against a local mock server using
Python's stdlib `http.server` (no third-party deps) and asserts the
expected status codes / bodies / exception subclasses round-trip
correctly.
"""

import http.server
import json
import socketserver
import threading
import time

import libdd_http_client as ddhttp


class _Handler(http.server.BaseHTTPRequestHandler):
    """Minimal mock that echoes the path, method, and JSON body."""

    def log_message(self, format, *args):  # noqa: A002 — match stdlib signature
        pass  # Silence the per-request log line.

    # Force HTTP/1.0 with `Connection: close` so reqwest doesn't keep the
    # socket alive waiting for the next request. BaseHTTPRequestHandler's
    # default is HTTP/1.0 already, but we make it explicit here to keep the
    # interaction simple.
    protocol_version = "HTTP/1.0"

    def _send(self, status, body, content_type=None):
        self.send_response(status)
        if content_type:
            self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802 — stdlib API
        if self.path == "/ping":
            self._send(200, b'{"ok":true}', "application/json")
        elif self.path == "/down":
            self._send(503, b"service unavailable")
        else:
            self._send(404, b"not found")

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        payload = self.rfile.read(length) if length else b""
        if self.path == "/v0.4/traces":
            assert (
                self.headers.get("content-type") == "application/json"
            ), f"missing content-type, got {dict(self.headers)}"
            assert json.loads(payload) == {"hello": "world"}, payload
            self._send(200, b"ok")
        else:
            self._send(404, b"not found")


def _start_server():
    # Threading server so a request that's still being read doesn't block
    # the next one. The reqwest backend opportunistically issues HEAD/keep-
    # alive activity that the single-threaded TCPServer doesn't service.
    server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), _Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    # Give the server a moment to start.
    time.sleep(0.05)
    return server, thread


def main():
    server, _thread = _start_server()
    base_url = f"http://127.0.0.1:{server.server_address[1]}"

    runtime = ddhttp.SharedRuntime()
    client = ddhttp.HttpClient(base_url, 5.0)

    # GET /ping → 200 + JSON body.
    req = ddhttp.HttpRequest(ddhttp.HttpMethod.Get, f"{base_url}/ping")
    resp = client.send_blocking(req, runtime)
    assert resp.status_code == 200, resp.status_code
    assert json.loads(resp.body) == {"ok": True}, resp.body
    assert resp.headers["content-type"] == "application/json", resp.headers

    # POST /v0.4/traces with JSON body and content-type header.
    req = ddhttp.HttpRequest(
        ddhttp.HttpMethod.Post,
        f"{base_url}/v0.4/traces",
        headers={"content-type": "application/json"},
        body=b'{"hello":"world"}',
    )
    resp = client.send_blocking(req, runtime)
    assert resp.status_code == 200, resp.status_code
    assert resp.body == b"ok", resp.body

    # GET /down → 503 → RequestFailedError with status + body.
    req = ddhttp.HttpRequest(ddhttp.HttpMethod.Get, f"{base_url}/down")
    try:
        client.send_blocking(req, runtime)
    except ddhttp.RequestFailedError as err:
        assert err.status == 503, err.status
        assert err.body == b"service unavailable", err.body
        # Subclass check: the base class is also `HttpClientError`.
        assert isinstance(err, ddhttp.HttpClientError)
    else:
        raise AssertionError("expected RequestFailedError on /down")

    # Builder path with mutators.
    builder = ddhttp.HttpClientBuilder()
    builder.set_base_url(base_url)
    builder.set_timeout_secs(5.0)
    builder.set_allow_connection_pooling(False)
    client2 = builder.build()
    req = ddhttp.HttpRequest(ddhttp.HttpMethod.Get, f"{base_url}/ping")
    resp = client2.send_blocking(req, runtime)
    assert resp.status_code == 200

    # Clean shutdown of the runtime.
    runtime.shutdown(timeout_secs=2.0)
    server.shutdown()
    print("smoke test passed")


if __name__ == "__main__":
    main()
