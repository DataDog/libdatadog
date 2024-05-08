// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::config::Config;
use crate::log;
use crate::service::SidecarServer;
use crate::watchdog::WatchdogHandle;
use ddcommon::tag::Tag;
use ddtelemetry::data::metrics::{MetricNamespace, MetricType};
use ddtelemetry::metrics::ContextKey;
use ddtelemetry::worker::{
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
}
impl<'a> MetricData<'a> {
    async fn send(&self, key: ContextKey, value: f64, tags: Vec<Tag>) {
        let _ = self
            .worker
            .send_msg(TelemetryActions::AddPoint((value, key, tags)))
            .await;
    }

    async fn collect_and_send(&self) {
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
            futures.push(self.send(
                self.logs_created,
                count as f64,
                vec![Tag::new("level", level.as_str().to_lowercase()).unwrap()],
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
        .lock()
        .unwrap()
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
    pub config: ddtelemetry::config::Config,
    pub server: SidecarServer,
}

impl SelfTelemetry {
    /// spawn_worker
    ///
    /// should always succeed
    /// not to bring down other functionality if we fail to initialize the internal telemetry
    pub async fn spawn_worker(mut self) {
        let (worker, join_handle) = match TelemetryWorkerBuilder::new_fetch_host(
            "datadog-ipc-helper".to_string(),
            "php".to_string(),
            "SIDECAR".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        )
        .spawn_with_config(self.config.clone())
        .await
        {
            Ok(r) => r,
            Err(_err) => {
                self.watchdog_handle.wait_for_shutdown().await;
                return;
            }
        };

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
