// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{anyhow, Context};
use bytes::Bytes;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use hyper::{
    header::{HeaderMap, HeaderValue},
    Body, Client, Method, Response,
};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

use crate::tracer_header_tags::TracerHeaderTags;
use datadog_trace_protobuf::pb::{self, TracerPayload};
use ddcommon::{connector, Endpoint, HttpRequestBuilder};

const DD_API_KEY: &str = "DD-API-KEY";
const HEADER_DD_TRACE_COUNT: &str = "X-Datadog-Trace-Count";
const HEADER_HTTP_CTYPE: &str = "Content-Type";
const HEADER_CTYPE_MSGPACK: &str = "application/msgpack";
const HEADER_CTYPE_PROTOBUF: &str = "application/x-protobuf";

type BytesSent = u64;
type ChunksSent = u64;
type ChunksDropped = u64;
type Attempts = u32;

#[derive(Debug)]
enum RequestError {
    Build,
    Network,
    Timeout,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::Timeout => write!(f, "Connection timed out"),
            RequestError::Network => write!(f, "Network error"),
            RequestError::Build => write!(f, "Request failed due to invalid property"),
        }
    }
}

impl std::error::Error for RequestError {}

pub enum RequestResult {
    /// Holds information from a succesful request.
    Success((Response<Body>, Attempts, BytesSent, ChunksSent)),
    /// Treats HTTP errors.
    Error((Response<Body>, Attempts, ChunksDropped)),
    /// Treats timeout errors originated in the transport layer.
    TimeoutError((Attempts, ChunksDropped)),
    /// Treats errors coming from networking.
    NetworkError((Attempts, ChunksDropped)),
    /// Treats errors coming from building the request
    BuildError((Attempts, ChunksDropped)),
}

#[derive(Debug)]
pub struct SendDataResult {
    // Keeps track of the last request result.
    pub last_result: anyhow::Result<Response<Body>>,
    // Count metric for 'trace_api.requests'.
    pub requests_count: u64,
    // Count metric for 'trace_api.responses'. Each key maps  a different HTTP status code.
    pub responses_count_per_code: HashMap<u16, u64>,
    // Count metric for 'trace_api.errors' (type: timeout).
    pub errors_timeout: u64,
    // Count metric for 'trace_api.errors' (type: network).
    pub errors_network: u64,
    // Count metric for 'trace_api.errors' (type: status_code).
    pub errors_status_code: u64,
    // Count metric for 'trace_api.bytes'
    pub bytes_sent: u64,
    // Count metric for 'trace_chunk_sent'
    pub chunks_sent: u64,
    // Count metric for 'trace_chunks_dropped'
    pub chunks_dropped: u64,
}

impl Default for SendDataResult {
    fn default() -> Self {
        SendDataResult {
            last_result: Err(anyhow!("No requests sent")),
            requests_count: 0,
            responses_count_per_code: Default::default(),
            errors_timeout: 0,
            errors_network: 0,
            errors_status_code: 0,
            bytes_sent: 0,
            chunks_sent: 0,
            chunks_dropped: 0,
        }
    }
}

impl SendDataResult {
    ///
    /// Updates `SendDataResult` internal information with the request's result information.
    ///
    /// # Arguments
    ///
    /// * `res` - Request result.
    ///
    /// # Examples
    ///
    /// ```
    /// use datadog_trace_utils::send_data::RequestResult;
    /// use datadog_trace_utils::trace_utils::SendDataResult;
    ///
    /// #[cfg_attr(miri, ignore)]
    /// async fn update_send_results_example() {
    ///     let result = RequestResult::NetworkError((1, 0));
    ///     let mut data_result = SendDataResult::default();
    ///     data_result.update(result).await;
    /// }
    /// ```

    pub async fn update(&mut self, res: RequestResult) {
        match res {
            RequestResult::Success((response, attempts, bytes, chunks)) => {
                *self
                    .responses_count_per_code
                    .entry(response.status().as_u16())
                    .or_default() += 1;
                self.bytes_sent += bytes;
                self.chunks_sent += chunks;
                self.last_result = Ok(response);
                self.requests_count += u64::from(attempts);
            }
            RequestResult::Error((response, attempts, chunks)) => {
                let status_code = response.status().as_u16();
                self.errors_status_code += 1;
                *self
                    .responses_count_per_code
                    .entry(status_code)
                    .or_default() += 1;
                self.chunks_dropped += chunks;
                self.requests_count += u64::from(attempts);

                let body_bytes = hyper::body::to_bytes(response.into_body()).await;
                let response_body =
                    String::from_utf8(body_bytes.unwrap_or_default().to_vec()).unwrap_or_default();
                self.last_result = Err(anyhow::format_err!(
                    "{} - Server did not accept traces: {}",
                    status_code,
                    response_body,
                ));
            }
            RequestResult::TimeoutError((attempts, chunks)) => {
                self.errors_timeout += 1;
                self.chunks_dropped += chunks;
                self.requests_count += u64::from(attempts);
            }
            RequestResult::NetworkError((attempts, chunks)) => {
                self.errors_network += 1;
                self.chunks_dropped += chunks;
                self.requests_count += u64::from(attempts);
            }
            RequestResult::BuildError((attempts, chunks)) => {
                self.chunks_dropped += chunks;
                self.requests_count += u64::from(attempts);
            }
        }
    }

    ///
    /// Sets `SendDataResult` last result information.
    /// expected result.
    ///
    /// # Arguments
    ///
    /// * `err` - Error to be set.
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
    /// Increases the delay by a fixed increment each attempt.
    Linear,
    /// The delay is constant for each attempt.
    Constant,
    /// The delay is doubled for each attempt.
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
///     backoff_type: RetryBackoffType::Exponential,
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
            backoff_type: RetryBackoffType::Exponential,
            jitter: None,
        }
    }
}

impl RetryStrategy {
    /// Delays the next request attempt based on the retry strategy.
    ///
    /// If a jitter duration is specified in the retry strategy, a random duration up to the jitter
    /// value is added to the delay.
    ///
    /// # Arguments
    ///
    /// * `attempt`: The number of the current attempt (1-indexed).
    pub(crate) async fn delay(&self, attempt: u32) {
        let delay = match self.backoff_type {
            RetryBackoffType::Exponential => self.delay_ms * 2u32.pow(attempt - 1),
            RetryBackoffType::Constant => self.delay_ms,
            RetryBackoffType::Linear => self.delay_ms + (self.delay_ms * (attempt - 1)),
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
    pub(crate) tracer_payloads: Vec<pb::TracerPayload>,
    pub(crate) size: usize, // have a rough size estimate to force flushing if it's large
    target: Endpoint,
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
            HashMap::from([(DD_API_KEY, api_key.as_ref().to_string())])
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

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    pub fn get_target(&self) -> &Endpoint {
        &self.target
    }

    pub fn get_payloads(&self) -> &Vec<TracerPayload> {
        &self.tracer_payloads
    }

    /// Overrides the default RetryStrategy with user-defined values.
    ///
    /// # Arguments
    ///
    /// * `retry_strategy`: The new retry strategy to be used.
    pub fn set_retry_strategy(&mut self, retry_strategy: RetryStrategy) {
        self.retry_strategy = retry_strategy;
    }

    async fn send_request(
        &self,
        req: HttpRequestBuilder,
        payload: Bytes,
    ) -> Result<Response<Body>, RequestError> {
        let req = match req.body(Body::from(payload)) {
            Ok(req) => req,
            Err(_) => return Err(RequestError::Build),
        };

        match Client::builder()
            .build(connector::Connector::default())
            .request(req)
            .await
        {
            Ok(resp) => Ok(resp),
            Err(e) => {
                if e.is_timeout() {
                    Err(RequestError::Timeout)
                } else {
                    Err(RequestError::Network)
                }
            }
        }
    }

    fn use_protobuf(&self) -> bool {
        self.target.api_key.is_some()
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
        payload_chunks: u64,
        // For payload specific headers that need to be added to the request like trace count.
        additional_payload_headers: Option<HashMap<&'static str, String>>,
    ) -> RequestResult {
        let mut request_attempt = 0;
        let payload = Bytes::from(payload);

        let mut headers = HeaderMap::new();
        headers.insert(HEADER_HTTP_CTYPE, HeaderValue::from_static(content_type));

        if let Some(additional_payload_headers) = &additional_payload_headers {
            for (key, value) in additional_payload_headers {
                headers.insert(*key, HeaderValue::from_str(value).unwrap());
            }
        }

        loop {
            request_attempt += 1;
            let mut req = self.create_request_builder();
            req.headers_mut().unwrap().extend(headers.clone());
            let result = self.send_request(req, payload.clone()).await;

            // If the request was successful, or if we have exhausted retries then return the
            // result. Otherwise, delay and try again.
            match result {
                Ok(response) => {
                    if response.status().is_client_error() || response.status().is_server_error() {
                        if request_attempt >= self.retry_strategy.max_retries {
                            return RequestResult::Error((
                                response,
                                request_attempt,
                                payload_chunks,
                            ));
                        } else {
                            self.retry_strategy.delay(request_attempt).await;
                        }
                    } else {
                        return RequestResult::Success((
                            response,
                            request_attempt,
                            u64::try_from(payload.len()).unwrap(),
                            payload_chunks,
                        ));
                    }
                }
                Err(e) => {
                    if request_attempt >= self.retry_strategy.max_retries {
                        return match e {
                            RequestError::Build => {
                                RequestResult::BuildError((request_attempt, payload_chunks))
                            }
                            RequestError::Network => {
                                RequestResult::NetworkError((request_attempt, payload_chunks))
                            }
                            RequestError::Timeout => {
                                RequestResult::TimeoutError((request_attempt, payload_chunks))
                            }
                        };
                    } else {
                        self.retry_strategy.delay(request_attempt).await;
                    }
                }
            }
        }
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

    async fn send_with_protobuf(&self) -> SendDataResult {
        let mut result = SendDataResult::default();
        let mut chunks: u64 = 0;
        for tracer_payload in &self.tracer_payloads {
            chunks += u64::try_from(tracer_payload.chunks.len()).unwrap();
        }
        let agent_payload = construct_agent_payload(self.tracer_payloads.clone());
        let serialized_trace_payload = match serialize_proto_payload(&agent_payload)
            .context("Failed to serialize trace agent payload, dropping traces")
        {
            Ok(p) => p,
            Err(e) => return result.error(e),
        };

        result
            .update(
                self.send_payload(
                    HEADER_CTYPE_PROTOBUF,
                    serialized_trace_payload,
                    chunks,
                    None,
                )
                .await,
            )
            .await;

        result
    }

    async fn send_with_msgpack(&self) -> SendDataResult {
        let mut result = SendDataResult::default();

        let mut futures = FuturesUnordered::new();
        for tracer_payload in self.tracer_payloads.iter() {
            let chunks = u64::try_from(tracer_payload.chunks.len()).unwrap();
            let additional_payload_headers =
                Some(HashMap::from([(HEADER_DD_TRACE_COUNT, chunks.to_string())]));

            let payload = match rmp_serde::to_vec_named(&tracer_payload) {
                Ok(p) => p,
                Err(e) => return result.error(anyhow!(e)),
            };
            futures.push(self.send_payload(
                HEADER_CTYPE_MSGPACK,
                payload,
                chunks,
                additional_payload_headers,
            ));
        }
        loop {
            match futures.next().await {
                Some(response) => {
                    result.update(response).await;
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
mod tests {
    use super::*;
    use crate::test_utils::{create_send_data, poll_for_mock_hit};
    use crate::trace_utils::{construct_trace_chunk, construct_tracer_payload, RootSpanTags};
    use crate::tracer_header_tags::TracerHeaderTags;
    use datadog_trace_protobuf::pb;
    use ddcommon::Endpoint;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use std::collections::HashMap;
    use tokio::time::Instant;

    const RETRY_STRATEGY_TIME_TOLERANCE_MS: u64 = 25;
    const HEADER_TAGS: TracerHeaderTags = TracerHeaderTags {
        lang: "test-lang",
        lang_version: "2.0",
        lang_interpreter: "interpreter",
        lang_vendor: "vendor",
        tracer_version: "1.0",
        container_id: "id",
        client_computed_top_level: false,
        client_computed_stats: false,
    };

    fn setup_payload(header_tags: &TracerHeaderTags) -> pb::TracerPayload {
        let root_tags = RootSpanTags {
            env: "TEST",
            app_version: "1.0",
            hostname: "test_bench",
            runtime_id: "id",
        };

        let chunk = construct_trace_chunk(vec![pb::Span {
            service: "test-service".to_string(),
            name: "test-service-name".to_string(),
            resource: "test-service-resource".to_string(),
            trace_id: 111,
            span_id: 222,
            parent_id: 333,
            start: 1,
            duration: 5,
            error: 0,
            meta: HashMap::new(),
            metrics: HashMap::new(),
            meta_struct: HashMap::new(),
            r#type: "".to_string(),
            span_links: vec![],
        }]);

        construct_tracer_payload(vec![chunk], header_tags, root_tags)
    }

    fn compute_payload_len(payload: &[pb::TracerPayload]) -> usize {
        let agent_payload = construct_agent_payload(payload.to_vec());
        let serialized_trace_payload = serialize_proto_payload(&agent_payload).unwrap();
        serialized_trace_payload.len()
    }

    fn rmp_compute_payload_len(payload: &Vec<pb::TracerPayload>) -> usize {
        let mut total: usize = 0;
        for payload in payload {
            total += rmp_serde::to_vec_named(payload).unwrap().len();
        }
        total
    }

    #[test]
    fn send_data_new_api_key() {
        let header_tags = TracerHeaderTags::default();

        let payload = setup_payload(&header_tags);
        let data = SendData::new(
            100,
            payload,
            HEADER_TAGS,
            &Endpoint {
                api_key: Some(std::borrow::Cow::Borrowed("TEST-KEY")),
                url: "/foo/bar?baz".parse::<hyper::Uri>().unwrap(),
            },
        );

        assert_eq!(data.size, 100);

        assert_eq!(data.target.api_key.unwrap(), "TEST-KEY");
        assert_eq!(data.target.url.path(), "/foo/bar");

        assert_eq!(data.headers.get("DD-API-KEY").unwrap(), "TEST-KEY");
    }

    #[test]
    fn send_data_new_no_api_key() {
        let header_tags = TracerHeaderTags::default();

        let payload = setup_payload(&header_tags);
        let data = SendData::new(
            100,
            payload,
            header_tags.clone(),
            &Endpoint {
                api_key: None,
                url: "/foo/bar?baz".parse::<hyper::Uri>().unwrap(),
            },
        );

        assert_eq!(data.size, 100);

        assert_eq!(data.target.api_key, None);
        assert_eq!(data.target.url.path(), "/foo/bar");

        assert_eq!(data.headers.get("DD-API-KEY"), None);
        assert_eq!(data.headers, HashMap::from(header_tags));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_protobuf() {
        let server = MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/x-protobuf")
                    .path("/");
                then.status(202).body("");
            })
            .await;

        let header_tags = TracerHeaderTags::default();

        let payload = setup_payload(&header_tags);
        let data = SendData::new(
            100,
            payload.clone(),
            header_tags,
            &Endpoint {
                api_key: Some(std::borrow::Cow::Borrowed("TEST-KEY")),
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
            },
        );

        let data_payload_len = compute_payload_len(&data.tracer_payloads);
        let res = data.send().await;

        mock.assert_async().await;

        assert_eq!(res.last_result.unwrap().status(), 202);
        assert_eq!(res.errors_timeout, 0);
        assert_eq!(res.errors_network, 0);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.requests_count, 1);
        assert_eq!(res.chunks_sent, 1);
        assert_eq!(res.bytes_sent, data_payload_len as u64);
        assert_eq!(*res.responses_count_per_code.get(&202).unwrap(), 1_u64);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_protobuf_several_payloads() {
        let server = MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/x-protobuf")
                    .path("/");
                then.status(202).body("");
            })
            .await;

        let header_tags = TracerHeaderTags::default();

        let payload = setup_payload(&header_tags);
        let mut data = SendData::new(
            100,
            payload.clone(),
            header_tags,
            &Endpoint {
                api_key: Some(std::borrow::Cow::Borrowed("TEST-KEY")),
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
            },
        );

        data.tracer_payloads.push(payload.clone());
        let data_payload_len = compute_payload_len(&data.tracer_payloads);
        let res = data.send().await;

        mock.assert_async().await;

        assert_eq!(res.last_result.unwrap().status(), 202);
        assert_eq!(res.errors_timeout, 0);
        assert_eq!(res.errors_network, 0);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.requests_count, 1);
        assert_eq!(res.chunks_sent, 2);
        assert_eq!(res.bytes_sent, data_payload_len as u64);
        assert_eq!(*res.responses_count_per_code.get(&202).unwrap(), 1_u64);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_msgpack() {
        let server = MockServer::start_async().await;

        let header_tags = HEADER_TAGS;
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header(HEADER_DD_TRACE_COUNT, "1")
                    .header("Content-type", "application/msgpack")
                    .header("datadog-meta-lang", header_tags.lang)
                    .header(
                        "datadog-meta-lang-interpreter",
                        header_tags.lang_interpreter,
                    )
                    .header("datadog-meta-lang-version", header_tags.lang_version)
                    .header("datadog-meta-lang-vendor", header_tags.lang_vendor)
                    .header("datadog-meta-tracer-version", header_tags.tracer_version)
                    .header("datadog-container-id", header_tags.container_id)
                    .path("/");
                then.status(200).body("");
            })
            .await;

        let header_tags = HEADER_TAGS;

        let payload = setup_payload(&header_tags);
        let data = SendData::new(
            100,
            payload.clone(),
            header_tags,
            &Endpoint {
                api_key: None,
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
            },
        );

        let data_payload_len = rmp_compute_payload_len(&data.tracer_payloads);
        let res = data.send().await;

        mock.assert_async().await;

        assert_eq!(res.last_result.unwrap().status(), 200);
        assert_eq!(res.errors_timeout, 0);
        assert_eq!(res.errors_network, 0);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.requests_count, 1);
        assert_eq!(res.chunks_sent, 1);
        assert_eq!(res.bytes_sent, data_payload_len as u64);
        assert_eq!(*res.responses_count_per_code.get(&200).unwrap(), 1_u64);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_msgpack_several_payloads() {
        let server = MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/msgpack")
                    .path("/");
                then.status(200).body("");
            })
            .await;

        let header_tags = TracerHeaderTags::default();

        let payload = setup_payload(&header_tags);
        let mut data = SendData::new(
            100,
            payload.clone(),
            header_tags,
            &Endpoint {
                api_key: None,
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
            },
        );

        data.tracer_payloads.push(payload.clone());
        let data_payload_len = rmp_compute_payload_len(&data.tracer_payloads);
        let res = data.send().await;

        mock.assert_hits_async(2).await;

        assert_eq!(res.last_result.unwrap().status(), 200);
        assert_eq!(res.errors_timeout, 0);
        assert_eq!(res.errors_network, 0);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.requests_count, 2);
        assert_eq!(res.chunks_sent, 2);
        assert_eq!(res.bytes_sent, data_payload_len as u64);
        assert_eq!(*res.responses_count_per_code.get(&200).unwrap(), 2_u64);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_error_status_code() {
        let server = MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/msgpack")
                    .path("/");
                then.status(500).body("");
            })
            .await;

        let payload = setup_payload(&HEADER_TAGS);
        let data = SendData::new(
            100,
            payload,
            HEADER_TAGS,
            &Endpoint {
                api_key: None,
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
            },
        );

        let res = data.send().await;

        mock.assert_hits_async(5).await;

        assert!(res.last_result.is_err());
        assert_eq!(res.errors_timeout, 0);
        assert_eq!(res.errors_network, 0);
        assert_eq!(res.errors_status_code, 1);
        assert_eq!(res.requests_count, 5);
        assert_eq!(res.chunks_sent, 0);
        assert_eq!(res.bytes_sent, 0);
        assert_eq!(*res.responses_count_per_code.get(&500).unwrap(), 1_u64);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_error_network() {
        // Server not created in order to return a 'connection refused' error.
        let payload = setup_payload(&HEADER_TAGS);
        let data = SendData::new(
            100,
            payload,
            HEADER_TAGS,
            &Endpoint {
                api_key: None,
                url: "http://127.0.0.1:4321/".parse::<hyper::Uri>().unwrap(),
            },
        );

        let res = data.send().await;

        assert!(res.last_result.is_err());
        assert_eq!(res.errors_timeout, 0);
        assert_eq!(res.errors_network, 1);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.requests_count, 5);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.chunks_sent, 0);
        assert_eq!(res.bytes_sent, 0);
        assert_eq!(res.responses_count_per_code.len(), 0);
    }

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
    async fn test_retry_strategy_linear() {
        let retry_strategy = RetryStrategy {
            max_retries: 5,
            delay_ms: Duration::from_millis(100),
            backoff_type: RetryBackoffType::Linear,
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

        // For the Linear strategy, the delay for the 3rd attempt should be delay_ms + (delay_ms *
        // 2).
        assert!(
            elapsed >= retry_strategy.delay_ms + (retry_strategy.delay_ms * 2)
                && elapsed
                    <= retry_strategy.delay_ms
                        + (retry_strategy.delay_ms * 2)
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
        // For the Exponential strategy, the delay for the 3rd attempt should be delay_ms * 2^(3-1)
        // = delay_ms * 4.
        assert!(
            elapsed >= retry_strategy.delay_ms * 4
                && elapsed
                    <= retry_strategy.delay_ms * 4
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
