// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Mock HTTP capabilities for integration testing.
//!
//! Responses are keyed by URI **path** (FIFO per path; falls back to `200 OK`).
//! Call [`MockHttpCapabilities::register_on_current_thread`] before `build()` so
//! capabilities constructed inside `build()` share the same queues as the test handle.
//!
//! `SleepCapability` delegates to `tokio::time::sleep`, so `tokio::time::pause()` works.

use bytes::Bytes;
use libdd_capabilities::{HttpClientCapability, HttpError, LogWriterCapability, SleepCapability};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// Response queue for a single URI path.
type PathResponseQueue = VecDeque<Result<http::Response<Bytes>, HttpError>>;

/// Compression encoding detected from the `Content-Encoding` header of a captured request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Zstd,
}

#[derive(Debug, Clone)]
pub struct CapturedRequest {
    pub method: http::Method,
    pub uri: http::Uri,
    pub headers: http::HeaderMap,
    /// The raw wire bytes exactly as received (may be compressed).
    pub raw_body: Bytes,
    /// Compression detected from `Content-Encoding`, or `None` if the body is uncompressed.
    pub compression: Option<Compression>,
    /// Always the decompressed body. Equal to `raw_body` when `compression` is `None`.
    pub body: Bytes,
}

impl CapturedRequest {
    fn from_parts(
        method: http::Method,
        uri: http::Uri,
        headers: http::HeaderMap,
        raw_body: Bytes,
    ) -> Self {
        let compression = headers
            .get(http::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| match s {
                "zstd" => Some(Compression::Zstd),
                _ => None,
            });
        let body = match compression {
            Some(Compression::Zstd) => zstd::decode_all(raw_body.as_ref())
                .map(Bytes::from)
                .unwrap_or_else(|_| raw_body.clone()),
            None => raw_body.clone(),
        };
        Self {
            method,
            uri,
            headers,
            raw_body,
            compression,
            body,
        }
    }

    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .unwrap_or_else(|| panic!("header '{name}' not found in captured request"))
            .to_str()
            .expect("header value is not valid UTF-8")
    }
}

pub struct MockHttpInner {
    /// Responses keyed by URI path, consumed FIFO per path.
    /// When a path has no queued response the mock returns `200 OK`.
    responses: Mutex<HashMap<String, PathResponseQueue>>,
    requests: Mutex<Vec<CapturedRequest>>,
    /// Broadcast-notified each time a request is captured.
    notify: Notify,
}

impl std::fmt::Debug for MockHttpInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.requests.lock().unwrap().len();
        f.debug_struct("MockHttpInner")
            .field("captured_requests", &n)
            .finish()
    }
}

thread_local! {
    /// Read by `new_client()` so builder-internal capability instances share the test's queues.
    static CURRENT_MOCK: RefCell<Option<Arc<MockHttpInner>>> = const { RefCell::new(None) };
}

/// Mock capabilities for integration tests. All clones share the same [`Arc<MockHttpInner>`].
#[derive(Clone, Debug)]
pub struct MockHttpCapabilities {
    inner: Arc<MockHttpInner>,
}

impl Default for MockHttpCapabilities {
    fn default() -> Self {
        Self::new()
    }
}

impl MockHttpCapabilities {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MockHttpInner {
                responses: Mutex::new(HashMap::new()),
                requests: Mutex::new(Vec::new()),
                notify: Notify::new(),
            }),
        }
    }

    /// Registers shared state on the current thread so `new_client()` (called internally by
    /// `build()`) returns an instance backed by the same queues. Call in every thread/closure
    /// that calls `build()`.
    pub fn register_on_current_thread(&self) {
        CURRENT_MOCK.with(|cell| *cell.borrow_mut() = Some(Arc::clone(&self.inner)));
    }

    /// Queues a response for the next request to `path` (FIFO per path).
    pub fn queue_response_for_path(&self, path: &str, status: u16, body: impl Into<Bytes>) {
        let resp = http::Response::builder()
            .status(status)
            .body(body.into())
            .expect("valid status code");
        self.inner
            .responses
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .or_default()
            .push_back(Ok(resp));
    }

    pub fn queue_error_for_path(&self, path: &str, err: HttpError) {
        self.inner
            .responses
            .lock()
            .unwrap()
            .entry(path.to_owned())
            .or_default()
            .push_back(Err(err));
    }

    pub fn captured_requests(&self) -> Vec<CapturedRequest> {
        self.inner.requests.lock().unwrap().clone()
    }

    pub fn captured_request_count(&self) -> usize {
        self.inner.requests.lock().unwrap().len()
    }

    /// Waits until at least `n` requests are captured or `timeout` elapses.
    pub async fn wait_for_requests(&self, n: usize, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.captured_request_count() >= n {
                return true;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return self.captured_request_count() >= n;
            }
            tokio::select! {
                _ = self.inner.notify.notified() => {}
                _ = tokio::time::sleep(remaining) => {}
            }
        }
    }

    pub async fn assert_requests(&self, n: usize, timeout: Duration) {
        assert!(
            self.wait_for_requests(n, timeout).await,
            "timed out waiting for {n} request(s); only {} arrived",
            self.captured_request_count()
        );
    }
}

impl HttpClientCapability for MockHttpCapabilities {
    fn new_client() -> Self {
        // Prefer the thread-local state registered by the test so all clones
        // share the same queues as the MockHttpCapabilities the test holds.
        let inner = CURRENT_MOCK
            .with(|cell| cell.borrow().clone())
            .unwrap_or_else(|| {
                Arc::new(MockHttpInner {
                    responses: Mutex::new(HashMap::new()),
                    requests: Mutex::new(Vec::new()),
                    notify: Notify::new(),
                })
            });
        Self { inner }
    }

    fn new_periodic() -> Self {
        Self::new_client()
    }

    fn request(
        &self,
        req: http::Request<Bytes>,
    ) -> impl std::future::Future<Output = Result<http::Response<Bytes>, HttpError>>
           + libdd_capabilities::MaybeSend {
        let path = req.uri().path().to_owned();
        let (parts, body) = req.into_parts();
        let captured = CapturedRequest::from_parts(parts.method, parts.uri, parts.headers, body);

        let response = {
            let mut responses = self.inner.responses.lock().unwrap();
            responses
                .get_mut(&path)
                .and_then(|q| q.pop_front())
                .unwrap_or_else(|| {
                    Ok(http::Response::builder()
                        .status(200)
                        .body(Bytes::new())
                        .expect("valid status"))
                })
        };

        self.inner.requests.lock().unwrap().push(captured);
        self.inner.notify.notify_waiters();

        async move { response }
    }
}

impl SleepCapability for MockHttpCapabilities {
    fn new() -> Self {
        Self::new_client()
    }

    fn sleep(
        &self,
        duration: Duration,
    ) -> impl std::future::Future<Output = ()> + libdd_capabilities::MaybeSend {
        tokio::time::sleep(duration)
    }
}

impl LogWriterCapability for MockHttpCapabilities {
    fn write_log_output(&self, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_capabilities::HttpClientCapability;

    #[tokio::test]
    async fn test_path_keyed_responses_are_served_in_fifo_order() {
        let mock = MockHttpCapabilities::new();
        mock.queue_response_for_path("/stats", 202, "accepted-1");
        mock.queue_response_for_path("/stats", 202, "accepted-2");
        mock.queue_response_for_path("/traces", 200, "ok");

        let cap = mock.clone();

        let resp_s1 = cap
            .request(
                http::Request::builder()
                    .method("POST")
                    .uri("http://host/stats")
                    .body(Bytes::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_s1.status(), 202);
        assert_eq!(resp_s1.into_body(), Bytes::from("accepted-1"));

        let resp_t = cap
            .request(
                http::Request::builder()
                    .method("POST")
                    .uri("http://host/traces")
                    .body(Bytes::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_t.status(), 200);

        let resp_s2 = cap
            .request(
                http::Request::builder()
                    .method("POST")
                    .uri("http://host/stats")
                    .body(Bytes::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_s2.into_body(), Bytes::from("accepted-2"));

        // After the queue is exhausted the mock returns the default 200 OK.
        let resp_fallback = cap
            .request(
                http::Request::builder()
                    .method("POST")
                    .uri("http://host/unknown")
                    .body(Bytes::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp_fallback.status(), 200);
    }

    #[tokio::test]
    async fn test_captured_requests_record_method_uri_and_body() {
        let mock = MockHttpCapabilities::new();
        let cap = mock.clone();

        cap.request(
            http::Request::builder()
                .method("POST")
                .uri("http://example.com/foo?q=1")
                .body(Bytes::from("payload"))
                .unwrap(),
        )
        .await
        .unwrap();

        let reqs = mock.captured_requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, http::Method::POST);
        assert_eq!(reqs[0].uri.path(), "/foo");
        assert_eq!(reqs[0].body, Bytes::from("payload"));
    }

    #[tokio::test]
    async fn test_register_on_current_thread_shares_state_with_new_client() {
        let mock = MockHttpCapabilities::new();
        mock.queue_response_for_path("/test", 201, "created");

        mock.register_on_current_thread();
        let from_new_client = MockHttpCapabilities::new_client();

        let resp = from_new_client
            .request(
                http::Request::builder()
                    .method("POST")
                    .uri("http://host/test")
                    .body(Bytes::new())
                    .unwrap(),
            )
            .await
            .unwrap();

        // The response was dequeued from the shared state.
        assert_eq!(resp.status(), 201);
        // The original handle sees the captured request.
        assert_eq!(mock.captured_request_count(), 1);
    }

    #[tokio::test]
    async fn test_wait_for_requests_returns_true_once_count_reached() {
        let mock = MockHttpCapabilities::new();
        let cap = mock.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cap.request(
                http::Request::builder()
                    .method("GET")
                    .uri("http://host/ping")
                    .body(Bytes::new())
                    .unwrap(),
            )
            .await
            .unwrap();
        });

        let arrived = mock.wait_for_requests(1, Duration::from_secs(1)).await;
        assert!(arrived);
    }

    #[tokio::test]
    async fn test_wait_for_requests_returns_false_on_timeout() {
        let mock = MockHttpCapabilities::new();
        let arrived = mock.wait_for_requests(1, Duration::from_millis(50)).await;
        assert!(!arrived);
    }

    #[tokio::test]
    async fn test_queue_error_for_path_returns_http_error() {
        use anyhow::anyhow;

        let mock = MockHttpCapabilities::new();
        mock.queue_error_for_path(
            "/bad",
            HttpError::Network(anyhow!("simulated network failure")),
        );

        let result = mock
            .request(
                http::Request::builder()
                    .method("POST")
                    .uri("http://host/bad")
                    .body(Bytes::new())
                    .unwrap(),
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), HttpError::Network(_)));
    }
}
