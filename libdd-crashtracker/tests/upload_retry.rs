// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for crash report upload retry behaviour.
//!
//! Each test starts a mock TCP server that returns configurable HTTP status
//! codes, then exercises the upload path through `CrashInfo::async_upload_to_endpoint`.

#![cfg(unix)]

use alloc::borrow::Cow;
use alloc::sync::Arc;
use core::net::SocketAddr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use core::time::Duration;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use libdd_common::Endpoint;
use libdd_crashtracker::{CrashInfoBuilder, ErrorKind};

extern crate alloc;

// ---------------------------------------------------------------------------
// Mock HTTP server
// ---------------------------------------------------------------------------

/// A tiny mock HTTP server that returns the status codes from `responses` in
/// order, cycling the last one for any requests beyond the list length.
/// It counts total requests received.
///
/// The server stops when `stop` is signalled or after an accept timeout of 1 s
/// with no new connection, so tests never hang even when the exact request
/// count cannot be predicted.
struct MockHttpServer {
    address: SocketAddr,
    request_count: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockHttpServer {
    fn start(responses: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        // Non-blocking accept with a short timeout so the thread can check the
        // stop flag regularly and exit on its own when no more requests arrive.
        listener
            .set_nonblocking(false)
            .expect("set_nonblocking(false)");
        let address = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let count_clone = Arc::clone(&request_count);
        let stop_clone = Arc::clone(&stop);

        let handle = thread::spawn(move || {
            // Accept with a timeout so we can check the stop flag.
            listener
                .set_nonblocking(false)
                .expect("set_nonblocking(false)");

            while !stop_clone.load(Ordering::Relaxed) {
                // Use a short SO_RCVTIMEO / poll to avoid blocking forever.
                // std TcpListener doesn't expose accept-with-timeout directly,
                // so we set a read timeout on the listener fd via a raw socket
                // poll. Instead, reuse set_nonblocking + sleep.
                listener.set_nonblocking(true).ok();
                let stream = listener.accept();
                listener.set_nonblocking(false).ok();

                let mut stream = match stream {
                    Ok((s, _)) => s,
                    #[allow(clippy::std_instead_of_core)]
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // No pending connection — sleep briefly and re-check.
                        thread::sleep(Duration::from_millis(50));
                        continue;
                    }
                    Err(_) => break,
                };

                let idx = count_clone.fetch_add(1, Ordering::SeqCst);
                let status = responses
                    .get(idx)
                    .copied()
                    .unwrap_or_else(|| responses.last().copied().unwrap_or(200));

                // Read the full HTTP request (headers + body).
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = match stream.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(header_end) = find_header_end(&raw) {
                        let headers = String::from_utf8_lossy(&raw[..header_end]);
                        let content_length = parse_content_length(&headers);
                        let body_received = raw.len() - header_end;
                        if body_received >= content_length {
                            break;
                        }
                    }
                }

                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\n\
                     content-length: 0\r\n\
                     connection: close\r\n\r\n",
                    status = status,
                    reason = reason_phrase(status),
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        Self {
            address,
            request_count,
            stop,
            handle: Some(handle),
        }
    }

    fn request_count(&self) -> usize {
        self.request_count.load(Ordering::SeqCst)
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint {
            url: format!("http://{}", self.address).parse().unwrap(),
            api_key: Some(Cow::Borrowed("test-api-key")),
            timeout_ms: 5_000,
            test_token: None,
            use_system_resolver: false,
        }
    }
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock the accept loop by connecting once.
        let _ = std::net::TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

fn parse_content_length(headers: &str) -> usize {
    for line in headers.to_ascii_lowercase().lines() {
        if let Some(val) = line.strip_prefix("content-length:") {
            return val.trim().parse().unwrap_or(0);
        }
    }
    0
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

fn build_test_crash_info() -> libdd_crashtracker::CrashInfo {
    let mut builder = CrashInfoBuilder::new();
    builder.with_kind(ErrorKind::UnixSignal).unwrap();
    builder.build().unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The upload succeeds on the first try so no retries needed.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn upload_succeeds_on_first_attempt() {
    let server = MockHttpServer::start(vec![200]);
    let endpoint = server.endpoint();

    let crash_info = build_test_crash_info();
    let result = crash_info
        .async_upload_to_endpoint(&Some(endpoint))
        .await
        .expect("upload_to_endpoint should not fail");

    assert!(
        result.telemetry.is_ok(),
        "telemetry upload should succeed: {:?}",
        result.telemetry
    );
    assert!(
        result.errors_intake.is_ok(),
        "errors intake upload should succeed: {:?}",
        result.errors_intake
    );

    // Both paths fire concurrently; expect exactly 2 requests.
    assert_eq!(
        server.request_count(),
        2,
        "expected 2 requests (one per upload path)"
    );
}

/// The server returns 500s then 200. The upload should succeed after retries.
/// We run telemetry and errors intake concurrently, so both paths interleave
/// their requests on the same mock server.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn upload_retries_on_server_error() {
    // First 4 requests -> 500, everything after -> 200.
    // With 2 concurrent paths each retrying up to 3 times, at least one path
    // will eventually reach a 200.
    let server = MockHttpServer::start(vec![500, 500, 500, 500, 200]);
    let endpoint = server.endpoint();

    let crash_info = build_test_crash_info();
    let result = crash_info
        .async_upload_to_endpoint(&Some(endpoint))
        .await
        .expect("upload_to_endpoint should not fail");

    // With interleaved requests from both paths, at least one should succeed.
    let any_succeeded = result.telemetry.is_ok() || result.errors_intake.is_ok();
    assert!(
        any_succeeded,
        "at least one upload path should succeed after retries; \
         telemetry: {:?}, errors_intake: {:?}",
        result.telemetry, result.errors_intake,
    );

    let count = server.request_count();
    assert!(
        count > 2,
        "expected retries (>2 requests), got {count} requests"
    );
}

/// A 429 (Too Many Requests) should also be retried.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn upload_retries_on_rate_limit() {
    // First 2 requests -> 429, everything after -> 200.
    let server = MockHttpServer::start(vec![429, 429, 200]);
    let endpoint = server.endpoint();

    let crash_info = build_test_crash_info();
    let result = crash_info
        .async_upload_to_endpoint(&Some(endpoint))
        .await
        .expect("upload_to_endpoint should not fail");

    let any_succeeded = result.telemetry.is_ok() || result.errors_intake.is_ok();
    assert!(
        any_succeeded,
        "at least one upload path should succeed after 429 retries; \
         telemetry: {:?}, errors_intake: {:?}",
        result.telemetry, result.errors_intake,
    );

    let count = server.request_count();
    assert!(
        count > 2,
        "expected retries (>2 requests), got {count} requests"
    );
}

/// A 403 is not retryable — the upload should fail immediately.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn upload_does_not_retry_client_error() {
    let server = MockHttpServer::start(vec![403]);
    let endpoint = server.endpoint();

    let crash_info = build_test_crash_info();
    let result = crash_info
        .async_upload_to_endpoint(&Some(endpoint))
        .await
        .expect("upload_to_endpoint should not fail");

    assert!(
        result.telemetry.is_err(),
        "telemetry upload should fail on 403"
    );
    assert!(
        result.errors_intake.is_err(),
        "errors intake upload should fail on 403"
    );

    // Should be exactly 2 requests — one per upload path, no retries.
    assert_eq!(
        server.request_count(),
        2,
        "expected exactly 2 requests (no retries for 403)"
    );
}

/// All retries exhausted — both paths should report failure.
#[cfg_attr(miri, ignore)]
#[tokio::test]
async fn upload_fails_after_max_retries() {
    // Always return 500 — all 3 attempts per path should fail.
    let server = MockHttpServer::start(vec![500]);
    let endpoint = server.endpoint();

    let crash_info = build_test_crash_info();
    let result = crash_info
        .async_upload_to_endpoint(&Some(endpoint))
        .await
        .expect("upload_to_endpoint should not fail");

    assert!(
        result.telemetry.is_err(),
        "telemetry upload should fail after max retries"
    );
    assert!(
        result.errors_intake.is_err(),
        "errors intake upload should fail after max retries"
    );

    // Both paths should have made 3 attempts each = 6 total.
    assert_eq!(
        server.request_count(),
        6,
        "expected 6 requests (3 retries x 2 paths)"
    );

    // Verify the error messages mention the status code.
    let telem_err = result.telemetry.unwrap_err().to_string();
    assert!(
        telem_err.contains("500"),
        "telemetry error should mention status 500: {telem_err}"
    );
    let intake_err = result.errors_intake.unwrap_err().to_string();
    assert!(
        intake_err.contains("500"),
        "errors intake error should mention status 500: {intake_err}"
    );
}
