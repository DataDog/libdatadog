// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{anyhow, Context};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use hyper::{Body, Client, Method, Response, StatusCode};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

use crate::tracer_header_tags::TracerHeaderTags;
use datadog_trace_protobuf::pb;
use ddcommon::{connector, Endpoint, HttpRequestBuilder};

#[derive(Debug)]
pub enum SendRequestError {
    Hyper(hyper::Error),
    Any(anyhow::Error),
}

pub struct SendDataResult {
    pub last_result: anyhow::Result<Response<Body>>,
    pub requests_count: u64,
    pub responses_count_per_code: HashMap<u16, u64>,
    pub errors_timeout: u64,
    pub errors_network: u64,
    pub errors_status_code: u64,
}

impl SendDataResult {
    fn new() -> SendDataResult {
        SendDataResult {
            last_result: Err(anyhow!("No requests sent")),
            requests_count: 0,
            responses_count_per_code: Default::default(),
            errors_timeout: 0,
            errors_network: 0,
            errors_status_code: 0,
        }
    }

    async fn update(
        &mut self,
        res: Result<Response<Body>, SendRequestError>,
        expected_status: StatusCode,
    ) {
        self.requests_count += 1;
        match res {
            Ok(response) => {
                *self
                    .responses_count_per_code
                    .entry(response.status().as_u16())
                    .or_default() += 1;
                self.last_result = if response.status() == expected_status {
                    Ok(response)
                } else {
                    self.errors_status_code += 1;

                    let body_bytes = hyper::body::to_bytes(response.into_body()).await;
                    let response_body = String::from_utf8(body_bytes.unwrap_or_default().to_vec())
                        .unwrap_or_default();
                    Err(anyhow::format_err!(
                        "Server did not accept traces: {response_body}"
                    ))
                }
            }
            Err(e) => match e {
                SendRequestError::Hyper(e) => {
                    if e.is_timeout() {
                        self.errors_timeout += 1;
                    } else {
                        self.errors_network += 1;
                    }
                    self.last_result = Err(anyhow!(e));
                }
                SendRequestError::Any(e) => {
                    self.last_result = Err(e);
                }
            },
        }
    }

    fn error(mut self, err: anyhow::Error) -> SendDataResult {
        self.last_result = Err(err);
        self
    }
}

fn construct_agent_payload(tracer_payloads: Vec<pb::TracerPayload>) -> pb::AgentPayload {
    pb::AgentPayload {
        host_name: "".to_string(),
        env: "".to_string(),
        agent_version: "".to_string(),
        error_tps: 60.0,
        target_tps: 60.0,
        tags: HashMap::new(),
        tracer_payloads,
        rare_sampler_enabled: false,
    }
}

fn serialize_proto_payload<T>(payload: &T) -> anyhow::Result<Vec<u8>>
where
    T: prost::Message,
{
    let mut buf = Vec::with_capacity(payload.encoded_len());
    payload.encode(&mut buf)?;
    Ok(buf)
}

/// Enum representing the type of backoff to use for the delay between retries.
///
/// ```
#[derive(Debug, Clone)]
pub enum RetryBackoffType {
    /// The delay is doubled for each attempt.
    Double,
    /// The delay is constant for each attempt.
    Constant,
    /// The delay is multiplied by the attempt number.
    Exponential,
}

// TODO: APMSP-1076 - RetryStrategy should be moved to a separate file when send_data is refactored.
/// Struct representing the retry strategy for sending data.
///
/// This struct contains the parameters that define how retries should be handled when sending data.
/// It includes the maximum number of retries, the delay between retries, the type of backoff to
/// use, and an optional jitter to add randomness to the delay.
///
/// # Examples
///
/// ```rust
/// use datadog_trace_utils::send_data::{RetryBackoffType, RetryStrategy};
/// use std::time::Duration;
///
/// let retry_strategy = RetryStrategy {
///     max_retries: 5,
///     delay_ms: Duration::from_millis(100),
///     backoff_type: RetryBackoffType::Double,
///     jitter: Some(Duration::from_millis(50)),
/// };
/// ```
#[derive(Debug, Clone)]
pub struct RetryStrategy {
    /// The maximum number of retries to attempt.
    pub max_retries: u32,
    /// The minimum delay between retries.
    pub delay_ms: Duration,
    /// The type of backoff to use for the delay between retries.
    pub backoff_type: RetryBackoffType,
    /// An optional jitter to add randomness to the delay.
    pub jitter: Option<Duration>,
}

impl Default for RetryStrategy {
    fn default() -> Self {
        RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Double,
            jitter: None,
        }
    }
}

impl RetryStrategy {
    /// Delays the next request attempt based on the retry strategy.
    ///
    /// This function calculates the delay duration based on the retry strategy's backoff type:
    /// - `Double`: The delay is doubled for each attempt.
    /// - `Constant`: The delay is constant for each attempt.
    /// - `Exponential`: The delay is multiplied by the attempt number.
    ///
    /// If a jitter duration is specified in the retry strategy, a random duration up to the jitter
    /// value is added to the delay.
    ///
    /// # Arguments
    ///
    /// * `attempt`: The number of the current attempt (1-indexed).
    pub(crate) async fn delay(&self, attempt: u32) {
        let delay = match self.backoff_type {
            RetryBackoffType::Double => self.delay_ms * 2u32.pow(attempt - 1),
            RetryBackoffType::Constant => self.delay_ms,
            RetryBackoffType::Exponential => self.delay_ms * attempt,
        };

        if let Some(jitter) = self.jitter {
            let jitter = rand::random::<u64>() % jitter.as_millis() as u64;
            sleep(delay + Duration::from_millis(jitter)).await;
        } else {
            sleep(delay).await;
        }
    }
}

#[derive(Debug, Clone)]
pub struct SendData {
    pub tracer_payloads: Vec<pb::TracerPayload>,
    pub size: usize, // have a rough size estimate to force flushing if it's large
    pub target: Endpoint,
    headers: HashMap<&'static str, String>,
    retry_strategy: RetryStrategy,
}

impl SendData {
    pub fn new(
        size: usize,
        tracer_payload: pb::TracerPayload,
        tracer_header_tags: TracerHeaderTags,
        target: &Endpoint,
    ) -> SendData {
        let headers = if let Some(api_key) = &target.api_key {
            HashMap::from([("DD-API-KEY", api_key.as_ref().to_string())])
        } else {
            tracer_header_tags.into()
        };

        SendData {
            tracer_payloads: vec![tracer_payload],
            size,
            target: target.clone(),
            headers,
            retry_strategy: RetryStrategy::default(),
        }
    }

    /// Overrides the default RetryStrategy with user-defined values.
    ///
    /// # Arguments
    ///
    /// * `retry_strategy`: The new retry strategy to be used.
    pub fn set_retry_strategy(&mut self, retry_strategy: RetryStrategy) {
        self.retry_strategy = retry_strategy;
    }

    pub async fn send(self) -> SendDataResult {
        if self.use_protobuf() {
            self.send_with_protobuf().await
        } else {
            self.send_with_msgpack().await
        }
    }

    // This function wraps send_data with a retry strategy and the building of the request.
    // Hyper doesn't allow you to send a ref to a request, and you can't clone it. So we have to
    // build a new one for every send attempt.
    async fn send_payload(
        &self,
        content_type: &'static str,
        payload: Vec<u8>,
    ) -> Result<Response<Body>, SendRequestError> {
        let mut request_attempt = 0;

        loop {
            request_attempt += 1;
            let mut req = self.create_request_builder();
            req = req.header("Content-type", content_type);

            let result = self.send_request(req, payload.clone()).await;

            // If the request was successful, or if we have exhausted retries then return the
            // result. Otherwise, delay and try again.
            match &result {
                Ok(response) => {
                    if response.status().is_client_error() || response.status().is_server_error() {
                        if request_attempt >= self.retry_strategy.max_retries {
                            return result;
                        } else {
                            self.retry_strategy.delay(request_attempt).await;
                        }
                    } else {
                        return result;
                    }
                }
                Err(_) => {
                    if request_attempt >= self.retry_strategy.max_retries {
                        return result;
                    } else {
                        self.retry_strategy.delay(request_attempt).await;
                    }
                }
            }
        }
    }

    fn use_protobuf(&self) -> bool {
        self.target.api_key.is_some()
    }

    fn create_request_builder(&self) -> HttpRequestBuilder {
        let mut req = hyper::Request::builder()
            .uri(self.target.url.clone())
            .header(
                hyper::header::USER_AGENT,
                concat!("Tracer/", env!("CARGO_PKG_VERSION")),
            )
            .method(Method::POST);

        for (key, value) in &self.headers {
            req = req.header(*key, value);
        }

        req
    }

    async fn send_request(
        &self,
        req: HttpRequestBuilder,
        payload: Vec<u8>,
    ) -> Result<Response<Body>, SendRequestError> {
        let req = req
            .body(Body::from(payload))
            .map_err(|e| SendRequestError::Any(anyhow!(e)))?;

        Client::builder()
            .build(connector::Connector::default())
            .request(req)
            .await
            .map_err(SendRequestError::Hyper)
    }

    async fn send_with_protobuf(&self) -> SendDataResult {
        let mut result = SendDataResult::new();

        let agent_payload = construct_agent_payload(self.tracer_payloads.clone());
        let serialized_trace_payload = match serialize_proto_payload(&agent_payload)
            .context("Failed to serialize trace agent payload, dropping traces")
        {
            Ok(p) => p,
            Err(e) => return result.error(e),
        };

        result
            .update(
                self.send_payload("application/x-protobuf", serialized_trace_payload)
                    .await,
                StatusCode::ACCEPTED,
            )
            .await;

        result
    }

    async fn send_with_msgpack(&self) -> SendDataResult {
        let mut result = SendDataResult::new();

        let mut req = self.create_request_builder();
        req = req.header("Content-type", "application/msgpack");

        let (template, _) = req.body(()).unwrap().into_parts();

        let mut futures = FuturesUnordered::new();
        for tracer_payload in self.tracer_payloads.clone().into_iter() {
            let mut builder = HttpRequestBuilder::new()
                .method(template.method.clone())
                .uri(template.uri.clone())
                .version(template.version)
                .header(
                    "X-Datadog-Trace-Count",
                    tracer_payload.chunks.len().to_string(),
                );
            builder
                .headers_mut()
                .unwrap()
                .extend(template.headers.clone());

            let payload = match rmp_serde::to_vec_named(&tracer_payload) {
                Ok(p) => p,
                Err(e) => return result.error(anyhow!(e)),
            };
            futures.push(self.send_payload("application/msgpack", payload));
        }
        loop {
            match futures.next().await {
                Some(response) => {
                    result.update(response, StatusCode::OK).await;
                    if result.last_result.is_err() {
                        return result;
                    }
                }
                None => return result,
            }
        }
    }
}

#[cfg(test)]
// For RetryStrategy tests the observed delay should be approximate.
// There may be a small amount of overhead, so we check that the elapsed time is within
// a tolerance of the expected delay.
// TODO: APMSP-1079 - We should have more comprehensive tests for SendData logic beyond retry logic.
mod tests {
    use super::*;
    use httpmock::{Mock, MockServer};
    use std::time::Duration;
    use tokio::time::Instant;

    const RETRY_STRATEGY_TIME_TOLERANCE_MS: u64 = 25;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_strategy_constant() {
        let retry_strategy = RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Constant,
            jitter: None,
        };

        let start = Instant::now();
        retry_strategy.delay(1).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time was not within expected range"
        );

        let start = Instant::now();
        retry_strategy.delay(2).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time was not within expected range"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_strategy_double() {
        let retry_strategy = RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Double,
            jitter: None,
        };

        let start = Instant::now();
        retry_strategy.delay(1).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time was not within expected range"
        );

        let start = Instant::now();
        retry_strategy.delay(2).await;
        let elapsed = start.elapsed();

        // For the Double strategy, the delay for the second attempt should be double the delay_ms.
        assert!(
            elapsed >= retry_strategy.delay_ms * 2
                && elapsed
                    <= retry_strategy.delay_ms * 2
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time was not within expected range"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_strategy_exponential() {
        let retry_strategy = RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Exponential,
            jitter: None,
        };

        let start = Instant::now();
        retry_strategy.delay(1).await;
        let elapsed = start.elapsed();

        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time was not within expected range"
        );

        let start = Instant::now();
        retry_strategy.delay(3).await;
        let elapsed = start.elapsed();

        // For the Exponential strategy, the delay for the second attempt should be double the
        // delay_ms.
        assert!(
            elapsed >= retry_strategy.delay_ms * 3
                && elapsed
                    <= retry_strategy.delay_ms * 3
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time was not within expected range"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_retry_strategy_jitter() {
        let retry_strategy = RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Constant,
            jitter: Some(Duration::from_millis(50)),
        };

        let start = Instant::now();
        retry_strategy.delay(1).await;
        let elapsed = start.elapsed();

        // The delay should be between delay_ms and delay_ms + jitter
        assert!(
            elapsed >= retry_strategy.delay_ms
                && elapsed
                    <= retry_strategy.delay_ms
                        + retry_strategy.jitter.unwrap()
                        + Duration::from_millis(RETRY_STRATEGY_TIME_TOLERANCE_MS),
            "Elapsed time was not within expected range"
        );
    }

    // TODO: APMSP-1153 - This function also exists in
    // sidecar::service::tracing::trace_flusher::tests. It should be moved to a common
    // trace_test_utils module when it is properly gated to just test dependency.
    async fn poll_for_mock_hit(
        mock: &mut Mock<'_>,
        poll_attempts: i32,
        sleep_interval_ms: u64,
        expected_hits: usize,
        delete_after_hit: bool,
    ) -> bool {
        let mut mock_hit = mock.hits_async().await == expected_hits;

        let mut mock_observations_remaining = poll_attempts;

        while !mock_hit {
            sleep(Duration::from_millis(sleep_interval_ms)).await;
            mock_hit = mock.hits_async().await == expected_hits;
            mock_observations_remaining -= 1;
            if mock_observations_remaining == 0 || mock_hit {
                if delete_after_hit {
                    mock.delete();
                }
                break;
            }
        }

        mock_hit
    }

    // TODO: APMSP-1153 - This function also exists in
    // sidecar::service::tracing::trace_flusher::tests. It should be moved to a common
    // trace_test_utils module when it is properly gated to just test dependency.
    fn create_send_data(size: usize, target_endpoint: &Endpoint) -> SendData {
        let tracer_header_tags = TracerHeaderTags::default();

        let tracer_payload = pb::TracerPayload {
            container_id: "container_id_1".to_owned(),
            language_name: "php".to_owned(),
            language_version: "4.0".to_owned(),
            tracer_version: "1.1".to_owned(),
            runtime_id: "runtime_1".to_owned(),
            chunks: vec![],
            tags: Default::default(),
            env: "test".to_owned(),
            hostname: "test_host".to_owned(),
            app_version: "2.0".to_owned(),
        };

        SendData::new(size, tracer_payload, tracer_header_tags, target_endpoint)
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
        };

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy {
            max_retries: 0,
            delay_ms: Duration::from_millis(2),
            backoff_type: RetryBackoffType::Constant,
            jitter: None,
        });

        tokio::spawn(async move {
            let result = send_data.send().await;
            assert!(result.last_result.is_err(), "Expected an error result");
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
        };

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy {
            max_retries: 2,
            delay_ms: Duration::from_millis(250),
            backoff_type: RetryBackoffType::Constant,
            jitter: None,
        });

        tokio::spawn(async move {
            let result = send_data.send().await;
            assert!(result.last_result.is_ok(), "Expected a successful result");
        });

        assert!(poll_for_mock_hit(&mut mock_503, 10, 100, 1, true).await);
        assert!(
            poll_for_mock_hit(&mut mock_202, 10, 100, 1, true).await,
            "Expected a retry request after a 5xx error"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    // Ensure at least one test exists for msgpack with retry logic
    async fn test_retry_logic_error_then_success_msgpack() {
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
            api_key: None,
        };

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy {
            max_retries: 2,
            delay_ms: Duration::from_millis(250),
            backoff_type: RetryBackoffType::Constant,
            jitter: None,
        });

        tokio::spawn(async move {
            let result = send_data.send().await;
            assert!(result.last_result.is_ok(), "Expected a successful result");
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
        };

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy {
            max_retries: expected_retry_attempts,
            delay_ms: Duration::from_millis(10),
            backoff_type: RetryBackoffType::Constant,
            jitter: None,
        });

        tokio::spawn(async move {
            send_data.send().await;
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
        };

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy {
            max_retries: 2,
            delay_ms: Duration::from_millis(10),
            backoff_type: RetryBackoffType::Constant,
            jitter: None,
        });

        tokio::spawn(async move {
            send_data.send().await;
        });

        assert!(
            poll_for_mock_hit(&mut mock_202, 10, 250, 1, true).await,
            "Expected only one request attempt"
        );
    }
}
