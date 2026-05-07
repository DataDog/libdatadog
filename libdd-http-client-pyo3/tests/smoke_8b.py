# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

"""Smoke test for the Task 8b additions to the libdd_http_client wheel.

Run after `maturin develop --features python-extension` from the parent
directory:

    cd libdd-http-client-pyo3
    maturin develop --features python-extension
    python tests/smoke_8b.py

Covers:

- `MultipartPart` construction + upload to a local mock server.
- `RetryConfig` + `HttpClientBuilder.retry()` driving a retry on a flaky
  endpoint that returns 503 first then 200.
- `AgentClient` + `AgentClientBuilder` driving `agent_info` (Some + None)
  and `send_traces`.
"""

import http.server
import itertools
import json
import socketserver
import threading
import time

import libdd_http_client as ddhttp


class _Handler(http.server.BaseHTTPRequestHandler):
    """Mock that emulates the agent for /info, /v0.4/traces, /flaky, /upload."""

    protocol_version = "HTTP/1.0"

    # Class-level state for the flaky-endpoint counter.
    flaky_counter = itertools.count()

    def log_message(self, format, *args):  # noqa: A002 — match stdlib signature
        pass

    def _send(self, status, body, headers=None):
        self.send_response(status)
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.send_header("content-length", str(len(body)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802
        if self.path == "/info":
            body = json.dumps(
                {
                    "version": "7.50.0",
                    "endpoints": ["/v0.4/traces", "/v0.5/traces", "/info"],
                    "client_drop_p0s": True,
                    "config": {"default_env": "test", "tags": ["foo", "bar"]},
                }
            ).encode("utf-8")
            self._send(
                200,
                body,
                {
                    "content-type": "application/json",
                    "Datadog-Container-Tags-Hash": "deadbeef",
                    "Datadog-Agent-State": "state-1",
                },
            )
        elif self.path == "/info404":
            self._send(404, b"not found")
        elif self.path == "/flaky":
            n = next(self.flaky_counter)
            if n == 0:
                self._send(503, b"first call fails")
            else:
                self._send(200, b"ok-after-retry")
        else:
            self._send(404, b"not found")

    def do_PUT(self):  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        _payload = self.rfile.read(length) if length else b""
        if self.path == "/v0.4/traces":
            assert (
                self.headers.get("content-type") == "application/msgpack"
            ), self.headers
            assert (
                self.headers.get("X-Datadog-Trace-Count") == "1"
            ), self.headers
            self._send(
                200,
                b'{"rate_by_service":{"service:env":0.5}}',
                {"content-type": "application/json"},
            )
        else:
            self._send(404, b"not found")

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("content-length", "0"))
        payload = self.rfile.read(length) if length else b""
        if self.path == "/upload":
            ct = self.headers.get("content-type") or ""
            assert ct.startswith("multipart/form-data"), ct
            assert b"file content here" in payload, payload[:200]
            self._send(200, b"uploaded")
        else:
            self._send(404, b"not found")


def _start_server():
    server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), _Handler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    time.sleep(0.05)
    return server, thread


def main():
    server, _thread = _start_server()
    base_url = f"http://127.0.0.1:{server.server_address[1]}"
    runtime = ddhttp.SharedRuntime()

    # 1. Multipart upload through HttpClient.
    client = ddhttp.HttpClient(base_url, 5.0)
    part = ddhttp.MultipartPart(
        "file",
        b"file content here",
        filename="blob.bin",
        content_type="application/octet-stream",
    )
    req = ddhttp.HttpRequest(ddhttp.HttpMethod.Post, f"{base_url}/upload")
    req.with_multipart_part(part)
    assert req.multipart_parts_len == 1
    resp = client.send_blocking(req, runtime)
    assert resp.status_code == 200
    assert resp.body == b"uploaded"

    # 2. Retry: build a client with a RetryConfig and hit /flaky.
    builder = ddhttp.HttpClientBuilder()
    builder.set_base_url(base_url)
    builder.set_timeout_secs(5.0)
    # Avoid pooling to keep retries on independent connections.
    builder.set_allow_connection_pooling(False)
    retry_cfg = ddhttp.RetryConfig(
        max_retries=3,
        initial_delay_millis=1,
        with_jitter=False,
    )
    builder.retry(retry_cfg)
    client_retry = builder.build()
    req = ddhttp.HttpRequest(ddhttp.HttpMethod.Get, f"{base_url}/flaky")
    resp = client_retry.send_blocking(req, runtime)
    assert resp.status_code == 200, resp.status_code
    assert resp.body == b"ok-after-retry", resp.body

    # 3. AgentClient: agent_info Some + None and send_traces.
    lang = ddhttp.LanguageMetadata("python", "3.12", "CPython", "2.18.0")
    transport = ddhttp.AgentTransport.http("127.0.0.1", server.server_address[1])
    ab = ddhttp.AgentClientBuilder()
    ab.set_transport(transport)
    ab.set_language_metadata(lang)
    ab.set_timeout_millis(5_000)
    ac = ab.build()

    info = ac.agent_info(runtime)
    assert info is not None, "expected Some(AgentInfo)"
    assert info.version == "7.50.0", info.version
    assert info.client_drop_p0s is True
    assert info.endpoints == ["/v0.4/traces", "/v0.5/traces", "/info"]
    assert info.container_tags_hash == "deadbeef"
    assert info.state_hash == "state-1"
    cfg = info.config
    assert isinstance(cfg, dict), type(cfg)
    assert cfg["default_env"] == "test"
    assert cfg["tags"] == ["foo", "bar"]

    # send_traces -> /v0.4/traces.
    opts = ddhttp.TraceSendOptions()  # computed_top_level=False
    resp = ac.send_traces(b"\x80\x81", 1, ddhttp.TraceFormat.MsgpackV4, opts, runtime)
    assert resp.status == 200
    assert resp.rate_by_service == {"service:env": 0.5}, resp.rate_by_service

    # agent_info on a 404 returns None — point at /info404 by building a
    # fresh client with a different transport. We re-bind to the same
    # server but customise the path via a wrapper agent client whose URL
    # is built off `base_url`. Since AgentClient hard-codes `/info`, we
    # exercise this via a second mock server bound to a fresh port that
    # always 404s.
    server404 = socketserver.ThreadingTCPServer(("127.0.0.1", 0), _AlwaysNotFoundHandler)
    server404.daemon_threads = True
    threading.Thread(target=server404.serve_forever, daemon=True).start()
    time.sleep(0.05)
    ab2 = ddhttp.AgentClientBuilder()
    ab2.set_transport(
        ddhttp.AgentTransport.http("127.0.0.1", server404.server_address[1])
    )
    ab2.set_language_metadata(lang)
    ab2.set_timeout_millis(5_000)
    ac2 = ab2.build()
    info_none = ac2.agent_info(runtime)
    assert info_none is None
    server404.shutdown()

    # 4. Builder error: missing transport -> AgentBuildError.
    bad = ddhttp.AgentClientBuilder()
    bad.set_language_metadata(lang)
    try:
        bad.build()
    except ddhttp.AgentBuildError as exc:
        assert "transport" in str(exc).lower()
    else:
        raise AssertionError("expected AgentBuildError on missing transport")

    runtime.shutdown(timeout_secs=2.0)
    server.shutdown()
    print("8b smoke test passed")


class _AlwaysNotFoundHandler(http.server.BaseHTTPRequestHandler):
    """Always returns 404. Used to exercise `agent_info -> None`."""

    protocol_version = "HTTP/1.0"

    def log_message(self, format, *args):  # noqa: A002
        pass

    def do_GET(self):  # noqa: N802
        body = b"not found"
        self.send_response(404)
        self.send_header("content-length", str(len(body)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(body)


if __name__ == "__main__":
    main()
