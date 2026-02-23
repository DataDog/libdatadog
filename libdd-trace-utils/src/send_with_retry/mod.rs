// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Provide [`send_with_retry`] utility to send a payload to an [`Endpoint`] with retries if the
//! request fails.

mod retry_strategy;
pub use retry_strategy::{RetryBackoffType, RetryStrategy};

use bytes::Bytes;
use libdd_capabilities::{HttpClientTrait, HttpError};
use libdd_capabilities_impl::DefaultHttpClient;
use libdd_common::Endpoint;
use std::{collections::HashMap, time::Duration};
use tracing::{debug, error};

pub type Attempts = u32;

pub type SendWithRetryResult = Result<(http::Response<Bytes>, Attempts), SendWithRetryError>;

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
/// # use libdd_trace_utils::send_with_retry::*;
/// # use std::collections::HashMap;
/// # async fn run() -> SendWithRetryResult {
/// let payload: Vec<u8> = vec![0, 1, 2, 3];
/// let target = Endpoint {
///     url: "localhost:8126/v04/traces".parse::<hyper::Uri>().unwrap(),
///     ..Endpoint::default()
/// };
/// let headers = HashMap::from([("Content-type", "application/msgpack".to_string())]);
/// let retry_strategy = RetryStrategy::new(3, 10, RetryBackoffType::Exponential, Some(5));
/// send_with_retry(&target, payload, &headers, &retry_strategy).await
/// # }
/// ```
pub async fn send_with_retry(
    target: &Endpoint,
    payload: Vec<u8>,
    headers: &HashMap<&'static str, String>,
    retry_strategy: &RetryStrategy,
) -> SendWithRetryResult {
    let mut request_attempt = 0;
    let timeout = Duration::from_millis(target.timeout_ms);
    let client = DefaultHttpClient::new_client();

    debug!(
        url = %target.url,
        payload_size = payload.len(),
        max_retries = retry_strategy.max_retries(),
        "Sending with retry"
    );

    loop {
        request_attempt += 1;

        debug!(
            attempt = request_attempt,
            max_retries = retry_strategy.max_retries(),
            "Attempting request"
        );

        let mut builder = http::Request::builder()
            .method(http::Method::POST)
            .uri(target.url.clone());
        builder =
            target.set_standard_headers(builder, concat!("Tracer/", env!("CARGO_PKG_VERSION")));
        for (key, value) in headers {
            builder = builder.header(*key, value.as_str());
        }
        let req = match builder.body(Bytes::from(payload.clone())) {
            Ok(r) => r,
            Err(_) => {
                return Err(SendWithRetryError::Build(request_attempt));
            }
        };

        let result = tokio::time::timeout(timeout, client.request(req)).await;

        match result {
            Ok(Ok(response)) => {
                let status = response.status();
                debug!(
                    status = status.as_u16(),
                    attempt = request_attempt,
                    "Received response"
                );

                if status.is_client_error() || status.is_server_error() {
                    debug!(
                        status = status.as_u16(),
                        attempt = request_attempt,
                        max_retries = retry_strategy.max_retries(),
                        "Received error status code"
                    );

                    if request_attempt < retry_strategy.max_retries() {
                        debug!(
                            attempt = request_attempt,
                            remaining_retries = retry_strategy.max_retries() - request_attempt,
                            "Retrying after error status code"
                        );
                        retry_strategy.delay(request_attempt).await;
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
                    error = ?e,
                    attempt = request_attempt,
                    max_retries = retry_strategy.max_retries(),
                    "Request failed with error"
                );

                if request_attempt < retry_strategy.max_retries() {
                    debug!(
                        attempt = request_attempt,
                        remaining_retries = retry_strategy.max_retries() - request_attempt,
                        "Retrying after request error"
                    );
                    retry_strategy.delay(request_attempt).await;
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
                    attempt = request_attempt,
                    max_retries = retry_strategy.max_retries(),
                    "Request timed out"
                );

                if request_attempt < retry_strategy.max_retries() {
                    debug!(
                        attempt = request_attempt,
                        remaining_retries = retry_strategy.max_retries() - request_attempt,
                        "Retrying after timeout"
                    );
                    retry_strategy.delay(request_attempt).await;
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
    use httpmock::MockServer;

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

        tokio::spawn(async move {
            let result = send_with_retry(
                &target_endpoint,
                vec![0, 1, 2, 3],
                &HashMap::new(),
                &strategy,
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

        tokio::spawn(async move {
            let result = send_with_retry(
                &target_endpoint,
                vec![0, 1, 2, 3],
                &HashMap::new(),
                &strategy,
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
        let expected_retry_attempts = 3;
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

        let strategy = RetryStrategy::new(
            expected_retry_attempts,
            10,
            RetryBackoffType::Constant,
            None,
        );

        tokio::spawn(async move {
            let result = send_with_retry(
                &target_endpoint,
                vec![0, 1, 2, 3],
                &HashMap::new(),
                &strategy,
            )
            .await;
            assert!(
                matches!(result.unwrap_err(), SendWithRetryError::Http(_, attempts) if attempts == expected_retry_attempts),
                "Expected an error result after max retry attempts"
            );
        });

        assert!(
            poll_for_mock_hit(
                &mut mock_503,
                10,
                100,
                expected_retry_attempts as usize,
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

        tokio::spawn(async move {
            let result = send_with_retry(
                &target_endpoint,
                vec![0, 1, 2, 3],
                &HashMap::new(),
                &strategy,
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
