// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::config::Config;
use crate::service::SidecarServer;
use crate::watchdog::WatchdogHandle;
use data_pipeline::telemetry::TelemetryClientBuilder;
use ddtelemetry::data::metrics::{MetricNamespace, MetricType};
use manual_future::ManualFuture;
use std::sync::atomic::Ordering;
use tokio::select;
use tokio::task::JoinHandle;

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
        select! {
            _ = watchdog_handle.wait_for_shutdown() => { },
            config = future => {
                let mut client = match TelemetryClientBuilder::new()
                    .set_service_name("datadog-ipc-helper")
                    .set_language("php")
                    .set_language_version("SIDECAR")
                    .set_tracer_version(crate::sidecar_version!())
                    .set_config(config)
                    .set_interval(60000)
                    .add_metric("server.submitted_payloads", MetricType::Count, MetricNamespace::Sidecar)
                    .add_metric("server.active_sessions", MetricType::Gauge, MetricNamespace::Sidecar)
                    .add_metric("server.memory_usage", MetricType::Distribution, MetricNamespace::Sidecar)
                    .add_metric("logs_created", MetricType::Count, MetricNamespace::General)
                    .add_metric("trace_api.requests", MetricType::Count, MetricNamespace::Tracers)
                    .add_metric("trace_api.responses", MetricType::Count, MetricNamespace::Tracers)
                    .add_metric("trace_api.errors", MetricType::Count, MetricNamespace::Tracers)
                    .add_metric("trace_api.bytes", MetricType::Count, MetricNamespace::Tracers)
                    .add_metric("trace_api.bytes", MetricType::Distribution, MetricNamespace::Tracers)
                    .add_metric("trace_chunks_sent", MetricType::Count, MetricNamespace::Tracers)
                    .add_metric("trace_chunks_dropped", MetricType::Count, MetricNamespace::Tracers)
                    .spawn().await {
                        Ok(client) => client,
                        Err(_err) => {
                            watchdog_handle.wait_for_shutdown().await;
                            return;
                        }
                    };

                client.run(|| {
                    let mut metrics = server.trace_flusher.collect_metrics();
                    metrics.submitted_payloads = server.submitted_payloads.swap(0, Ordering::Relaxed);
                    metrics.active_sessions = server.active_session_count();
                    metrics.memory_usage = watchdog_handle.mem_usage_bytes.load(Ordering::Relaxed);
                    metrics
                },
                watchdog_handle.clone())
                .await;

            },
        }
    })
}
