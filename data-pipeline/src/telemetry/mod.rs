// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Telemetry provides a client to send results accumulated in 'Metrics'.
pub mod error;
pub mod metrics;
use crate::telemetry::error::TelemetryError;
use crate::telemetry::metrics::Metrics;
use datadog_trace_utils::{
    send_with_retry::{SendWithRetryError, SendWithRetryResult},
    trace_utils::SendDataResult,
};
use ddcommon::tag::Tag;
use ddtelemetry::worker::{
    LifecycleAction, TelemetryActions, TelemetryWorker, TelemetryWorkerBuilder,
    TelemetryWorkerFlavor, TelemetryWorkerHandle,
};
use std::{collections::HashMap, time::Duration};
use tokio::runtime::Handle;

/// Structure to build a Telemetry client.
///
/// Holds partial data until the `build` method is called which results in a new
/// `TelemetryClient`.
#[derive(Default)]
pub struct TelemetryClientBuilder {
    service_name: Option<String>,
    language: Option<String>,
    language_version: Option<String>,
    tracer_version: Option<String>,
    config: ddtelemetry::config::Config,
    runtime_id: Option<String>,
}

impl TelemetryClientBuilder {
    /// Sets the service name for the telemetry client
    pub fn set_service_name(mut self, name: &str) -> Self {
        self.service_name = Some(name.to_string());
        self
    }

    /// Sets the language name for the telemetry client
    pub fn set_language(mut self, lang: &str) -> Self {
        self.language = Some(lang.to_string());
        self
    }

    /// Sets the language version for the telemetry client
    pub fn set_language_version(mut self, version: &str) -> Self {
        self.language_version = Some(version.to_string());
        self
    }

    /// Sets the tracer version for the telemetry client
    pub fn set_tracer_version(mut self, version: &str) -> Self {
        self.tracer_version = Some(version.to_string());
        self
    }

    /// Sets the url where the metrics will be sent.
    pub fn set_url(mut self, url: &str) -> Self {
        let _ = self
            .config
            .set_endpoint(ddcommon::Endpoint::from_slice(url));
        self
    }

    /// Sets the heartbeat notification interval in millis.
    pub fn set_heartbeat(mut self, interval: u64) -> Self {
        if interval > 0 {
            self.config.telemetry_heartbeat_interval = Duration::from_millis(interval);
        }
        self
    }

    /// Sets runtime id for the telemetry client.
    pub fn set_runtime_id(mut self, id: &str) -> Self {
        self.runtime_id = Some(id.to_string());
        self
    }

    /// Sets the debug enabled flag for the telemetry client.
    pub fn set_debug_enabled(mut self, debug: bool) -> Self {
        self.config.debug_enabled = debug;
        self
    }

    /// Builds the telemetry client.
    pub fn build(
        self,
        runtime: Handle,
    ) -> Result<(TelemetryClient, TelemetryWorker), TelemetryError> {
        #[allow(clippy::unwrap_used)]
        let mut builder = TelemetryWorkerBuilder::new_fetch_host(
            self.service_name.unwrap(),
            self.language.unwrap(),
            self.language_version.unwrap(),
            self.tracer_version.unwrap(),
        );
        builder.config = self.config;
        // Send only metrics and logs and drop lifecycle events
        builder.flavor = TelemetryWorkerFlavor::MetricsLogs;

        if let Some(id) = self.runtime_id {
            builder.runtime_id = Some(id);
        }

        let (worker_handle, worker) = builder
            .build_worker(runtime)
            .map_err(|e| TelemetryError::Builder(e.to_string()))?;

        Ok((
            TelemetryClient {
                metrics: Metrics::new(&worker_handle),
                worker: worker_handle,
            },
            worker,
        ))
    }
}

/// Telemetry handle used to send metrics to the agent
#[derive(Debug)]
pub struct TelemetryClient {
    metrics: Metrics,
    worker: TelemetryWorkerHandle,
}

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
    chunks_dropped: u64,
    responses_count_per_code: HashMap<u16, u64>,
}

impl From<&SendDataResult> for SendPayloadTelemetry {
    fn from(value: &SendDataResult) -> Self {
        Self {
            requests_count: value.requests_count,
            errors_network: value.errors_network,
            errors_timeout: value.errors_timeout,
            errors_status_code: value.errors_status_code,
            bytes_sent: value.bytes_sent,
            chunks_sent: value.chunks_sent,
            chunks_dropped: value.chunks_dropped,
            responses_count_per_code: value.responses_count_per_code.clone(),
        }
    }
}

impl SendPayloadTelemetry {
    /// Create a [`SendPayloadTelemetry`] from a [`SendWithRetryResult`].
    pub fn from_retry_result(value: &SendWithRetryResult, bytes_sent: u64, chunks: u64) -> Self {
        let mut telemetry = Self::default();
        match value {
            Ok((response, attempts)) => {
                telemetry.chunks_sent = chunks;
                telemetry.bytes_sent = bytes_sent;
                telemetry
                    .responses_count_per_code
                    .insert(response.status().into(), 1);
                telemetry.requests_count = *attempts as u64;
            }
            Err(err) => {
                telemetry.chunks_dropped = chunks;
                match err {
                    SendWithRetryError::Http(response, attempts) => {
                        telemetry.errors_status_code = 1;
                        telemetry
                            .responses_count_per_code
                            .insert(response.status().into(), 1);
                        telemetry.requests_count = *attempts as u64;
                    }
                    SendWithRetryError::Timeout(attempts) => {
                        telemetry.errors_timeout = 1;
                        telemetry.requests_count = *attempts as u64;
                    }
                    SendWithRetryError::Network(_, attempts) => {
                        telemetry.errors_network = 1;
                        telemetry.requests_count = *attempts as u64;
                    }
                    SendWithRetryError::Build(attempts) => {
                        telemetry.requests_count = *attempts as u64;
                    }
                }
            }
        };
        telemetry
    }
}

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
        if data.chunks_dropped > 0 {
            let key = self.metrics.get(metrics::MetricKind::ChunksDropped);
            self.worker
                .add_point(data.chunks_dropped as f64, key, vec![])?;
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

#[cfg(test)]
mod tests {
    use ddcommon::hyper_migration;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use hyper::{Response, StatusCode};
    use regex::Regex;
    use tokio::time::sleep;

    use super::*;

    async fn get_test_client(url: &str) -> TelemetryClient {
        let (client, mut worker) = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(url)
            .set_heartbeat(100)
            .set_debug_enabled(true)
            .build(Handle::current())
            .unwrap();
        tokio::spawn(async move { worker.run().await });
        client
    }

    #[test]
    fn builder_test() {
        let builder = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url("http://localhost")
            .set_debug_enabled(true)
            .set_heartbeat(30);

        assert_eq!(&builder.service_name.unwrap(), "test_service");
        assert_eq!(&builder.language.unwrap(), "test_language");
        assert_eq!(&builder.language_version.unwrap(), "test_language_version");
        assert_eq!(&builder.tracer_version.unwrap(), "test_tracer_version");
        assert!(builder.config.debug_enabled);
        assert_eq!(
            <String as AsRef<str>>::as_ref(&builder.config.endpoint().unwrap().url.to_string()),
            "http://localhost/telemetry/proxy/api/v2/apmtelemetry"
        );
        assert_eq!(
            builder.config.telemetry_heartbeat_interval,
            Duration::from_millis(30)
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_test() {
        let client = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .build(Handle::current());

        assert!(client.is_ok());
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
        while telemetry_srv.hits_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_hits_async(1).await;
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
        while telemetry_srv.hits_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_hits_async(1).await;
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
        while telemetry_srv.hits_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_hits_async(1).await;
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
        while telemetry_srv.hits_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_hits_async(1).await;
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
        while telemetry_srv.hits_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_hits_async(1).await;
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
        while telemetry_srv.hits_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_hits_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn chunks_sent_test() {
        let payload = Regex::new(r#""metric":"trace_chunk_sent","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog"\],"common":true,"type":"count"#).unwrap();
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
        while telemetry_srv.hits_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_hits_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn chunks_dropped_test() {
        let payload = Regex::new(r#""metric":"trace_chunk_dropped","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let data = SendPayloadTelemetry {
            chunks_dropped: 1,
            ..Default::default()
        };

        let client = get_test_client(&server.url("/")).await;

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        while telemetry_srv.hits_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        telemetry_srv.assert_hits_async(1).await;
    }

    #[test]
    fn telemetry_from_ok_response_test() {
        let result = Ok((Response::default(), 3));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 4, 5);
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
    fn telemetry_from_request_error_test() {
        let mut error_response = Response::default();
        *error_response.status_mut() = StatusCode::BAD_REQUEST;
        let result = Err(SendWithRetryError::Http(error_response, 5));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 1, 2);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                chunks_dropped: 2,
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
        let hyper_error = hyper_migration::new_default_client()
            .get(hyper::Uri::from_static("localhost:12345"))
            .await
            .unwrap_err();

        let result = Err(SendWithRetryError::Network(hyper_error, 5));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 1, 2);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                chunks_dropped: 2,
                requests_count: 5,
                errors_network: 1,
                ..Default::default()
            }
        )
    }

    #[test]
    fn telemetry_from_timeout_error_test() {
        let result = Err(SendWithRetryError::Timeout(5));
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 1, 2);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                chunks_dropped: 2,
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
        let telemetry = SendPayloadTelemetry::from_retry_result(&result, 1, 2);
        assert_eq!(
            telemetry,
            SendPayloadTelemetry {
                chunks_dropped: 2,
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
            chunks_dropped: 6,
            responses_count_per_code: HashMap::from([(200, 3)]),
        };

        assert_eq!(SendPayloadTelemetry::from(&result), expected_telemetry)
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn runtime_id_test() {
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_contains(r#""runtime_id":"foo""#);
                then.status(200).body("");
            })
            .await;

        let result = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_heartbeat(100)
            .set_runtime_id("foo")
            .build(Handle::current());

        let (client, mut worker) = result.unwrap();
        tokio::spawn(async move { worker.run().await });

        client.start().await;
        client
            .send(&SendPayloadTelemetry {
                requests_count: 1,
                ..Default::default()
            })
            .unwrap();
        client.shutdown().await;
        while telemetry_srv.hits_async().await == 0 {
            sleep(Duration::from_millis(10)).await;
        }
        // One payload generate-metrics
        telemetry_srv.assert_hits_async(1).await;
    }
}
