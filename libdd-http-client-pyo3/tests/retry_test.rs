// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Smoke test for the retry surface through pyo3.
//!
//! Verifies that a `RetryConfig` plumbed through `HttpClientBuilder.retry()`
//! actually retries on failure. We use a tiny stateful TCP server (rather
//! than `httpmock`) because httpmock's matchers don't expose a "first call
//! fails, second succeeds" pattern out of the box.

use libdd_http_client_pyo3::{
    HttpClientBuilder, HttpMethod, HttpRequest, RetryConfig, SharedRuntime,
};
use pyo3::prelude::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn ensure_python() {
    Python::initialize();
}

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Read until "\r\n\r\n" so the connection cleanly hits EOF after.
fn drain_request(mut stream: &TcpStream) {
    let mut buf = [0u8; 4096];
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    while let Ok(n) = stream.read(&mut buf) {
        if n == 0 {
            break;
        }
        // Look for the end of headers; bodies in this test are GETs (none).
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
}

#[test]
fn retry_config_construction_defaults() {
    ensure_python();
    Python::attach(|py| {
        let cfg = Py::new(py, RetryConfig::new(None, None, None)).unwrap();
        let r = cfg
            .into_pyobject(py)
            .unwrap()
            .repr()
            .unwrap()
            .to_string();
        assert!(r.contains("RetryConfig"));
    });
}

#[test]
fn retried_request_succeeds_on_second_attempt() {
    ensure_python();
    ensure_crypto_provider();

    // Bind to an ephemeral port and run a tiny single-thread server that
    // alternates 503 -> 200.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_t = counter.clone();
    let server_thread = thread::spawn(move || {
        // We expect at most a handful of connections; cap to avoid hanging.
        for _ in 0..8 {
            match listener.accept() {
                Ok((mut sock, _peer)) => {
                    drain_request(&sock);
                    let n = counter_t.fetch_add(1, Ordering::SeqCst);
                    let response = if n == 0 {
                        // First attempt: 503
                        b"HTTP/1.1 503 Service Unavailable\r\n\
                          Content-Length: 5\r\n\
                          Connection: close\r\n\
                          \r\n\
                          fail!"
                            .to_vec()
                    } else {
                        // Subsequent attempts: 200
                        b"HTTP/1.1 200 OK\r\n\
                          Content-Length: 2\r\n\
                          Connection: close\r\n\
                          \r\n\
                          ok"
                            .to_vec()
                    };
                    let _ = sock.write_all(&response);
                    let _ = sock.shutdown(std::net::Shutdown::Both);
                    if n >= 1 {
                        // Done; the test only ever needs two requests.
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    Python::attach(|py| {
        // Build a client with retries enabled — drive setters via call_method
        // because they are not exposed on the Rust-side type.
        let builder = Py::new(py, HttpClientBuilder::new()).unwrap();
        let bbb = builder.into_pyobject(py).unwrap();
        bbb.call_method1("set_base_url", (format!("http://{}", addr),))
            .unwrap();
        bbb.call_method1("set_timeout_secs", (5.0,)).unwrap();
        // No connection pooling: keeps each retry on a fresh socket so the
        // server thread sees both as independent accepts.
        bbb.call_method1("set_allow_connection_pooling", (false,))
            .unwrap();
        let retry = Py::new(py, RetryConfig::new(Some(3), Some(1), Some(false))).unwrap();
        bbb.call_method1("retry", (retry,)).unwrap();
        let client_obj = bbb.call_method0("build").unwrap();

        let req = HttpRequest::new(
            HttpMethod::Get,
            format!("http://{}/flaky", addr),
            None,
            None,
            None,
        )
        .unwrap();
        let req = Py::new(py, req).unwrap();
        let runtime = Py::new(py, SharedRuntime::new().unwrap()).unwrap();

        let resp = client_obj
            .call_method1("send_blocking", (&req, &runtime))
            .expect("retried request should succeed");
        let status: u16 = resp.getattr("status_code").unwrap().extract().unwrap();
        let body: Vec<u8> = resp.getattr("body").unwrap().extract().unwrap();
        assert_eq!(status, 200, "expected 200 after retry");
        assert_eq!(body, b"ok");
    });

    // Wait for the server thread; tolerate residual sleeping in some
    // environments by giving it a moment.
    let _ = server_thread.join();
    let total = counter.load(Ordering::SeqCst);
    assert!(
        total >= 2,
        "expected at least 2 server hits, got {total}"
    );
}
