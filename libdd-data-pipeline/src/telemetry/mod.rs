// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Telemetry provides a client to send results accumulated in 'Metrics'.
mod builder;
mod error;
#[cfg(feature = "telemetry")]
mod metrics;
mod worker;

#[cfg(feature = "telemetry")]
use libdd_common::tag::Tag;
#[cfg(feature = "telemetry")]
use libdd_telemetry::worker::{LifecycleAction, TelemetryActions, TelemetryWorkerHandle};
#[cfg(feature = "telemetry")]
use libdd_trace_utils::send_with_retry::SendWithRetryError;
use libdd_trace_utils::send_with_retry::SendWithRetryResult;
use libdd_trace_utils::trace_utils::SendDataResult;
#[cfg(feature = "telemetry")]
use metrics::Metrics;

pub use builder::TelemetryClientBuilder;
pub(crate) use error::TelemetryError;
pub(crate) use worker::TelemetryWorker;

/// Configuration for telemetry reporting.
#[derive(Debug, Default, Clone)]
pub struct TelemetryConfig {
    pub heartbeat: u64,
    pub runtime_id: Option<String>,
    pub debug_enabled: bool,
}

#[cfg(feature = "telemetry")]
/// Telemetry handle used to send metrics to the agent
#[derive(Debug)]
pub struct TelemetryClient {
    metrics: Metrics,
    worker: TelemetryWorkerHandle,
}

#[cfg(not(feature = "telemetry"))]
#[derive(Debug)]
pub struct TelemetryClient {}

#[cfg(feature = "telemetry")]
impl TelemetryClient {
    /// Sends metrics to the agent using a telemetry worker handle.
    ///
    /// # Arguments:
    ///
    /// * `telemetry_handle`: telemetry worker handle used to enqueue metrics.
    pub fn send(&self, data: &SendPayloadTelemetry) -> Result<(), TelemetryError> {
        if data.requests_count > 0 {
            let key = self.metrics.get(metrics::MetricKind::ApiRequest);
            self.worker
                .add_point(data.requests_count as f64, key, vec![])?;
        }
        if data.errors_network > 0 {
            let key = self.metrics.get(metrics::MetricKind::ApiErrorsNetwork);
            self.worker
                .add_point(data.errors_network as f64, key, vec![])?;
        }
        if data.errors_timeout > 0 {
            let key = self.metrics.get(metrics::MetricKind::ApiErrorsTimeout);
            self.worker
                .add_point(data.errors_timeout as f64, key, vec![])?;
        }
        if data.errors_status_code > 0 {
            let key = self.metrics.get(metrics::MetricKind::ApiErrorsStatusCode);
            self.worker
                .add_point(data.errors_status_code as f64, key, vec![])?;
        }
        if data.bytes_sent > 0 {
            let key = self.metrics.get(metrics::MetricKind::ApiBytes);
            self.worker.add_point(data.bytes_sent as f64, key, vec![])?;
        }
        if data.chunks_sent > 0 {
            let key = self.metrics.get(metrics::MetricKind::ChunksSent);
            self.worker
                .add_point(data.chunks_sent as f64, key, vec![])?;
        }
        if data.chunks_dropped_p0 > 0 {
            let key = self.metrics.get(metrics::MetricKind::ChunksDroppedP0);
            self.worker
                .add_point(data.chunks_dropped_p0 as f64, key, vec![])?;
        }
        if data.chunks_dropped_serialization_error > 0 {
            let key = self
                .metrics
                .get(metrics::MetricKind::ChunksDroppedSerializationError);
            self.worker
                .add_point(data.chunks_dropped_serialization_error as f64, key, vec![])?;
        }
        if data.chunks_dropped_send_failure > 0 {
            let key = self
                .metrics
                .get(metrics::MetricKind::ChunksDroppedSendFailure);
            self.worker
                .add_point(data.chunks_dropped_send_failure as f64, key, vec![])?;
        }
        if !data.responses_count_per_code.is_empty() {
            let key = self.metrics.get(metrics::MetricKind::ApiResponses);
            for (status_code, count) in &data.responses_count_per_code {
                let tag = Tag::new("status_code", status_code.to_string().as_str())?;
                self.worker.add_point(*count as f64, key, vec![tag])?;
            }
        }
        Ok(())
    }

    /// Starts the client
    pub async fn start(&self) {
        _ = self
            .worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
            .await;
    }

    /// Shutdowns the telemetry client.
    pub async fn shutdown(self) {
        _ = self
            .worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Stop))
            .await;
    }
}

#[cfg(not(feature = "telemetry"))]
impl TelemetryClient {
    /// No-op: telemetry is disabled.
    pub fn send(&self, _data: &SendPayloadTelemetry) -> Result<(), TelemetryError> {
        Ok(())
    }

    /// No-op: telemetry is disabled.
    pub async fn start(&self) {}

    /// No-op: telemetry is disabled.
    pub async fn shutdown(self) {}
}

#[cfg(feature = "telemetry")]
/// Telemetry describing the sending of a trace payload
/// It can be produced from a [`SendWithRetryResult`] or from a [`SendDataResult`].
#[derive(PartialEq, Debug, Default)]
pub struct SendPayloadTelemetry {
    requests_count: u64,
    errors_network: u64,
    errors_timeout: u64,
    errors_status_code: u64,
    bytes_sent: u64,
    chunks_sent: u64,
    chunks_dropped_p0: u64,
    chunks_dropped_serialization_error: u64,
    chunks_dropped_send_failure: u64,
    responses_count_per_code: std::collections::HashMap<u16, u64>,
}

#[cfg(not(feature = "telemetry"))]
#[derive(Debug)]
pub struct SendPayloadTelemetry {}

#[cfg(feature = "telemetry")]
impl SendPayloadTelemetry {
    /// Create a [`SendPayloadTelemetry`] from a [`SendWithRetryResult`].
    ///
    /// # Arguments
    /// * `value` - The result of sending traces with retry
    /// * `bytes_sent` - The number of bytes in the payload
    /// * `chunks` - The number of trace chunks in the payload
    /// * `chunks_dropped_p0` - The number of P0 trace chunks dropped due to sampling
    pub fn from_retry_result(
        value: &SendWithRetryResult,
        bytes_sent: u64,
        chunks: u64,
        chunks_dropped_p0: u64,
    ) -> Self {
        let mut telemetry = Self {
            chunks_dropped_p0,
            ..Default::default()
        };
        match value {
            Ok((response, attempts)) => {
                telemetry.chunks_sent = chunks;
                telemetry.bytes_sent = bytes_sent;
                telemetry
                    .responses_count_per_code
                    .insert(response.status().into(), 1);
                telemetry.requests_count = *attempts as u64;
            }
            Err(err) => match err {
                SendWithRetryError::Http(response, attempts) => {
                    telemetry.chunks_dropped_send_failure = chunks;
                    telemetry.errors_status_code = 1;
                    telemetry
                        .responses_count_per_code
                        .insert(response.status().into(), 1);
                    telemetry.requests_count = *attempts as u64;
                }
                SendWithRetryError::Timeout(attempts) => {
                    telemetry.chunks_dropped_send_failure = chunks;
                    telemetry.errors_timeout = 1;
                    telemetry.requests_count = *attempts as u64;
                }
                SendWithRetryError::Network(_, attempts) => {
                    telemetry.chunks_dropped_send_failure = chunks;
                    telemetry.errors_network = 1;
                    telemetry.requests_count = *attempts as u64;
                }
                SendWithRetryError::Build(attempts) => {
                    telemetry.chunks_dropped_serialization_error = chunks;
                    telemetry.requests_count = *attempts as u64;
                }
            },
        };
        telemetry
    }
}

#[cfg(not(feature = "telemetry"))]
impl SendPayloadTelemetry {
    /// No-op: telemetry is disabled.
    pub fn from_retry_result(
        _value: &SendWithRetryResult,
        _bytes_sent: u64,
        _chunks: u64,
        _chunks_dropped_p0: u64,
    ) -> Self {
        Self {}
    }
}

#[cfg(feature = "telemetry")]
impl From<&SendDataResult> for SendPayloadTelemetry {
    fn from(value: &SendDataResult) -> Self {
        Self {
            requests_count: value.requests_count,
            errors_network: value.errors_network,
            errors_timeout: value.errors_timeout,
            errors_status_code: value.errors_status_code,
            bytes_sent: value.bytes_sent,
            chunks_sent: value.chunks_sent,
            chunks_dropped_send_failure: value.chunks_dropped,
            responses_count_per_code: value.responses_count_per_code.clone(),
            ..Default::default()
        }
    }
}

#[cfg(not(feature = "telemetry"))]
impl From<&SendDataResult> for SendPayloadTelemetry {
    fn from(_value: &SendDataResult) -> Self {
        Self {}
    }
}

#[cfg(test)]
#[cfg(feature = "telemetry")]
mod tests {
    use http::{Response, StatusCode};
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use libdd_common::{http_common, worker::Worker};
    use regex::Regex;
    use std::collections::HashMap;
    use std::time::Duration;
    use tokio::{runtime::Handle, time::sleep};

    use super::*;

    async fn get_test_client(url: &str) -> TelemetryClient {
        let (client, mut worker) = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_service_version("test_version")
            .set_env("test_env")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(url)
            .set_heartbeat(100)
            .set_debug_enabled(true)
            .build(Handle::current());
        tokio::spawn(async move { worker.run().await });
        client
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_test() {
        let _ = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_service_version("test_version")
            .set_env("test_env")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .build(Handle::current());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn api_bytes_test() {
        let payload = Regex::new(r#""metric":"trace_api.bytes","tags":\["src_library:libdatadog"\],"sketch_b64":".+","common":true,"interval":\d+,"type":"distribution""#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            bytes_sent: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn requests_test() {
        let payload = Regex::new(r#""metric":"trace_api.requests","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog"\],"common":true,"type":"count""#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            requests_count: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn responses_per_code_test() {
        let payload = Regex::new(r#""metric":"trace_api.responses","points":\[\[\d+,1\.0\]\],"tags":\["status_code:200","src_library:libdatadog"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            responses_count_per_code: HashMap::from([(200, 1)]),
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn errors_timeout_test() {
        let payload = Regex::new(r#""metric":"trace_api.errors","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog","type:timeout"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            errors_timeout: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn errors_network_test() {
        let payload = Regex::new(r#""metric":"trace_api.errors","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog","type:network"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            errors_network: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn errors_status_code_test() {
        let payload = Regex::new(r#""metric":"trace_api.errors","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog","type:status_code"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            errors_status_code: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn chunks_sent_test() {
        let payload = Regex::new(r#""metric":"trace_chunks_sent","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            chunks_sent: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn chunks_dropped_send_failure_test() {
        let payload = Regex::new(r#""metric":"trace_chunks_dropped","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog","reason:send_failure"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            chunks_dropped_send_failure: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn chunks_dropped_p0_test() {
        let payload = Regex::new(r#""metric":"trace_chunks_dropped","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog","reason:p0_drop"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            chunks_dropped_p0: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn chunks_dropped_serialization_error_test() {
        let payload = Regex::new(r#""metric":"trace_chunks_dropped","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog","reason:serialization_error"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            chunks_dropped_serialization_error: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_calls_async(1).await;
    }

    #[test]
    fn telemetry_from_ok_response_test() {
        let result = Ok((
            http_common::empty_response(http::response::Builder::new()).unwrap(),
            3,
        ));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 4, 5, 0);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                bytes_sent: 4,
                chunks_sent: 5,
                requests_count: 3,
                responses_count_per_code: HashMap::from([(200, 1)]),
                ..Default::default()
            }
        )
    }

    #[test]
    fn telemetry_from_ok_response_with_p0_drops_test() {
        let result = Ok((
            http_common::empty_response(http::response::Builder::new()).unwrap(),
            3,
        ));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 4, 5, 10);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                bytes_sent: 4,
                chunks_sent: 5,
                requests_count: 3,
                chunks_dropped_p0: 10,
                responses_count_per_code: HashMap::from([(200, 1)]),
                ..Default::default()
            }
        )
    }

    #[test]
    fn telemetry_from_request_error_test() {
        let error_response =
            http_common::empty_response(Response::builder().status(StatusCode::BAD_REQUEST))
                .unwrap();
        let result = Err(SendWithRetryError::Http(error_response, 5));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 1, 2, 0);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                chunks_dropped_send_failure: 2,
                requests_count: 5,
                errors_status_code: 1,
                responses_count_per_code: HashMap::from([(400, 1)]),
                ..Default::default()
            }
        )
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn telemetry_from_network_error_test() {
        // Create an hyper error by calling an undefined service
        let err = http_common::new_default_client()
            .get(http::Uri::from_static("localhost:12345"))
            .await
            .unwrap_err();

        let result = Err(SendWithRetryError::Network(http_common::into_error(err), 5));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 1, 2, 0);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                chunks_dropped_send_failure: 2,
                requests_count: 5,
                errors_network: 1,
                ..Default::default()
            }
        )
    }

    #[test]
    fn telemetry_from_timeout_error_test() {
        let result = Err(SendWithRetryError::Timeout(5));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 1, 2, 0);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                chunks_dropped_send_failure: 2,
                requests_count: 5,
                errors_timeout: 1,
                ..Default::default()
            }
        )
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn telemetry_from_build_error_test() {
        let result = Err(SendWithRetryError::Build(5));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 1, 2, 0);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                chunks_dropped_serialization_error: 2,
                requests_count: 5,
                ..Default::default()
            }
        )
    }

    #[test]
    fn telemetry_from_send_data_result_test() {
        let result = SendDataResult {
            requests_count: 10,
            responses_count_per_code: HashMap::from([(200, 3)]),
            errors_timeout: 1,
            errors_network: 2,
            errors_status_code: 3,
            bytes_sent: 4,
            chunks_sent: 5,
            chunks_dropped: 6,
            ..Default::default()
        };

        let expected_telemetry = SendPayloadTelemetry {
            requests_count: 10,
            errors_network: 2,
            errors_timeout: 1,
            errors_status_code: 3,
            bytes_sent: 4,
            chunks_sent: 5,
            chunks_dropped_send_failure: 6,
            responses_count_per_code: HashMap::from([(200, 3)]),
            ..Default::default()
        };

        assert_eq!(SendPayloadTelemetry::from(&result), expected_telemetry)
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn runtime_id_test() {
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_includes(r#""runtime_id":"foo""#);
                then.status(200).body("");
            })
            .await;

        let (client, mut worker) = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_service_version("test_version")
            .set_env("test_env")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_heartbeat(100)
            .set_runtime_id("foo")
            .build(Handle::current());
        tokio::spawn(async move { worker.run().await });

        client.start().await;
        client
            .send(&SendPayloadTelemetry {
                requests_count: 1,
                ..Default::default()
            })
            .unwrap();
        client.shutdown().await;
        while telemetry_srv.calls_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        // One payload generate-metrics
        telemetry_srv.assert_calls_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn application_metadata_test() {
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST)
                    .body_includes(r#""application":{"service_name":"test_service","service_version":"test_version","env":"test_env","language_name":"test_language","language_version":"test_language_version","tracer_version":"test_tracer_version"}"#);
                then.status(200).body("");
            })
            .await;

        let (client, mut worker) = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_service_version("test_version")
            .set_env("test_env")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_heartbeat(100)
            .set_runtime_id("foo")
            .build(Handle::current());
        tokio::spawn(async move { worker.run().await });

        client.start().await;
        client
            .send(&SendPayloadTelemetry {
                requests_count: 1,
                ..Default::default()
            })
            .unwrap();
        client.shutdown().await;
        // Wait for the server to receive at least one call, but don't hang forever.
        let start = std::time::Instant::now();
        while telemetry_srv.calls_async().await == 0 {
            if start.elapsed() > Duration::from_secs(180) {
                panic!("telemetry server did not receive calls within timeout");
            }
            sleep(Duration::from_millis(10)).await;
        }
        // One payload generate-metrics
        telemetry_srv.assert_calls_async(1).await;
    }
}
