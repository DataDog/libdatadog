// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod send_data_result;

use crate::msgpack_encoder;
use crate::send_with_retry::{send_with_retry, RetryStrategy, SendWithRetryResult};
use crate::trace_utils::TracerHeaderTags;
use crate::tracer_payload::TracerPayloadCollection;
use anyhow::{anyhow, Context};
use ddcommon::HttpClient;
use ddcommon::{
    header::{
        APPLICATION_MSGPACK_STR, APPLICATION_PROTOBUF_STR, DATADOG_SEND_REAL_HTTP_STATUS_STR,
        DATADOG_TRACE_COUNT_STR,
    },
    Endpoint,
};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use hyper::header::CONTENT_TYPE;
use libdd_trace_protobuf::pb::{AgentPayload, TracerPayload};
use send_data_result::SendDataResult;
use std::collections::HashMap;
#[cfg(feature = "compression")]
use std::io::Write;
#[cfg(feature = "compression")]
use zstd::stream::write::Encoder;

#[derive(Debug, Clone)]
/// `SendData` is a structure that holds the data to be sent to a target endpoint.
/// It includes the payloads to be sent, the size of the data, the target endpoint,
/// headers for the request, and a retry strategy for sending the data.
///
/// # Example
///
/// ```rust
/// use libdd_trace_protobuf::pb::TracerPayload;
/// use datadog_trace_utils::send_data::{
///     SendData,
/// };
/// use datadog_trace_utils::send_with_retry::{RetryBackoffType, RetryStrategy};
/// use datadog_trace_utils::trace_utils::TracerHeaderTags;
/// use datadog_trace_utils::tracer_payload::TracerPayloadCollection;
/// use ddcommon::Endpoint;
/// use ddcommon::hyper_migration::new_default_client;
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
///     let client = new_default_client();
///     // Send the data
///     let result = send_data.send(&client).await;
/// }
/// ```
pub struct SendData {
    pub(crate) tracer_payloads: TracerPayloadCollection,
    pub(crate) size: usize, // have a rough size estimate to force flushing if it's large
    target: Endpoint,
    headers: HashMap<&'static str, String>,
    retry_strategy: RetryStrategy,
    #[cfg(feature = "compression")]
    compression: Compression,
}

#[cfg(feature = "compression")]
#[derive(Debug, Clone)]
pub enum Compression {
    Zstd(i32),
    None,
}

#[derive(Clone)]
pub struct SendDataBuilder {
    pub(crate) tracer_payloads: TracerPayloadCollection,
    pub(crate) size: usize,
    target: Endpoint,
    headers: HashMap<&'static str, String>,
    retry_strategy: RetryStrategy,
    #[cfg(feature = "compression")]
    compression: Compression,
}

impl SendDataBuilder {
    pub fn new(
        size: usize,
        tracer_payload: TracerPayloadCollection,
        tracer_header_tags: TracerHeaderTags,
        target: &Endpoint,
    ) -> SendDataBuilder {
        let mut headers: HashMap<&'static str, String> = tracer_header_tags.into();
        headers.insert(DATADOG_SEND_REAL_HTTP_STATUS_STR, "1".to_string());
        SendDataBuilder {
            tracer_payloads: tracer_payload,
            size,
            target: target.clone(),
            headers,
            retry_strategy: RetryStrategy::default(),
            #[cfg(feature = "compression")]
            compression: Compression::None,
        }
    }

    #[cfg(feature = "compression")]
    pub fn with_compression(mut self, compression: Compression) -> SendDataBuilder {
        self.compression = compression;
        self
    }

    pub fn with_api_key(mut self, api_key: &str) -> SendDataBuilder {
        self.target.api_key = Some(api_key.to_string().into());
        self
    }

    pub fn with_retry_strategy(mut self, retry_strategy: RetryStrategy) -> SendDataBuilder {
        self.retry_strategy = retry_strategy;
        self
    }

    pub fn build(self) -> SendData {
        SendData {
            tracer_payloads: self.tracer_payloads,
            size: self.size,
            target: self.target,
            headers: self.headers,
            retry_strategy: self.retry_strategy,
            #[cfg(feature = "compression")]
            compression: self.compression,
        }
    }
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
    #[allow(unused_variables)]
    pub fn new(
        size: usize,
        tracer_payload: TracerPayloadCollection,
        tracer_header_tags: TracerHeaderTags,
        target: &Endpoint,
    ) -> SendData {
        let mut headers: HashMap<&'static str, String> = tracer_header_tags.into();
        headers.insert(DATADOG_SEND_REAL_HTTP_STATUS_STR, "1".to_string());
        SendData {
            tracer_payloads: tracer_payload,
            size,
            target: target.clone(),
            headers,
            retry_strategy: RetryStrategy::default(),
            #[cfg(feature = "compression")]
            compression: Compression::None,
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

    /// Returns a clone of the SendData with the user-defined endpoint.
    ///
    /// # Arguments
    ///
    /// * `endpoint`: The new endpoint to be used.
    pub fn with_endpoint(&self, endpoint: Endpoint) -> SendData {
        SendData {
            target: endpoint,
            ..self.clone()
        }
    }

    /// Sends the data to the target endpoint.
    ///
    /// # Returns
    ///
    /// A `SendDataResult` instance containing the result of the operation.
    pub async fn send(&self, http_client: &HttpClient) -> SendDataResult {
        self.send_internal(http_client).await
    }

    async fn send_internal(&self, http_client: &HttpClient) -> SendDataResult {
        if self.use_protobuf() {
            self.send_with_protobuf(http_client).await
        } else {
            self.send_with_msgpack(http_client).await
        }
    }

    async fn send_payload(
        &self,
        chunks: u64,
        payload: Vec<u8>,
        headers: HashMap<&'static str, String>,
        http_client: &HttpClient,
    ) -> (SendWithRetryResult, u64, u64) {
        #[allow(clippy::unwrap_used)]
        let payload_len = u64::try_from(payload.len()).unwrap();
        (
            send_with_retry(
                http_client,
                &self.target,
                payload,
                &headers,
                &self.retry_strategy,
            )
            .await,
            payload_len,
            chunks,
        )
    }

    fn use_protobuf(&self) -> bool {
        self.target.api_key.is_some()
    }

    #[cfg(feature = "compression")]
    fn compress_payload(&self, payload: Vec<u8>, headers: &mut HashMap<&str, String>) -> Vec<u8> {
        match self.compression {
            Compression::Zstd(level) => {
                let result = (|| -> std::io::Result<Vec<u8>> {
                    let mut encoder = Encoder::new(Vec::new(), level)?;
                    encoder.write_all(&payload)?;
                    encoder.finish()
                })();

                match result {
                    Ok(compressed_payload) => {
                        headers.insert("Content-Encoding", "zstd".to_string());
                        compressed_payload
                    }
                    Err(_) => payload,
                }
            }
            _ => payload,
        }
    }

    async fn send_with_protobuf(&self, http_client: &HttpClient) -> SendDataResult {
        let mut result = SendDataResult::default();

        #[allow(clippy::unwrap_used)]
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
                let mut request_headers = self.headers.clone();

                #[cfg(feature = "compression")]
                let final_payload =
                    self.compress_payload(serialized_trace_payload, &mut request_headers);

                #[cfg(not(feature = "compression"))]
                let final_payload = serialized_trace_payload;

                request_headers.insert(CONTENT_TYPE.as_str(), APPLICATION_PROTOBUF_STR.to_string());

                let (response, bytes_sent, chunks) = self
                    .send_payload(chunks, final_payload, request_headers, http_client)
                    .await;

                result.update(response, bytes_sent, chunks);

                result
            }
            _ => result,
        }
    }

    async fn send_with_msgpack(&self, http_client: &HttpClient) -> SendDataResult {
        let mut result = SendDataResult::default();
        let mut futures = FuturesUnordered::new();

        match &self.tracer_payloads {
            TracerPayloadCollection::V07(payloads) => {
                for tracer_payload in payloads {
                    #[allow(clippy::unwrap_used)]
                    let chunks = u64::try_from(tracer_payload.chunks.len()).unwrap();
                    let mut headers = self.headers.clone();
                    headers.insert(DATADOG_TRACE_COUNT_STR, chunks.to_string());
                    headers.insert(CONTENT_TYPE.as_str(), APPLICATION_MSGPACK_STR.to_string());

                    let payload = match rmp_serde::to_vec_named(tracer_payload) {
                        Ok(p) => p,
                        Err(e) => return result.error(anyhow!(e)),
                    };

                    futures.push(self.send_payload(chunks, payload, headers, http_client));
                }
            }
            TracerPayloadCollection::V04(payload) => {
                #[allow(clippy::unwrap_used)]
                let chunks = u64::try_from(self.tracer_payloads.size()).unwrap();
                let mut headers = self.headers.clone();
                headers.insert(DATADOG_TRACE_COUNT_STR, chunks.to_string());
                headers.insert(CONTENT_TYPE.as_str(), APPLICATION_MSGPACK_STR.to_string());

                let payload = msgpack_encoder::v04::to_vec(payload);

                futures.push(self.send_payload(chunks, payload, headers, http_client));
            }
            TracerPayloadCollection::V05(payload) => {
                #[allow(clippy::unwrap_used)]
                let chunks = u64::try_from(self.tracer_payloads.size()).unwrap();
                let mut headers = self.headers.clone();
                headers.insert(DATADOG_TRACE_COUNT_STR, chunks.to_string());
                headers.insert(CONTENT_TYPE.as_str(), APPLICATION_MSGPACK_STR.to_string());

                let payload = match rmp_serde::to_vec(payload) {
                    Ok(p) => p,
                    Err(e) => return result.error(anyhow!(e)),
                };

                futures.push(self.send_payload(chunks, payload, headers, http_client));
            }
        }

        loop {
            match futures.next().await {
                Some((response, payload_len, chunks)) => {
                    result.update(response, payload_len, chunks);
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
        idx_tracer_payloads: Vec::new(),
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
    use crate::send_with_retry::{RetryBackoffType, RetryStrategy};
    use crate::test_utils::create_test_no_alloc_span;
    use crate::trace_utils::{construct_trace_chunk, construct_tracer_payload, RootSpanTags};
    use crate::tracer_header_tags::TracerHeaderTags;
    use ddcommon::Endpoint;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use libdd_trace_protobuf::pb::Span;
    use std::collections::HashMap;
    use std::time::Duration;

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
        dropped_p0_traces: 0,
        dropped_p0_spans: 0,
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
            span_events: vec![],
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
                msgpack_encoder::v04::to_len(payloads) as usize
            }
            TracerPayloadCollection::V05(payloads) => rmp_serde::to_vec(payloads).unwrap().len(),
        }
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

        for (key, value) in HashMap::from(header_tags) {
            assert_eq!(data.headers.get(key).unwrap(), &value);
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn request_protobuf() {
        let server = MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/x-protobuf")
                    .header("DD-API-KEY", "TEST-KEY")
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
        let client = ddcommon::hyper_migration::new_default_client();
        let res = data.send(&client).await;

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
                    .header("DD-API-KEY", "TEST-KEY")
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
        let client = ddcommon::hyper_migration::new_default_client();
        let res = data.send(&client).await;

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
                    .header(DATADOG_TRACE_COUNT_STR, "1")
                    .header("Content-type", "application/msgpack")
                    .header("datadog-meta-lang", header_tags.lang)
                    .header(
                        "datadog-meta-lang-interpreter",
                        header_tags.lang_interpreter,
                    )
                    .header("datadog-meta-lang-version", header_tags.lang_version)
                    .header(
                        "datadog-meta-lang-interpreter-vendor",
                        header_tags.lang_vendor,
                    )
                    .header("datadog-meta-tracer-version", header_tags.tracer_version)
                    .header("datadog-container-id", header_tags.container_id)
                    .header("Datadog-Send-Real-Http-Status", "1")
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
        let client = ddcommon::hyper_migration::new_default_client();
        let res = data.send(&client).await;

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
                    .header(DATADOG_TRACE_COUNT_STR, "1")
                    .header("Content-type", "application/msgpack")
                    .header("datadog-meta-lang", header_tags.lang)
                    .header(
                        "datadog-meta-lang-interpreter",
                        header_tags.lang_interpreter,
                    )
                    .header("datadog-meta-lang-version", header_tags.lang_version)
                    .header(
                        "datadog-meta-lang-interpreter-vendor",
                        header_tags.lang_vendor,
                    )
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
        let client = ddcommon::hyper_migration::new_default_client();
        let res = data.send(&client).await;

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
        let client = ddcommon::hyper_migration::new_default_client();
        let res = data.send(&client).await;

        mock.assert_calls_async(2).await;

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

        let client = ddcommon::hyper_migration::new_default_client();
        let res = data.send(&client).await;

        mock.assert_calls_async(5).await;

        assert!(res.last_result.is_ok());
        assert_eq!(res.last_result.unwrap().status(), 500);
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

        let client = ddcommon::hyper_migration::new_default_client();
        let res = data.send(&client).await;

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
                    .header(DATADOG_TRACE_COUNT_STR, "2")
                    .header("Content-type", "application/msgpack")
                    .header("datadog-meta-lang", header_tags.lang)
                    .header(
                        "datadog-meta-lang-interpreter",
                        header_tags.lang_interpreter,
                    )
                    .header("datadog-meta-lang-version", header_tags.lang_version)
                    .header(
                        "datadog-meta-lang-interpreter-vendor",
                        header_tags.lang_vendor,
                    )
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

        let client = ddcommon::hyper_migration::new_default_client();
        let res = data.send(&client).await;

        mock.assert_calls_async(5).await;

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

        let client = ddcommon::hyper_migration::new_default_client();
        let res = data.send(&client).await;

        mock.assert_calls_async(10).await;

        assert_eq!(res.errors_timeout, 1);
        assert_eq!(res.errors_network, 0);
        assert_eq!(res.errors_status_code, 0);
        assert_eq!(res.requests_count, 5);
        assert_eq!(res.chunks_sent, 0);
        assert_eq!(res.bytes_sent, 0);
        assert_eq!(res.responses_count_per_code.len(), 0);
    }

    #[test]
    fn test_with_endpoint() {
        let header_tags = HEADER_TAGS;
        let payload = setup_payload(&header_tags);
        let original_endpoint = Endpoint {
            api_key: Some(std::borrow::Cow::Borrowed("original-key")),
            url: "http://originalexample.com/".parse::<hyper::Uri>().unwrap(),
            timeout_ms: 1000,
            ..Endpoint::default()
        };

        let original_data = SendData::new(
            100,
            TracerPayloadCollection::V07(vec![payload]),
            header_tags,
            &original_endpoint,
        );

        let new_endpoint = Endpoint {
            api_key: Some(std::borrow::Cow::Borrowed("new-key")),
            url: "http://newexample.com/".parse::<hyper::Uri>().unwrap(),
            timeout_ms: 2000,
            ..Endpoint::default()
        };

        let new_data = original_data.with_endpoint(new_endpoint.clone());

        assert_eq!(new_data.target.api_key, new_endpoint.api_key);
        assert_eq!(new_data.target.url, new_endpoint.url);
        assert_eq!(new_data.target.timeout_ms, new_endpoint.timeout_ms);

        assert_eq!(new_data.size, original_data.size);
        assert_eq!(new_data.headers, original_data.headers);
        assert_eq!(new_data.retry_strategy, original_data.retry_strategy);
        assert_eq!(
            new_data.tracer_payloads.size(),
            original_data.tracer_payloads.size()
        );

        assert_eq!(original_data.target.api_key, original_endpoint.api_key);
        assert_eq!(original_data.target.url, original_endpoint.url);
        assert_eq!(
            original_data.target.timeout_ms,
            original_endpoint.timeout_ms
        );

        #[cfg(feature = "compression")]
        assert!(matches!(new_data.compression, Compression::None));
    }

    #[test]
    fn test_builder() {
        let header_tags = HEADER_TAGS;
        let payload = setup_payload(&header_tags);
        let retry_strategy = RetryStrategy::new(5, 100, RetryBackoffType::Constant, None);

        let send_data = SendDataBuilder::new(
            100,
            TracerPayloadCollection::V07(vec![payload]),
            header_tags,
            &Endpoint::default(),
        )
        // Test with_api_key()
        .with_api_key("TEST-KEY")
        // Test with_retry_strategy()
        .with_retry_strategy(retry_strategy.clone())
        .build();

        assert_eq!(
            send_data.target.api_key,
            Some(std::borrow::Cow::Borrowed("TEST-KEY"))
        );
        assert_eq!(send_data.retry_strategy, retry_strategy);
    }
}
