// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Telemetry provides a client to send results accumulated in 'Metrics'.
pub mod error;
pub mod metrics;
use crate::telemetry::error::TelemetryError;
use crate::telemetry::metrics::Metrics;
use datadog_trace_utils::trace_utils::SendDataResult;
use ddcommon::tag::Tag;
use ddtelemetry::worker::{
    LifecycleAction, TelemetryActions, TelemetryWorkerBuilder, TelemetryWorkerHandle,
};
use std::time::Duration;
use tokio::task::JoinHandle;

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
    pub fn set_hearbeat(mut self, interval: u64) -> Self {
        if interval > 0 {
            self.config.telemetry_hearbeat_interval = Duration::from_millis(interval);
        }
        self
    }

    /// Sets runtime id for the telemetry client.
    pub fn set_runtime_id(mut self, id: &str) -> Self {
        self.runtime_id = Some(id.to_string());
        self
    }

    /// Builds the telemetry client.
    pub async fn build(self) -> Result<TelemetryClient, TelemetryError> {
        let mut builder = TelemetryWorkerBuilder::new_fetch_host(
            self.service_name.unwrap(),
            self.language.unwrap(),
            self.language_version.unwrap(),
            self.tracer_version.unwrap(),
        );

        if let Some(id) = self.runtime_id {
            builder.runtime_id = Some(id);
        }

        let (worker, handle) = builder
            .spawn_with_config(self.config)
            .await
            .map_err(|e| TelemetryError::Builder(e.to_string()))?;

        Ok(TelemetryClient {
            handle,
            metrics: Metrics::new(&worker),
            worker,
        })
    }
}

/// Telemetry handle used to send metrics to the agent
pub struct TelemetryClient {
    metrics: Metrics,
    worker: TelemetryWorkerHandle,
    handle: JoinHandle<()>,
}

impl TelemetryClient {
    /// Sends metrics to the agent using a telemetry worker handle.
    ///
    /// # Arguments:
    ///
    /// * `telemetry_handle`: telemetry worker handle used to enqueue metrics.
    pub fn send(&self, data: &SendDataResult) -> Result<(), TelemetryError> {
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
        if let Err(_e) = self
            .worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
            .await
        {
            self.handle.abort();
        }
    }
    /// Shutdowns the telemetry client.
    pub async fn shutdown(&self) {
        if let Err(_e) = self
            .worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Stop))
            .await
        {
            self.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use regex::Regex;
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn builder_test() {
        let builder = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url("http://localhost")
            .set_hearbeat(30);

        assert_eq!(&builder.service_name.unwrap(), "test_service");
        assert_eq!(&builder.language.unwrap(), "test_language");
        assert_eq!(&builder.language_version.unwrap(), "test_language_version");
        assert_eq!(&builder.tracer_version.unwrap(), "test_tracer_version");
        assert_eq!(
            &builder.config.endpoint.unwrap().url.to_string().as_ref(),
            "http://localhost/telemetry/proxy/api/v2/apmtelemetry"
        );
        assert_eq!(
            builder.config.telemetry_hearbeat_interval,
            Duration::from_millis(30)
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn spawn_test() {
        let client = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .build()
            .await;

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

        let result = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_hearbeat(100)
            .build()
            .await;

        assert!(result.is_ok());

        let data = SendDataResult {
            last_result: Ok(hyper::Response::default()),
            bytes_sent: 1,
            ..Default::default()
        };

        let client = result.unwrap();

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        let _ = client.handle.await;
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

        let result = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_hearbeat(100)
            .build()
            .await;

        assert!(result.is_ok());

        let data = SendDataResult {
            last_result: Ok(hyper::Response::default()),
            requests_count: 1,
            ..Default::default()
        };

        let client = result.unwrap();

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        let _ = client.handle.await;
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

        let result = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_hearbeat(100)
            .build()
            .await;

        assert!(result.is_ok());

        let data = SendDataResult {
            last_result: Ok(hyper::Response::default()),
            responses_count_per_code: HashMap::from([(200, 1)]),
            ..Default::default()
        };

        let client = result.unwrap();

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        let _ = client.handle.await;
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

        let result = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_hearbeat(100)
            .build()
            .await;

        assert!(result.is_ok());

        let data = SendDataResult {
            last_result: Ok(hyper::Response::default()),
            errors_timeout: 1,
            ..Default::default()
        };

        let client = result.unwrap();

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        let _ = client.handle.await;
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

        let result = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_hearbeat(100)
            .build()
            .await;

        assert!(result.is_ok());

        let data = SendDataResult {
            last_result: Ok(hyper::Response::default()),
            errors_network: 1,
            ..Default::default()
        };

        let client = result.unwrap();

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        let _ = client.handle.await;
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

        let result = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_hearbeat(100)
            .build()
            .await;

        assert!(result.is_ok());

        let data = SendDataResult {
            last_result: Ok(hyper::Response::default()),
            errors_status_code: 1,
            ..Default::default()
        };

        let client = result.unwrap();

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        let _ = client.handle.await;
        telemetry_srv.assert_hits_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn errors_chunks_sent_test() {
        let payload = Regex::new(r#""metric":"trace_chunk_sent","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let result = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_hearbeat(100)
            .build()
            .await;

        assert!(result.is_ok());

        let data = SendDataResult {
            last_result: Ok(hyper::Response::default()),
            chunks_sent: 1,
            ..Default::default()
        };

        let client = result.unwrap();

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        let _ = client.handle.await;
        telemetry_srv.assert_hits_async(1).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn errors_chunks_dropped_test() {
        let payload = Regex::new(r#""metric":"trace_chunk_dropped","points":\[\[\d+,1\.0\]\],"tags":\["src_library:libdatadog"\],"common":true,"type":"count"#).unwrap();
        let server = MockServer::start_async().await;

        let telemetry_srv = server
            .mock_async(|when, then| {
                when.method(POST).body_matches(payload);
                then.status(200).body("");
            })
            .await;

        let result = TelemetryClientBuilder::default()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_url(&server.url("/"))
            .set_hearbeat(100)
            .build()
            .await;

        assert!(result.is_ok());

        let data = SendDataResult {
            last_result: Ok(hyper::Response::default()),
            chunks_dropped: 1,
            ..Default::default()
        };

        let client = result.unwrap();

        client.start().await;
        let _ = client.send(&data);
        client.shutdown().await;
        let _ = client.handle.await;
        telemetry_srv.assert_hits_async(1).await;
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
            .set_hearbeat(100)
            .set_runtime_id("foo")
            .build()
            .await;

        assert!(result.is_ok());

        let client = result.unwrap();

        client.start().await;
        client.shutdown().await;
        let _ = client.handle.await;

        // Check for 2 hits: app-started and app-closing.
        telemetry_srv.assert_hits_async(2).await;
    }
}
