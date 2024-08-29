// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod retry_strategy;
pub mod send_data_result;

pub use crate::send_data::retry_strategy::{RetryBackoffType, RetryStrategy};

use crate::trace_utils::{SendDataResult, TracerHeaderTags};
use crate::tracer_payload::TracerPayloadCollection;
use anyhow::{anyhow, Context};
use bytes::Bytes;
use datadog_trace_protobuf::pb::{AgentPayload, TracerPayload};
use ddcommon::{connector, Endpoint, HttpRequestBuilder};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use hyper::header::HeaderValue;
use hyper::{Body, Client, HeaderMap, Method, Response};
use std::collections::HashMap;
use std::time::Duration;

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
    TimeoutSocket,
    TimeoutApi,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestError::TimeoutSocket => write!(f, "Socket timed out"),
            RequestError::TimeoutApi => write!(f, "Api timeout exhausted"),
            RequestError::Network => write!(f, "Network error"),
            RequestError::Build => write!(f, "Request failed due to invalid property"),
        }
    }
}

impl std::error::Error for RequestError {}

pub(crate) enum RequestResult {
    /// Holds information from a successful request.
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

#[derive(Debug, Clone)]
/// `SendData` is a structure that holds the data to be sent to a target endpoint.
/// It includes the payloads to be sent, the size of the data, the target endpoint,
/// headers for the request, and a retry strategy for sending the data.
///
/// # Example
///
/// ```rust
/// use datadog_trace_protobuf::pb::TracerPayload;
/// use datadog_trace_utils::send_data::{
///     retry_strategy::{RetryBackoffType, RetryStrategy},
///     SendData,
/// };
/// use datadog_trace_utils::trace_utils::TracerHeaderTags;
/// use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
/// use ddcommon::Endpoint;
///
/// #[cfg_attr(miri, ignore)]
/// async fn update_send_results_example() {
///     let size = 100;
///     let tracer_payload = TracerPayloadCollection::V07(
///         vec![TracerPayload::default()]); // Replace with actual payload
///     let tracer_header_tags = TracerHeaderTags::default(); // Replace with actual header tags
///     let target = Endpoint::default(); // Replace with actual endpoint
///
///     let mut send_data = SendData::new(size, tracer_payload, tracer_header_tags, &target);
///
///     // Set a custom retry strategy
///     let retry_strategy = RetryStrategy::new(3, 10, RetryBackoffType::Exponential, Some(5));
///
///     send_data.set_retry_strategy(retry_strategy);
///
///     // Send the data
///     let result = send_data.send().await;
/// }
/// ```
pub struct SendData {
    pub(crate) tracer_payloads: TracerPayloadCollection,
    pub(crate) size: usize, // have a rough size estimate to force flushing if it's large
    target: Endpoint,
    headers: HashMap<&'static str, String>,
    retry_strategy: RetryStrategy,
}

impl SendData {
    /// Creates a new instance of `SendData`.
    ///
    /// # Arguments
    ///
    /// * `size`: Approximate size of the data to be sent in bytes.
    /// * `tracer_payload`: The payload to be sent.
    /// * `tracer_header_tags`: The header tags for the tracer.
    /// * `target`: The endpoint to which the data will be sent.
    ///
    /// # Returns
    ///
    /// A new `SendData` instance.
    pub fn new(
        size: usize,
        tracer_payload: TracerPayloadCollection,
        tracer_header_tags: TracerHeaderTags,
        target: &Endpoint,
    ) -> SendData {
        let mut headers = if let Some(api_key) = &target.api_key {
            HashMap::from([(DD_API_KEY, api_key.as_ref().to_string())])
        } else {
            tracer_header_tags.into()
        };
        if let Some(token) = &target.test_token {
            headers.insert("x-datadog-test-session-token", token.to_string());
        }

        SendData {
            tracer_payloads: tracer_payload,
            size,
            target: target.clone(),
            headers,
            retry_strategy: RetryStrategy::default(),
        }
    }

    /// Returns the user defined approximate size of the data to be sent in bytes.
    ///
    /// # Returns
    ///
    /// The size of the data.
    pub fn len(&self) -> usize {
        self.size
    }

    /// Checks if the user defined approximate size of the data to be sent is zero.
    ///
    /// # Returns
    ///
    /// `true` if size is 0, `false` otherwise.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Returns the target endpoint.
    ///
    /// # Returns
    ///
    /// A reference to the target endpoint.
    pub fn get_target(&self) -> &Endpoint {
        &self.target
    }

    /// Returns the payloads to be sent.
    ///
    /// # Returns
    ///
    /// A reference to the vector of payloads.
    pub fn get_payloads(&self) -> &TracerPayloadCollection {
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

    /// Sends the data to the target endpoint.
    ///
    /// # Returns
    ///
    /// A `SendDataResult` instance containing the result of the operation.
    pub async fn send(&self) -> SendDataResult {
        if self.use_protobuf() {
            self.send_with_protobuf().await
        } else {
            self.send_with_msgpack().await
        }
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

        match tokio::time::timeout(
            Duration::from_millis(self.target.timeout_ms),
            Client::builder()
                .build(connector::Connector::default())
                .request(req),
        )
        .await
        {
            Ok(resp) => match resp {
                Ok(body) => Ok(body),
                Err(e) => {
                    if e.is_timeout() {
                        Err(RequestError::TimeoutSocket)
                    } else {
                        Err(RequestError::Network)
                    }
                }
            },
            Err(_) => Err(RequestError::TimeoutApi),
        }
    }

    fn use_protobuf(&self) -> bool {
        self.target.api_key.is_some()
    }

    // This function wraps send_data with a retry strategy and the building of the request.
    // Hyper doesn't allow you to send a ref to a request, and you can't clone it. So we have to
    // build a new one for every send attempt. Being of type Bytes, the payload.clone() is not doing
    // a deep clone.
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
            req.headers_mut()
                .expect("HttpRequestBuilder unable to get headers for request")
                .extend(headers.clone());

            match self.send_request(req, payload.clone()).await {
                // An Ok response doesn't necessarily mean the request was successful, we need to
                // check the status code and if it's not a 2xx or 3xx we treat it as an error
                Ok(response) => {
                    let request_result = self.build_request_result_from_ok_response(
                        response,
                        request_attempt,
                        payload_chunks,
                        payload.len(),
                    );
                    match request_result {
                        RequestResult::Error(_)
                            if request_attempt < self.retry_strategy.max_retries() =>
                        {
                            self.retry_strategy.delay(request_attempt).await;
                            continue;
                        }
                        _ => return request_result,
                    }
                }
                Err(e) => {
                    if request_attempt >= self.retry_strategy.max_retries() {
                        return self.handle_request_error(e, request_attempt, payload_chunks);
                    } else {
                        self.retry_strategy.delay(request_attempt).await;
                    }
                }
            }
        }
    }

    fn build_request_result_from_ok_response(
        &self,
        response: Response<Body>,
        request_attempt: Attempts,
        payload_chunks: ChunksSent,
        payload_len: usize,
    ) -> RequestResult {
        if response.status().is_client_error() || response.status().is_server_error() {
            RequestResult::Error((response, request_attempt, payload_chunks))
        } else {
            RequestResult::Success((
                response,
                request_attempt,
                u64::try_from(payload_len).unwrap(),
                payload_chunks,
            ))
        }
    }

    fn handle_request_error(
        &self,
        e: RequestError,
        request_attempt: Attempts,
        payload_chunks: ChunksDropped,
    ) -> RequestResult {
        match e {
            RequestError::Build => RequestResult::BuildError((request_attempt, payload_chunks)),
            RequestError::Network => RequestResult::NetworkError((request_attempt, payload_chunks)),
            RequestError::TimeoutSocket => {
                RequestResult::TimeoutError((request_attempt, payload_chunks))
            }
            RequestError::TimeoutApi => {
                RequestResult::TimeoutError((request_attempt, payload_chunks))
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
        let chunks = u64::try_from(self.tracer_payloads.size()).unwrap();

        match &self.tracer_payloads {
            TracerPayloadCollection::V07(payloads) => {
                let agent_payload = construct_agent_payload(payloads.to_vec());
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
            _ => result,
        }
    }

    async fn send_with_msgpack(&self) -> SendDataResult {
        let mut result = SendDataResult::default();
        let mut futures = FuturesUnordered::new();

        match &self.tracer_payloads {
            TracerPayloadCollection::V07(payloads) => {
                for tracer_payload in payloads {
                    let chunks = u64::try_from(tracer_payload.chunks.len()).unwrap();
                    let additional_payload_headers =
                        Some(HashMap::from([(HEADER_DD_TRACE_COUNT, chunks.to_string())]));

                    let payload = match rmp_serde::to_vec_named(tracer_payload) {
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
            }
            TracerPayloadCollection::V04(payloads) => {
                let chunks = u64::try_from(self.tracer_payloads.size()).unwrap();
                let headers = Some(HashMap::from([(HEADER_DD_TRACE_COUNT, chunks.to_string())]));

                let payload = match rmp_serde::to_vec_named(payloads) {
                    Ok(p) => p,
                    Err(e) => return result.error(anyhow!(e)),
                };

                futures.push(self.send_payload(HEADER_CTYPE_MSGPACK, payload, chunks, headers));
            }
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

fn construct_agent_payload(tracer_payloads: Vec<TracerPayload>) -> AgentPayload {
    AgentPayload {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::send_data::retry_strategy::RetryBackoffType;
    use crate::send_data::retry_strategy::RetryStrategy;
    use crate::test_utils::{create_send_data, create_test_no_alloc_span, create_test_span, poll_for_mock_hit};
    use crate::trace_utils::{construct_trace_chunk, construct_tracer_payload, RootSpanTags};
    use crate::tracer_header_tags::TracerHeaderTags;
    use datadog_trace_protobuf::pb::Span;
    use ddcommon::Endpoint;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use std::collections::HashMap;

    const ONE_SECOND: u64 = 1_000;
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

    fn setup_payload(header_tags: &TracerHeaderTags) -> TracerPayload {
        let root_tags = RootSpanTags {
            env: "TEST",
            app_version: "1.0",
            hostname: "test_bench",
            runtime_id: "id",
        };

        let chunk = construct_trace_chunk(vec![Span {
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

    fn compute_payload_len(collection: &TracerPayloadCollection) -> usize {
        match collection {
            TracerPayloadCollection::V07(payloads) => {
                let agent_payload = construct_agent_payload(payloads.to_vec());
                let serialized_trace_payload = serialize_proto_payload(&agent_payload).unwrap();
                serialized_trace_payload.len()
            }
            _ => 0,
        }
    }

    fn rmp_compute_payload_len(collection: &TracerPayloadCollection) -> usize {
        match collection {
            TracerPayloadCollection::V07(payloads) => {
                let mut total: usize = 0;
                for payload in payloads {
                    total += rmp_serde::to_vec_named(payload).unwrap().len();
                }
                total
            }
            TracerPayloadCollection::V04(payloads) => {
                rmp_serde::to_vec_named(payloads).unwrap().len()
            }
        }
    }

    #[test]
    fn error_format() {
        assert_eq!(
            RequestError::Build.to_string(),
            "Request failed due to invalid property"
        );
        assert_eq!(RequestError::Network.to_string(), "Network error");
        assert_eq!(RequestError::TimeoutSocket.to_string(), "Socket timed out");
        assert_eq!(
            RequestError::TimeoutApi.to_string(),
            "Api timeout exhausted"
        );
    }

    #[test]
    fn send_data_new_api_key() {
        let header_tags = TracerHeaderTags::default();

        let payload = setup_payload(&header_tags);
        let data = SendData::new(
            100,
            TracerPayloadCollection::V07(vec![payload]),
            HEADER_TAGS,
            &Endpoint {
                api_key: Some(std::borrow::Cow::Borrowed("TEST-KEY")),
                url: "/foo/bar?baz".parse::<hyper::Uri>().unwrap(),
                timeout_ms: ONE_SECOND,
                ..Endpoint::default()
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
            TracerPayloadCollection::V07(vec![payload]),
            header_tags.clone(),
            &Endpoint {
                api_key: None,
                url: "/foo/bar?baz".parse::<hyper::Uri>().unwrap(),
                timeout_ms: ONE_SECOND,
                ..Endpoint::default()
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
            TracerPayloadCollection::V07(vec![payload.clone()]),
            header_tags,
            &Endpoint {
                api_key: Some(std::borrow::Cow::Borrowed("TEST-KEY")),
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
                timeout_ms: ONE_SECOND,
                ..Endpoint::default()
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
        let data = SendData::new(
            100,
            TracerPayloadCollection::V07(vec![payload.clone(), payload.clone()]),
            header_tags,
            &Endpoint {
                api_key: Some(std::borrow::Cow::Borrowed("TEST-KEY")),
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
                timeout_ms: ONE_SECOND,
                ..Endpoint::default()
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
        assert_eq!(res.chunks_sent, 2);
        assert_eq!(res.bytes_sent, data_payload_len as u64);
        assert_eq!(*res.responses_count_per_code.get(&202).unwrap(), 1_u64);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_msgpack_v07() {
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
            TracerPayloadCollection::V07(vec![payload.clone()]),
            header_tags,
            &Endpoint {
                api_key: None,
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
                timeout_ms: ONE_SECOND,
                ..Endpoint::default()
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
    async fn request_msgpack_v04() {
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

        let trace = vec![create_test_no_alloc_span(1234, 12342, 12341, 1, false)];
        let data = SendData::new(
            100,
            TracerPayloadCollection::V04(vec![trace.clone()]),
            header_tags,
            &Endpoint {
                api_key: None,
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
                timeout_ms: ONE_SECOND,
                ..Endpoint::default()
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
        let data = SendData::new(
            100,
            TracerPayloadCollection::V07(vec![payload.clone(), payload.clone()]),
            header_tags,
            &Endpoint {
                api_key: None,
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
                timeout_ms: ONE_SECOND,
                ..Endpoint::default()
            },
        );

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
            TracerPayloadCollection::V07(vec![payload]),
            HEADER_TAGS,
            &Endpoint {
                api_key: None,
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
                timeout_ms: ONE_SECOND,
                ..Endpoint::default()
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
            TracerPayloadCollection::V07(vec![payload]),
            HEADER_TAGS,
            &Endpoint {
                api_key: None,
                url: "http://127.0.0.1:4321/".parse::<hyper::Uri>().unwrap(),
                timeout_ms: ONE_SECOND,
                ..Endpoint::default()
            },
        );

        let res = data.send().await;

        assert!(res.last_result.is_err());
        match std::env::consts::OS {
            "windows" => {
                // On windows the TCP/IP stack returns a timeout error (at hyper level) rather
                // than a connection refused error despite not having a listening socket on the
                // port.
                assert_eq!(res.errors_timeout, 1);
                assert_eq!(res.errors_network, 0);
            }
            _ => {
                assert_eq!(res.errors_timeout, 0);
                assert_eq!(res.errors_network, 1);
            }
        }
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.requests_count, 5);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.chunks_sent, 0);
        assert_eq!(res.bytes_sent, 0);
        assert_eq!(res.responses_count_per_code.len(), 0);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_error_timeout_v04() {
        let server = MockServer::start_async().await;

        let header_tags = HEADER_TAGS;
        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header(HEADER_DD_TRACE_COUNT, "2")
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
                then.status(200).body("").delay(Duration::from_millis(500));
            })
            .await;

        let header_tags = HEADER_TAGS;

        let trace = vec![create_test_no_alloc_span(1234, 12342, 12341, 1, false)];
        let data = SendData::new(
            100,
            TracerPayloadCollection::V04(vec![trace.clone(), trace.clone()]),
            header_tags,
            &Endpoint {
                api_key: None,
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
                timeout_ms: 200,
                ..Endpoint::default()
            },
        );

        let res = data.send().await;

        mock.assert_hits_async(5).await;

        assert_eq!(res.errors_timeout, 1);
        assert_eq!(res.errors_network, 0);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.requests_count, 5);
        assert_eq!(res.chunks_sent, 0);
        assert_eq!(res.bytes_sent, 0);
        assert_eq!(res.responses_count_per_code.len(), 0);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_error_timeout_v07() {
        let server = MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/msgpack")
                    .path("/");
                then.status(200).body("").delay(Duration::from_millis(500));
            })
            .await;

        let header_tags = TracerHeaderTags::default();

        let payload = setup_payload(&header_tags);
        let data = SendData::new(
            100,
            TracerPayloadCollection::V07(vec![payload.clone(), payload.clone()]),
            header_tags,
            &Endpoint {
                api_key: None,
                url: server.url("/").parse::<hyper::Uri>().unwrap(),
                timeout_ms: 200,
                ..Endpoint::default()
            },
        );

        let res = data.send().await;

        mock.assert_hits_async(10).await;

        assert_eq!(res.errors_timeout, 1);
        assert_eq!(res.errors_network, 0);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.requests_count, 5);
        assert_eq!(res.chunks_sent, 0);
        assert_eq!(res.bytes_sent, 0);
        assert_eq!(res.responses_count_per_code.len(), 0);
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
            ..Default::default()
        };

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy::new(0, 2, RetryBackoffType::Constant, None));

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
            ..Default::default()
        };

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy::new(2, 250, RetryBackoffType::Constant, None));

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

        let target_endpoint = Endpoint::from_slice(server.url("").as_str());

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy::new(2, 250, RetryBackoffType::Constant, None));

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
            ..Default::default()
        };

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy::new(
            expected_retry_attempts,
            10,
            RetryBackoffType::Constant,
            None,
        ));

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
            ..Default::default()
        };

        let size = 512;

        let mut send_data = create_send_data(size, &target_endpoint);
        send_data.set_retry_strategy(RetryStrategy::new(2, 10, RetryBackoffType::Constant, None));

        tokio::spawn(async move {
            send_data.send().await;
        });

        assert!(
            poll_for_mock_hit(&mut mock_202, 10, 250, 1, true).await,
            "Expected only one request attempt"
        );
    }
}
