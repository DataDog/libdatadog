// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Provide [`send_with_retry`] utility to send a payload to an [`Endpoint`] with retries if the
//! request fails.

mod retry_strategy;
pub use retry_strategy::{RetryBackoffType, RetryStrategy};

pub(crate) mod compression;
pub use compression::CompressionStrategy;

use bytes::Bytes;
use futures::future::{select, Either};
use http::HeaderMap;
use libdd_capabilities::{HttpClientCapability, HttpError, SleepCapability};
use libdd_common::Endpoint;
use std::time::Duration;
use tracing::{debug, error};

pub type Attempts = u32;

pub type SendWithRetryResult = Result<(http::Response<Bytes>, Attempts), SendWithRetryError>;

/// An HTTP request prepared for repeated transport attempts.
///
/// Payload compression and invariant header construction happen once. Each
/// call to [`Self::request`] creates a fresh request with a cheaply cloned
/// [`Bytes`] body, allowing a host runtime to implement retries without using
/// libdatadog's HTTP client or async executor.
#[derive(Clone, Debug)]
pub struct PreparedRequest {
    target: Endpoint,
    payload: Bytes,
    headers: HeaderMap,
    compression_strategy: CompressionStrategy,
}

impl PreparedRequest {
    /// Prepares a payload and its invariant request metadata.
    pub fn new(
        target: Endpoint,
        payload: Vec<u8>,
        headers: HeaderMap,
        compression_strategy: CompressionStrategy,
    ) -> Self {
        let (payload, compression_strategy) = compression::compress(payload, compression_strategy);
        Self {
            target,
            payload: Bytes::from(payload),
            headers,
            compression_strategy,
        }
    }

    /// Builds one transport attempt.
    pub fn request(&self) -> Result<http::Request<Bytes>, http::Error> {
        let mut builder = http::Request::builder()
            .method(http::Method::POST)
            .uri(self.target.url.clone());
        builder = self
            .target
            .set_standard_headers(builder, concat!("Tracer/", env!("CARGO_PKG_VERSION")));
        for (key, value) in &self.headers {
            builder = builder.header(key, value);
        }
        if let Some(headers) = builder.headers_mut() {
            compression::add_headers(headers, self.compression_strategy);
        }
        builder.body(self.payload.clone())
    }

    /// Returns the timeout for one transport attempt.
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.target.timeout_ms)
    }

    /// Returns the prepared payload after any configured compression.
    pub fn payload(&self) -> &Bytes {
        &self.payload
    }
}

/// All errors contain the number of attempts after which the final error was returned
#[derive(Debug)]
pub enum SendWithRetryError {
    /// The request received an error HTTP code.
    Http(http::Response<Bytes>, Attempts),
    /// Treats timeout errors originated in the transport layer.
    Timeout(Attempts),
    /// Treats errors coming from networking.
    Network(HttpError, Attempts),
    /// Treats errors while reading the response body.
    ResponseBody(Attempts),
    /// Treats errors coming from building the request
    Build(Attempts),
}

impl std::fmt::Display for SendWithRetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendWithRetryError::Http(_, _) => write!(f, "Http error code received"),
            SendWithRetryError::Timeout(_) => write!(f, "Request timed out"),
            SendWithRetryError::Network(error, _) => write!(f, "Network error: {error}"),
            SendWithRetryError::ResponseBody(_) => write!(f, "Failed to read response body"),
            SendWithRetryError::Build(_) => {
                write!(f, "Failed to build request due to invalid property")
            }
        }
    }
}

impl std::error::Error for SendWithRetryError {}

/// Send the `payload` with a POST request to `target` using the provided `retry_strategy` if the
/// request fails.
///
/// Standard endpoint headers (user-agent, api-key, test-token, entity headers) are set
/// automatically via [`Endpoint::set_standard_headers`]. Additional `headers` are appended to the
/// request. The request is executed with a timeout of [`Endpoint::timeout_ms`].
///
/// # Returns
///
/// Return a [`SendWithRetryResult`] containing the response and the number of attempts or an error
/// describing the last attempt failure.
///
/// # Errors
/// Fail if the request didn't succeed after applying the retry strategy.
///
/// # Example
///
/// ```rust, no_run
/// # use libdd_common::Endpoint;
/// # use libdd_capabilities::{HttpClientCapability, SleepCapability};
/// # use libdd_trace_utils::send_with_retry::*;
/// # async fn run() -> SendWithRetryResult {
/// let payload: Vec<u8> = vec![0, 1, 2, 3];
/// let target = Endpoint {
///     url: "localhost:8126/v04/traces".parse::<hyper::Uri>().unwrap(),
///     ..Endpoint::default()
/// };
/// let mut headers = http::HeaderMap::new();
/// headers.insert(
///     http::HeaderName::from_static("content-type"),
///     http::HeaderValue::from_static("application/msgpack"),
/// );
/// let retry_strategy = RetryStrategy::new(3, 10, RetryBackoffType::Exponential, Some(5));
/// let capabilities = libdd_capabilities_impl::NativeCapabilities::new_client();
/// send_with_retry(
///     &capabilities,
///     &target,
///     payload,
///     &headers,
///     &retry_strategy,
///     CompressionStrategy::None,
/// )
/// .await
/// # }
/// ```
#[allow(clippy::result_large_err)]
pub async fn send_with_retry<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    target: &Endpoint,
    payload: Vec<u8>,
    headers: &HeaderMap,
    retry_strategy: &RetryStrategy,
    compression_strategy: CompressionStrategy,
) -> SendWithRetryResult {
    let prepared = PreparedRequest::new(
        target.clone(),
        payload,
        headers.clone(),
        compression_strategy,
    );
    send_prepared_with_retry(capabilities, &prepared, retry_strategy).await
}

/// Sends a pre-built request plan with retries.
///
/// This is the executor-driven counterpart to the host-driven API exposed by
/// [`PreparedRequest`] and [`RetryStrategy`].
#[allow(clippy::result_large_err)]
pub async fn send_prepared_with_retry<C: HttpClientCapability + SleepCapability>(
    capabilities: &C,
    prepared: &PreparedRequest,
    retry_strategy: &RetryStrategy,
) -> SendWithRetryResult {
    let mut request_attempt = 0;
    let timeout = prepared.timeout();

    debug!(
        url = %prepared.target.url,
        payload_size = prepared.payload.len(),
        max_retries = retry_strategy.max_retries(),
        "Sending with retry"
    );

    loop {
        request_attempt += 1;

        debug!(
            url = %prepared.target.url,
            attempt = request_attempt,
            max_retries = retry_strategy.max_retries(),
            "Attempting request"
        );

        let req = match prepared.request() {
            Ok(r) => r,
            Err(_) => {
                return Err(SendWithRetryError::Build(request_attempt));
            }
        };

        let request = capabilities.request(req);
        let timeout = capabilities.sleep(timeout);
        futures::pin_mut!(request, timeout);
        let result = match select(request, timeout).await {
            Either::Left((response, _)) => Ok(response),
            Either::Right(((), _)) => Err(()),
        };

        match result {
            Ok(Ok(response)) => {
                let status = response.status();
                debug!(
                    url = %prepared.target.url,
                    status = status.as_u16(),
                    attempt = request_attempt,
                    "Received response"
                );

                if RetryStrategy::is_retryable_status(status) {
                    debug!(
                        status = status.as_u16(),
                        attempt = request_attempt,
                        max_retries = retry_strategy.max_retries(),
                        "Received error status code"
                    );

                    if request_attempt <= retry_strategy.max_retries() {
                        debug!(
                            attempt = request_attempt,
                            remaining_retries = retry_strategy.max_retries() - request_attempt + 1,
                            "Retrying after error status code"
                        );
                        retry_strategy.delay(request_attempt, capabilities).await;
                        continue;
                    } else {
                        error!(
                            status = status.as_u16(),
                            attempts = request_attempt,
                            "Max retries exceeded, returning HTTP error"
                        );
                        return Err(SendWithRetryError::Http(response, request_attempt));
                    }
                } else {
                    debug!(
                        status = status.as_u16(),
                        attempts = request_attempt,
                        "Request succeeded"
                    );
                    return Ok((response, request_attempt));
                }
            }
            Ok(Err(e)) => {
                debug!(
                    url = %prepared.target.url,
                    error = ?e,
                    attempt = request_attempt,
                    max_retries = retry_strategy.max_retries(),
                    "Request failed with error"
                );

                if request_attempt <= retry_strategy.max_retries() {
                    debug!(
                        attempt = request_attempt,
                        remaining_retries = retry_strategy.max_retries() - request_attempt + 1,
                        "Retrying after request error"
                    );
                    retry_strategy.delay(request_attempt, capabilities).await;
                    continue;
                } else {
                    let classified_error = match e {
                        HttpError::Timeout => SendWithRetryError::Timeout(request_attempt),
                        HttpError::InvalidRequest(_) => SendWithRetryError::Build(request_attempt),
                        HttpError::ResponseBody(_) => {
                            SendWithRetryError::ResponseBody(request_attempt)
                        }
                        other => SendWithRetryError::Network(other, request_attempt),
                    };
                    error!(
                        error = ?classified_error,
                        attempts = request_attempt,
                        "Max retries exceeded, returning request error"
                    );
                    return Err(classified_error);
                }
            }
            Err(_) => {
                debug!(
                    url = %prepared.target.url,
                    attempt = request_attempt,
                    max_retries = retry_strategy.max_retries(),
                    "Request timed out"
                );

                if request_attempt <= retry_strategy.max_retries() {
                    debug!(
                        attempt = request_attempt,
                        remaining_retries = retry_strategy.max_retries() - request_attempt + 1,
                        "Retrying after timeout"
                    );
                    retry_strategy.delay(request_attempt, capabilities).await;
                    continue;
                } else {
                    error!(
                        attempts = request_attempt,
                        "Max retries exceeded, returning timeout error"
                    );
                    return Err(SendWithRetryError::Timeout(request_attempt));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::poll_for_mock_hit;
    use http::{HeaderName, HeaderValue, Method};
    use httpmock::MockServer;
    use libdd_capabilities::HttpClientCapability;
    use libdd_capabilities_impl::NativeCapabilities;

    #[test]
    fn prepared_request_builds_repeatable_attempts() {
        let target = Endpoint {
            url: "https://example.test/v1/input".parse().unwrap(),
            timeout_ms: 1_234,
            test_token: Some("test-token".into()),
            ..Default::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-datadog-trace-count"),
            HeaderValue::from_static("2"),
        );
        let prepared =
            PreparedRequest::new(target, vec![1, 2, 3], headers, CompressionStrategy::None);

        let first = prepared.request().unwrap();
        let second = prepared.request().unwrap();

        assert_eq!(first.method(), Method::POST);
        assert_eq!(first.uri(), "https://example.test/v1/input");
        assert_eq!(first.headers()["x-datadog-trace-count"], "2");
        assert_eq!(
            first.headers()["x-datadog-test-session-token"],
            "test-token"
        );
        assert_eq!(first.body().as_ref(), &[1, 2, 3]);
        assert_eq!(first.body(), second.body());
        assert_eq!(prepared.payload().as_ref(), &[1, 2, 3]);
        assert_eq!(prepared.timeout(), Duration::from_millis(1_234));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_zero_retries_on_error() {
        let server = MockServer::start();

        let mut mock_503 = server
            .mock_async(|_when, then| {
                then.status(503)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"error"}"#);
            })
            .await;

        let _mock_202 = server
            .mock_async(|_when, then| {
                then.status(202)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"ok"}"#);
            })
            .await;

        let target_endpoint = Endpoint {
            url: server.url("").to_owned().parse().unwrap(),
            api_key: Some("test-key".into()),
            ..Default::default()
        };

        let strategy = RetryStrategy::new(0, 2, RetryBackoffType::Constant, None);
        let capabilities = NativeCapabilities::new_client();

        tokio::spawn(async move {
            let result = send_with_retry(
                &capabilities,
                &target_endpoint,
                vec![0, 1, 2, 3],
                &HeaderMap::new(),
                &strategy,
                CompressionStrategy::None,
            )
            .await;
            assert!(result.is_err(), "Expected an error result");
            assert!(
                matches!(result.unwrap_err(), SendWithRetryError::Http(_, 1)),
                "Expected an http error with one attempt"
            );
        });

        assert!(poll_for_mock_hit(&mut mock_503, 10, 100, 1, true).await);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_logic_error_then_success() {
        let server = MockServer::start();

        let mut mock_503 = server
            .mock_async(|_when, then| {
                then.status(503)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"error"}"#);
            })
            .await;

        let mut mock_202 = server
            .mock_async(|_when, then| {
                then.status(202)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"ok"}"#);
            })
            .await;

        let target_endpoint = Endpoint {
            url: server.url("").to_owned().parse().unwrap(),
            api_key: Some("test-key".into()),
            ..Default::default()
        };

        let strategy = RetryStrategy::new(2, 250, RetryBackoffType::Constant, None);
        let capabilities = NativeCapabilities::new_client();

        tokio::spawn(async move {
            let result = send_with_retry(
                &capabilities,
                &target_endpoint,
                vec![0, 1, 2, 3],
                &HeaderMap::new(),
                &strategy,
                CompressionStrategy::None,
            )
            .await;
            assert!(
                matches!(result.unwrap(), (_, 2)),
                "Expected an ok result after two attempts"
            );
        });

        assert!(poll_for_mock_hit(&mut mock_503, 10, 100, 1, true).await);
        assert!(
            poll_for_mock_hit(&mut mock_202, 10, 100, 1, true).await,
            "Expected a retry request after a 5xx error"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_logic_max_errors() {
        let server = MockServer::start();
        let max_retries = 3;
        let expected_total_attempts = max_retries + 1;
        let mut mock_503 = server
            .mock_async(|_when, then| {
                then.status(503)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"error"}"#);
            })
            .await;

        let target_endpoint = Endpoint {
            url: server.url("").to_owned().parse().unwrap(),
            api_key: Some("test-key".into()),
            ..Default::default()
        };

        let strategy = RetryStrategy::new(max_retries, 10, RetryBackoffType::Constant, None);
        let capabilities = NativeCapabilities::new_client();

        tokio::spawn(async move {
            let result = send_with_retry(
                &capabilities,
                &target_endpoint,
                vec![0, 1, 2, 3],
                &HeaderMap::new(),
                &strategy,
                CompressionStrategy::None,
            )
            .await;
            assert!(
                matches!(result.unwrap_err(), SendWithRetryError::Http(_, attempts) if attempts == expected_total_attempts),
                "Expected an error result after max retry attempts"
            );
        });

        assert!(
            poll_for_mock_hit(
                &mut mock_503,
                10,
                100,
                expected_total_attempts as usize,
                true
            )
            .await,
            "Expected max retry attempts"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_logic_no_errors() {
        let server = MockServer::start();
        let mut mock_202 = server
            .mock_async(|_when, then| {
                then.status(202)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"Ok"}"#);
            })
            .await;

        let target_endpoint = Endpoint {
            url: server.url("").to_owned().parse().unwrap(),
            api_key: Some("test-key".into()),
            ..Default::default()
        };

        let strategy = RetryStrategy::new(2, 10, RetryBackoffType::Constant, None);
        let capabilities = NativeCapabilities::new_client();

        tokio::spawn(async move {
            let result = send_with_retry(
                &capabilities,
                &target_endpoint,
                vec![0, 1, 2, 3],
                &HeaderMap::new(),
                &strategy,
                CompressionStrategy::None,
            )
            .await;
            assert!(
                matches!(result, Ok((_, attempts)) if attempts == 1),
                "Expected an ok result after one attempts"
            );
        });

        assert!(
            poll_for_mock_hit(&mut mock_202, 10, 250, 1, true).await,
            "Expected only one request attempt"
        );
    }
}
