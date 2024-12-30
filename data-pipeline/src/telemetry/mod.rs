// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use datadog_trace_utils::trace_utils::SendDataResult;
use ddcommon::tag;
use ddcommon::tag::Tag;
use ddtelemetry::data::metrics::{MetricNamespace, MetricType};
use ddtelemetry::metrics::ContextKey;
use ddtelemetry::worker::{
    LifecycleAction, TelemetryActions, TelemetryWorkerBuilder, TelemetryWorkerHandle,
};
use std::collections::HashMap;
use std::time::Duration;
use tokio::select;
use tokio::task::JoinHandle;

#[derive(Default)]
pub struct Metrics {
    pub active_sessions: usize,
    pub api_requests: u64,
    pub api_responses_count_per_code: HashMap<u16, u64>,
    pub api_errors_timeout: u64,
    pub api_errors_network: u64,
    pub api_errors_status_code: u64,
    pub bytes_sent: u64,
    pub chunks_sent: u64,
    pub chunks_dropped: u64,
    pub submitted_payloads: u64,
    pub memory_usage: usize,
}

impl Metrics {
    pub fn update(&mut self, result: &SendDataResult) {
        self.api_requests += result.requests_count;
        self.api_errors_timeout += result.errors_timeout;
        self.api_errors_network += result.errors_network;
        self.api_errors_status_code += result.errors_status_code;
        self.bytes_sent += result.bytes_sent;
        self.chunks_sent += result.chunks_sent;
        self.chunks_dropped += result.chunks_dropped;

        for (status_code, count) in &result.responses_count_per_code {
            *self
                .api_responses_count_per_code
                .entry(*status_code)
                .or_default() += count;
        }
    }
}

#[derive(Debug, PartialEq)]
struct SelfMetric(MetricType, MetricNamespace);

#[derive(Default)]
pub struct TelemetryClientBuilder {
    service_name: Option<String>,
    language: Option<String>,
    language_version: Option<String>,
    tracer_version: Option<String>,
    config: ddtelemetry::config::Config,
    metrics: HashMap<String, SelfMetric>,
    interval: u64,
    url: Option<String>,
}

impl TelemetryClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_service_name(mut self, name: &str) -> Self {
        self.service_name = Some(name.to_string());
        self
    }

    pub fn set_language(mut self, lang: &str) -> Self {
        self.language = Some(lang.to_string());
        self
    }

    pub fn set_language_version(mut self, version: &str) -> Self {
        self.language_version = Some(version.to_string());
        self
    }

    pub fn set_tracer_version(mut self, version: &str) -> Self {
        self.tracer_version = Some(version.to_string());
        self
    }

    pub fn add_metric(
        mut self,
        name: &str,
        metric_type: MetricType,
        namespace: MetricNamespace,
    ) -> Self {
        self.metrics
            .insert(name.to_string(), SelfMetric(metric_type, namespace));
        self
    }

    pub fn set_interval(mut self, msecs: u64) -> Self {
        self.interval = msecs;
        self
    }

    pub fn set_config(mut self, config: ddtelemetry::config::Config) -> Self {
        self.config = config;
        self
    }

    pub fn set_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_string());
        self
    }

    pub async fn spawn(self) -> Result<TelemetryClient> {
        let (worker, handle) = TelemetryWorkerBuilder::new_fetch_host(
            self.service_name.unwrap(),
            self.language.unwrap(),
            self.language_version.unwrap(),
            self.tracer_version.unwrap(),
        )
        .spawn_with_config(self.config.clone())
        .await?;

        let metrics = {
            let mut map: HashMap<String, ContextKey> = HashMap::new();
            for (k, v) in self.metrics.into_iter() {
                map.insert(
                    k.clone(),
                    worker.register_metric_context(k, vec![], v.0, true, v.1),
                );
            }
            map
        };

        Ok(TelemetryClient {
            handle,
            interval: tokio::time::interval(Duration::from_millis(self.interval)),
            worker,
            metrics,
        })
    }
}

pub struct TelemetryClient {
    interval: tokio::time::Interval,
    worker: TelemetryWorkerHandle,
    handle: JoinHandle<()>,
    // TODO: use Rc<str> as key?
    metrics: HashMap<String, ContextKey>,
    // config: ddtelemetry::config::Config,
}

impl TelemetryClient {
    async fn enqueue_point(&self, value: f64, key: ContextKey, tags: Vec<Tag>) -> Result<()> {
        self.worker
            .send_msg(TelemetryActions::AddPoint((value, key, tags)))
            .await
    }

    async fn send(&mut self, metrics: Metrics) {
        let mut futures = Vec::new();
        if metrics.active_sessions > 0 {
            let key = self.metrics.get("server.active_sessions").unwrap();
            futures.push(self.enqueue_point(metrics.active_sessions as f64, *key, vec![]));
        }
        if metrics.memory_usage > 0 {
            let key = self.metrics.get("server.memory_usage").unwrap();
            futures.push(self.enqueue_point(metrics.memory_usage as f64, *key, vec![]));
        }
        if metrics.submitted_payloads > 0 {
            let key = self.metrics.get("server.submitted_payloads").unwrap();
            futures.push(self.enqueue_point(metrics.submitted_payloads as f64, *key, vec![]));
        }
        if metrics.api_requests > 0 {
            let key = self.metrics.get("trace_api.requests").unwrap();
            futures.push(self.enqueue_point(
                metrics.api_requests as f64,
                *key,
                vec![Tag::new("src_library", "libdatadog").unwrap()],
            ));
        }
        if metrics.api_errors_network > 0 {
            let key = self.metrics.get("trace_api.errors").unwrap();
            futures.push(self.enqueue_point(
                metrics.api_errors_network as f64,
                *key,
                vec![tag!("type", "network"), tag!("src_library", "libdatadog")],
            ));
        }
        if metrics.api_errors_timeout > 0 {
            let key = self.metrics.get("trace_api.errors").unwrap();
            futures.push(self.enqueue_point(
                metrics.api_errors_timeout as f64,
                *key,
                vec![tag!("type", "timeout"), tag!("src_library", "libdatadog")],
            ));
        }
        if metrics.api_errors_status_code > 0 {
            let key = self.metrics.get("trace_api.errors").unwrap();
            futures.push(self.enqueue_point(
                metrics.api_errors_status_code as f64,
                *key,
                vec![
                    tag!("type", "status_code"),
                    tag!("src_library", "libdatadog"),
                ],
            ));
        }
        if metrics.bytes_sent > 0 {
            let key = self.metrics.get("trace_api.bytes").unwrap();
            futures.push(self.enqueue_point(
                metrics.bytes_sent as f64,
                *key,
                vec![tag!("src_library", "libdatadog")],
            ));
        }
        if metrics.chunks_sent > 0 {
            let key = self.metrics.get("trace_chunks_sent").unwrap();
            futures.push(self.enqueue_point(
                metrics.chunks_sent as f64,
                *key,
                vec![tag!("src_library", "libdatadog")],
            ));
        }
        if metrics.chunks_dropped > 0 {
            let key = self.metrics.get("trace_chunks_dropped").unwrap();
            futures.push(self.enqueue_point(
                metrics.chunks_dropped as f64,
                *key,
                vec![tag!("src_library", "libdatadog")],
            ));
        }
        if !metrics.api_responses_count_per_code.is_empty() {
            let key = self.metrics.get("trace_api.responses").unwrap();
            for (status_code, count) in &metrics.api_responses_count_per_code {
                futures.push(self.enqueue_point(
                    *count as f64,
                    *key,
                    vec![
                        Tag::new("status_code", status_code.to_string().as_str()).unwrap(),
                        tag!("src_library", "libdatadog"),
                    ],
                ));
            }
        }

        futures::future::join_all(futures).await;
    }

    pub async fn run<U, C>(&mut self, mut update: U, cancellation: C)
    where
        U: FnMut() -> Option<Metrics>,
        C: std::future::Future,
    {
        let _ = self
            .worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
            .await;

        tokio::pin!(cancellation);

        loop {
            select! {
                _ = self.interval.tick() => {
                    if let Some(metrics) = update() {
                        self.send(metrics).await;
                        let _ = self.worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushMetricAggr)).await;
                        let _ = self.worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData)).await;
                    }
                },
                _ = &mut cancellation => {
                    if let Some(metrics) = update() {
                        // TODO: is this necessary?
                        self.send(metrics).await;
                    }
                    let _ = self.worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::Stop)).await;
                    let _ = (&mut self.handle).await;
                    return
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ddcommon::Endpoint;
    use ddtelemetry::config::Config;
    use httpmock::Method::POST;
    use httpmock::MockServer;

    use super::*;

    #[derive(Debug, Default)]
    struct MetricBucket {
        updated: bool,
        bytes: u64,
    }

    impl MetricBucket {
        pub fn update(&mut self, bytes: u64) {
            self.updated = true;
            self.bytes += bytes;
        }

        pub fn get(&mut self) -> Option<Metrics> {
            if self.updated {
                self.updated = false;
                Some(Metrics {
                    bytes_sent: self.bytes,
                    ..Default::default()
                })
            } else {
                None
            }
        }
    }

    #[test]
    fn builder_test() {
        let config = Config::default();

        let builder = TelemetryClientBuilder::new()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_interval(100)
            .set_config(config)
            .add_metric("test.foo", MetricType::Count, MetricNamespace::Telemetry)
            .add_metric(
                "test.bar",
                MetricType::Distribution,
                MetricNamespace::General,
            );

        assert_eq!(&builder.service_name.unwrap(), "test_service");
        assert_eq!(&builder.language.unwrap(), "test_language");
        assert_eq!(&builder.language_version.unwrap(), "test_language_version");
        assert_eq!(&builder.tracer_version.unwrap(), "test_tracer_version");
        assert_eq!(builder.interval, 100_u64);
        assert_eq!(builder.config.endpoint, None);
        assert!(!builder.config.restartable);
        assert!(!builder.config.direct_submission_enabled);
        assert_eq!(
            builder.config.telemetry_hearbeat_interval,
            Duration::new(60, 0)
        );
        assert!(!builder.config.telemetry_debug_logging_enabled);
        assert_eq!(
            *builder.metrics.get("test.foo").unwrap(),
            SelfMetric(MetricType::Count, MetricNamespace::Telemetry)
        );
        assert_eq!(
            *builder.metrics.get("test.bar").unwrap(),
            SelfMetric(MetricType::Distribution, MetricNamespace::General)
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn spawn_test() {
        let config = Config::default();

        let client = TelemetryClientBuilder::new()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_interval(100)
            .set_config(config)
            .add_metric("test.foo", MetricType::Count, MetricNamespace::Telemetry)
            .add_metric(
                "test.bar",
                MetricType::Distribution,
                MetricNamespace::General,
            )
            .spawn()
            .await;

        assert!(client.is_ok());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn run_test() {
        let server = MockServer::start_async().await;

        let telemetry_srv = server.mock_async(|when, then| {
            when.method(POST)
                .body_contains("\"payload\":[{\"request_type\":\"sketches\",\"payload\":{\"series\":[{\"namespace\":\"tracers\",\"metric\":\"trace_api.bytes\",\"tags\":[\"src_library:libdatadog\"]")
                .path("/telemetry/proxy/api/v2/apmtelemetry");
            then.status(200).body("");
        })
        .await;

        let mut config = Config {
            telemetry_hearbeat_interval: Duration::from_secs(1),
            ..Default::default()
        };
        let _ = config.set_endpoint(Endpoint::from_url(
            server.url("/").parse::<hyper::Uri>().unwrap(),
        ));

        let client = TelemetryClientBuilder::new()
            .set_service_name("test_service")
            .set_language("test_language")
            .set_language_version("test_language_version")
            .set_tracer_version("test_tracer_version")
            .set_interval(100)
            .set_config(config)
            .add_metric(
                "trace_api.bytes",
                MetricType::Distribution,
                MetricNamespace::Tracers,
            )
            .spawn()
            .await;

        assert!(client.is_ok());

        // Ensure metrics are only sent once by just allowing one interval.
        let cancel_future =
            tokio::time::sleep_until(tokio::time::Instant::now() + Duration::from_millis(100));
        // Mock metrics retrieval
        let mut global_metrics = MetricBucket::default();
        global_metrics.update(1);

        client
            .unwrap()
            .run(|| global_metrics.get(), cancel_future)
            .await;

        // Assert that just one payload contains the 'trace.api_bytes' metric.
        telemetry_srv.assert_hits_async(1).await;
    }
}
