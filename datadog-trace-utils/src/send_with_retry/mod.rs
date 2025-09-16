// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Provide [`send_with_retry`] utility to send a payload to an [`Endpoint`] with retries if the
//! request fails.

mod retry_strategy;
pub use retry_strategy::{RetryBackoffType, RetryStrategy};

use bytes::Bytes;
use ddcommon::{hyper_migration, Endpoint, HttpRequestBuilder};
use hyper::Method;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

pub type Attempts = u32;

pub type SendWithRetryResult =
    Result<(hyper_migration::HttpResponse, Attempts), SendWithRetryError>;

/// All errors contain the number of attempts after which the final error was returned
#[derive(Debug)]
pub enum SendWithRetryError {
    /// The request received an error HTTP code.
    Http(hyper_migration::HttpResponse, Attempts),
    /// Treats timeout errors originated in the transport layer.
    Timeout(Attempts),
    /// Treats errors coming from networking.
    Network(hyper_migration::ClientError, Attempts),
    /// Treats errors coming from building the request
    Build(Attempts),
}

impl std::fmt::Display for SendWithRetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendWithRetryError::Http(_, _) => write!(f, "Http error code received"),
            SendWithRetryError::Timeout(_) => write!(f, "Request timed out"),
            SendWithRetryError::Network(error, _) => write!(f, "Network error: {error}"),
            SendWithRetryError::Build(_) => {
                write!(f, "Failed to build request due to invalid property")
            }
        }
    }
}

impl std::error::Error for SendWithRetryError {}

impl SendWithRetryError {
    fn from_request_error(err: RequestError, request_attempt: Attempts) -> Self {
        match err {
            RequestError::Build => SendWithRetryError::Build(request_attempt),
            RequestError::Network(error) => SendWithRetryError::Network(error, request_attempt),
            RequestError::TimeoutApi => SendWithRetryError::Timeout(request_attempt),
        }
    }
}

#[derive(Debug)]
enum RequestError {
    Build,
    Network(hyper_migration::ClientError),
    TimeoutApi,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::TimeoutApi => write!(f, "Api timeout exhausted"),
            RequestError::Network(error) => write!(f, "Network error: {error}"),
            RequestError::Build => write!(f, "Failed to build request due to invalid property"),
        }
    }
}

impl std::error::Error for RequestError {}

type ClientCache = Arc<
    RwLock<
        HashMap<
            Option<String>,
            Arc<
                dyn Fn(hyper_migration::HttpRequest) -> hyper_migration::ResponseFuture
                    + Send
                    + Sync,
            >,
        >,
    >,
>;

fn get_global_client_cache() -> &'static ClientCache {
    static CLIENT_CACHE: std::sync::OnceLock<ClientCache> = std::sync::OnceLock::new();
    CLIENT_CACHE.get_or_init(|| Arc::new(RwLock::new(HashMap::new())))
}

async fn get_or_create_client(
    proxy_url: Option<&str>,
) -> Result<
    Arc<dyn Fn(hyper_migration::HttpRequest) -> hyper_migration::ResponseFuture + Send + Sync>,
    RequestError,
> {
    let cache_key = proxy_url.map(|s| s.to_string());
    let cache = get_global_client_cache();

    {
        let clients = cache.read().await;
        if let Some(client) = clients.get(&cache_key) {
            return Ok(client.clone());
        }
    }

    let mut clients = cache.write().await;

    if let Some(client) = clients.get(&cache_key) {
        return Ok(client.clone());
    }

    #[cfg(feature = "proxy")]
    let client: Arc<
        dyn Fn(hyper_migration::HttpRequest) -> hyper_migration::ResponseFuture + Send + Sync,
    > = {
        if let Some(url) = proxy_url {
            let proxy_uri = url.parse().map_err(|_| RequestError::Build)?;
            let proxy = hyper_http_proxy::Proxy::new(hyper_http_proxy::Intercept::Https, proxy_uri);
            let proxy_connector = hyper_http_proxy::ProxyConnector::from_proxy(
                ddcommon::connector::Connector::default(),
                proxy,
            )
            .map_err(|_| RequestError::Build)?;
            let client = hyper_migration::client_builder().build(proxy_connector);
            Arc::new(move |req| client.request(req))
        } else {
            let client = hyper_migration::new_default_client();
            Arc::new(move |req| client.request(req))
        }
    };

    #[cfg(not(feature = "proxy"))]
    let client: Arc<
        dyn Fn(hyper_migration::HttpRequest) -> hyper_migration::ResponseFuture + Send + Sync,
    > = {
        let _ = proxy_url;
        let client = hyper_migration::new_default_client();
        Arc::new(move |req| client.request(req))
    };

    clients.insert(cache_key, client.clone());
    Ok(client)
}

pub async fn clear_client_cache() {
    let cache = get_global_client_cache();
    let mut clients = cache.write().await;
    clients.clear();
}

/// Send the `payload` with a POST request to `target` using the provided `retry_strategy` if the
/// request fails.
///
/// The request builder from [`Endpoint::to_request_builder`] is used with the associated headers
/// (api key, test token), and `headers` are added to the request. If `http_proxy` is provided then
/// it is used as the uri of the proxy. The request is executed with a timeout of
/// [`Endpoint::timeout_ms`].
///
/// # Arguments
/// http_proxy will be ignored if hte crate is not compiled with the `proxy` feature
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
/// # use ddcommon::Endpoint;
/// # use std::collections::HashMap;
/// # use datadog_trace_utils::send_with_retry::*;
/// # async fn run() -> SendWithRetryResult {
/// let payload: Vec<u8> = vec![0, 1, 2, 3];
/// let target = Endpoint {
///     url: "localhost:8126/v04/traces".parse::<hyper::Uri>().unwrap(),
///     ..Endpoint::default()
/// };
/// let headers = HashMap::from([("Content-type", "application/msgpack".to_string())]);
/// let retry_strategy = RetryStrategy::new(3, 10, RetryBackoffType::Exponential, Some(5));
/// send_with_retry(&target, payload, &headers, &retry_strategy, None).await
/// # }
/// ```
pub async fn send_with_retry(
    target: &Endpoint,
    payload: Vec<u8>,
    headers: &HashMap<&'static str, String>,
    retry_strategy: &RetryStrategy,
    http_proxy: Option<&str>,
) -> SendWithRetryResult {
    let mut request_attempt = 0;
    // Wrap the payload in Bytes to avoid expensive clone between retries
    let payload = Bytes::from(payload);

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

        let mut req = target
            .to_request_builder(concat!("Tracer/", env!("CARGO_PKG_VERSION")))
            .or(Err(SendWithRetryError::Build(request_attempt)))?
            .method(Method::POST);
        for (key, value) in headers {
            req = req.header(*key, value.clone());
        }

        match send_request(
            Duration::from_millis(target.timeout_ms),
            req,
            payload.clone(),
            http_proxy,
        )
        .await
        {
            // An Ok response doesn't necessarily mean the request was successful, we need to
            // check the status code and if it's not a 2xx or 3xx we treat it as an error
            Ok(response) => {
                let status = response.status();
                debug!(status = %status, attempt = request_attempt, "Received response");

                if status.is_client_error() || status.is_server_error() {
                    warn!(
                        status = %status,
                        attempt = request_attempt,
                        max_retries = retry_strategy.max_retries(),
                        "Received error status code"
                    );

                    if request_attempt < retry_strategy.max_retries() {
                        info!(
                            attempt = request_attempt,
                            remaining_retries = retry_strategy.max_retries() - request_attempt,
                            "Retrying after error status code"
                        );
                        retry_strategy.delay(request_attempt).await;
                        continue;
                    } else {
                        error!(
                            status = %status,
                            attempts = request_attempt,
                            "Max retries exceeded, returning HTTP error"
                        );
                        return Err(SendWithRetryError::Http(response, request_attempt));
                    }
                } else {
                    info!(
                        status = %status,
                        attempts = request_attempt,
                        "Request succeeded"
                    );
                    return Ok((response, request_attempt));
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    attempt = request_attempt,
                    max_retries = retry_strategy.max_retries(),
                    "Request failed with error"
                );

                if request_attempt < retry_strategy.max_retries() {
                    info!(
                        attempt = request_attempt,
                        remaining_retries = retry_strategy.max_retries() - request_attempt,
                        "Retrying after request error"
                    );
                    retry_strategy.delay(request_attempt).await;
                    continue;
                } else {
                    error!(
                        error = %e,
                        attempts = request_attempt,
                        "Max retries exceeded, returning request error"
                    );
                    return Err(SendWithRetryError::from_request_error(e, request_attempt));
                }
            }
        }
    }
}

async fn send_request(
    timeout: Duration,
    req: HttpRequestBuilder,
    payload: Bytes,
    http_proxy: Option<&str>,
) -> Result<hyper_migration::HttpResponse, RequestError> {
    let req = req
        .body(hyper_migration::Body::from_bytes(payload))
        .or(Err(RequestError::Build))?;

    let client = get_or_create_client(http_proxy).await?;
    let req_future = client(req);

    match tokio::time::timeout(timeout, req_future).await {
        Ok(resp) => match resp {
            Ok(body) => Ok(hyper_migration::into_response(body)),
            Err(e) => Err(RequestError::Network(e)),
        },
        Err(_) => Err(RequestError::TimeoutApi),
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
        clear_client_cache().await;
        let server = MockServer::start();

        let mut mock_503 = server
            .mock_async(|_when, then| {
                then.status(503)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"error"}"#);
            })
            .await;

        // We add this mock so that if a second request was made it would be a success and our
        // assertion below that last_result is an error would fail.
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
                None,
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
        clear_client_cache().await;
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
                None,
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
        clear_client_cache().await;
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
                None,
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
        clear_client_cache().await;
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
                None,
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

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_client_caching() {
        clear_client_cache().await;

        // Test that the same client is reused for the same proxy configuration
        let _client1 = get_or_create_client(None).await.unwrap();
        let _client2 = get_or_create_client(None).await.unwrap();

        // Check cache has one entry for no proxy
        let cache = get_global_client_cache().read().await;
        assert_eq!(cache.len(), 1, "Should have one cached client");
        assert!(
            cache.contains_key(&None),
            "Should have cached client for no proxy"
        );
        drop(cache);

        #[cfg(feature = "proxy")]
        {
            let _proxy_client1 = get_or_create_client(Some("http://proxy.example.com:8080"))
                .await
                .unwrap();
            let _proxy_client2 = get_or_create_client(Some("http://proxy.example.com:8080"))
                .await
                .unwrap();

            // Check cache now has two entries
            let cache = get_global_client_cache().read().await;
            assert_eq!(cache.len(), 2, "Should have two cached clients");
            assert!(
                cache.contains_key(&Some("http://proxy.example.com:8080".to_string())),
                "Should have cached client for proxy"
            );
            drop(cache);

            // Different proxy URL should create a different client
            let _different_proxy = get_or_create_client(Some("http://other.proxy.com:8080"))
                .await
                .unwrap();

            let cache = get_global_client_cache().read().await;
            assert_eq!(cache.len(), 3, "Should have three cached clients");
            drop(cache);
        }

        // Test cache clearing
        let cache_before_clear = get_global_client_cache().read().await.len();
        assert!(cache_before_clear > 0, "Cache should have entries");

        clear_client_cache().await;

        let cache_after_clear = get_global_client_cache().read().await.len();
        assert_eq!(cache_after_clear, 0, "Cache should be empty after clearing");
    }
}
