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
use std::future::Future;
use std::time::Duration;
use tokio::select;
use tokio::task::JoinHandle;

pub trait CancellationObject {
    fn shutdown(&self) -> impl Future<Output = ()>;
}

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
    async fn enqueue_point(&self, value: f64, key: ContextKey, tags: Vec<Tag>) {
        let _ = self
            .worker
            .send_msg(TelemetryActions::AddPoint((value, key, tags)))
            .await;
    }

    pub async fn send(&mut self, metrics: Metrics) {
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
        U: FnMut() -> Metrics,
        C: CancellationObject,
    {
        let _ = self
            .worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
            .await;
        loop {
            select! {
                _ = self.interval.tick() => {
                    let metrics = update();
                    self.send(metrics).await;
                    let _ = self.worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushMetricAggr)).await;
                    let _ = self.worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData)).await;
                },
                _ = cancellation.shutdown() => {
                    let metrics = update();
                    self.send(metrics).await;
                    let _ = self.worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::Stop)).await;
                    let _ = (&mut self.handle).await;
                    return
                },
            }
        }
    }
}
