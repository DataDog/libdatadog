// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::config::Config;
use crate::log;
use crate::service::SidecarServer;
use crate::watchdog::WatchdogHandle;
use ddcommon::{tag, tag::Tag, MutexExt};
use libdd_telemetry::data::metrics::{MetricNamespace, MetricType};
use libdd_telemetry::metrics::ContextKey;
use libdd_telemetry::worker::{
    LifecycleAction, TelemetryActions, TelemetryWorkerBuilder, TelemetryWorkerHandle,
};
use manual_future::ManualFuture;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::select;
use tokio::task::JoinHandle;

struct MetricData<'a> {
    worker: &'a TelemetryWorkerHandle,
    sidecar_watchdog: &'a WatchdogHandle,
    server: &'a SidecarServer,
    submitted_payloads: ContextKey,
    active_sessions: ContextKey,
    memory_usage: ContextKey,
    logs_created: ContextKey,
    trace_api_requests: ContextKey,
    trace_api_responses: ContextKey,
    trace_api_errors: ContextKey,
    trace_api_bytes: ContextKey,
    trace_chunks_sent: ContextKey,
    trace_chunks_dropped: ContextKey,
}
impl MetricData<'_> {
    async fn send(&self, key: ContextKey, value: f64, tags: Vec<Tag>) {
        let _ = self
            .worker
            .send_msg(TelemetryActions::AddPoint((value, key, tags)))
            .await;
    }

    async fn collect_and_send(&self) {
        let trace_metrics = self.server.trace_flusher.collect_metrics();

        let mut futures = vec![
            self.send(
                self.submitted_payloads,
                self.server.submitted_payloads.swap(0, Ordering::Relaxed) as f64,
                vec![],
            ),
            self.send(
                self.active_sessions,
                self.server.active_session_count() as f64,
                vec![],
            ),
            self.send(
                self.memory_usage,
                self.sidecar_watchdog
                    .mem_usage_bytes
                    .load(Ordering::Relaxed) as f64,
                vec![],
            ),
        ];
        for (level, count) in log::MULTI_LOG_FILTER
            .collect_logs_created_count()
            .into_iter()
        {
            #[allow(clippy::unwrap_used)]
            futures.push(self.send(
                self.logs_created,
                count as f64,
                vec![
                    Tag::new("level", level.as_str().to_lowercase()).unwrap(),
                    tag!("src_library", "libdatadog"),
                ],
            ));
        }
        if trace_metrics.api_requests > 0 {
            #[allow(clippy::unwrap_used)]
            futures.push(self.send(
                self.trace_api_requests,
                trace_metrics.api_requests as f64,
                vec![Tag::new("src_library", "libdatadog").unwrap()],
            ));
        }
        if trace_metrics.api_errors_network > 0 {
            futures.push(self.send(
                self.trace_api_errors,
                trace_metrics.api_errors_network as f64,
                vec![tag!("type", "network"), tag!("src_library", "libdatadog")],
            ));
        }
        if trace_metrics.api_errors_timeout > 0 {
            futures.push(self.send(
                self.trace_api_errors,
                trace_metrics.api_errors_timeout as f64,
                vec![tag!("type", "timeout"), tag!("src_library", "libdatadog")],
            ));
        }
        if trace_metrics.api_errors_status_code > 0 {
            futures.push(self.send(
                self.trace_api_errors,
                trace_metrics.api_errors_status_code as f64,
                vec![
                    tag!("type", "status_code"),
                    tag!("src_library", "libdatadog"),
                ],
            ));
        }
        if trace_metrics.bytes_sent > 0 {
            futures.push(self.send(
                self.trace_api_bytes,
                trace_metrics.bytes_sent as f64,
                vec![tag!("src_library", "libdatadog")],
            ));
        }
        if trace_metrics.chunks_sent > 0 {
            futures.push(self.send(
                self.trace_chunks_sent,
                trace_metrics.chunks_sent as f64,
                vec![tag!("src_library", "libdatadog")],
            ));
        }
        if trace_metrics.chunks_dropped > 0 {
            futures.push(self.send(
                self.trace_chunks_dropped,
                trace_metrics.chunks_dropped as f64,
                vec![tag!("src_library", "libdatadog")],
            ));
        }
        for (status_code, count) in &trace_metrics.api_responses_count_per_code {
            #[allow(clippy::unwrap_used)]
            futures.push(self.send(
                self.trace_api_responses,
                *count as f64,
                vec![
                    Tag::new("status_code", status_code.to_string().as_str()).unwrap(),
                    tag!("src_library", "libdatadog"),
                ],
            ));
        }

        futures::future::join_all(futures).await;
    }
}

pub fn self_telemetry(server: SidecarServer, watchdog_handle: WatchdogHandle) -> JoinHandle<()> {
    if !Config::get().self_telemetry {
        return tokio::spawn(async move {
            watchdog_handle.wait_for_shutdown().await;
        });
    }

    let (future, completer) = ManualFuture::new();
    server
        .self_telemetry_config
        .lock_or_panic()
        .replace(completer);

    tokio::spawn(async move {
        let submission_interval = tokio::time::interval(Duration::from_secs(60));

        select! {
            _ = watchdog_handle.wait_for_shutdown() => { },
            config = future => {
                let worker_cfg = SelfTelemetry { submission_interval, watchdog_handle, config, server };
                worker_cfg.spawn_worker().await
            },
        }
    })
}

pub struct SelfTelemetry {
    pub submission_interval: tokio::time::Interval,
    pub watchdog_handle: WatchdogHandle,
    pub config: libdd_telemetry::config::Config,
    pub server: SidecarServer,
}

impl SelfTelemetry {
    /// spawn_worker
    ///
    /// should always succeed
    /// not to bring down other functionality if we fail to initialize the internal telemetry
    pub async fn spawn_worker(mut self) {
        let mut builder = TelemetryWorkerBuilder::new_fetch_host(
            "datadog-ipc-helper".to_string(),
            "php".to_string(),
            "SIDECAR".to_string(),
            crate::sidecar_version!().to_string(),
        );
        builder.config = self.config.clone();
        let (worker, join_handle) = builder.spawn();

        let metrics = MetricData {
            worker: &worker,
            server: &self.server,
            sidecar_watchdog: &self.watchdog_handle,
            submitted_payloads: worker.register_metric_context(
                "server.submitted_payloads".to_string(),
                vec![],
                MetricType::Count,
                true,
                MetricNamespace::Sidecar,
            ),
            active_sessions: worker.register_metric_context(
                "server.active_sessions".to_string(),
                vec![],
                MetricType::Gauge,
                true,
                MetricNamespace::Sidecar,
            ),
            memory_usage: worker.register_metric_context(
                "server.memory_usage".to_string(),
                vec![],
                MetricType::Distribution,
                true,
                MetricNamespace::Sidecar,
            ),
            logs_created: worker.register_metric_context(
                "logs_created".to_string(),
                vec![],
                MetricType::Count,
                true,
                MetricNamespace::General,
            ),
            trace_api_requests: worker.register_metric_context(
                "trace_api.requests".to_string(),
                vec![],
                MetricType::Count,
                true,
                MetricNamespace::Tracers,
            ),
            trace_api_responses: worker.register_metric_context(
                "trace_api.responses".to_string(),
                vec![],
                MetricType::Count,
                true,
                MetricNamespace::Tracers,
            ),
            trace_api_errors: worker.register_metric_context(
                "trace_api.errors".to_string(),
                vec![],
                MetricType::Count,
                true,
                MetricNamespace::Tracers,
            ),
            trace_api_bytes: worker.register_metric_context(
                "trace_api.bytes".to_string(),
                vec![],
                MetricType::Distribution,
                true,
                MetricNamespace::Tracers,
            ),
            trace_chunks_sent: worker.register_metric_context(
                "trace_chunks_sent".to_string(),
                vec![],
                MetricType::Count,
                true,
                MetricNamespace::Tracers,
            ),
            trace_chunks_dropped: worker.register_metric_context(
                "trace_chunks_dropped".to_string(),
                vec![],
                MetricType::Count,
                true,
                MetricNamespace::Tracers,
            ),
        };

        let _ = worker
            .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
            .await;
        loop {
            select! {
                _ = self.submission_interval.tick() => {
                    metrics.collect_and_send().await;
                    let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushMetricAggr)).await;
                    let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData)).await;
                },
                _ = self.watchdog_handle.wait_for_shutdown() => {
                    metrics.collect_and_send().await;
                    let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::Stop)).await;
                    let _ = join_handle.await;
                    return
                },
            }
        }
    }
}
