use crate::config::Config;
use crate::interface::SidecarServer;
use ddtelemetry::data::metrics::{MetricNamespace, MetricType};
use ddtelemetry::metrics::ContextKey;
use ddtelemetry::worker::{
    LifecycleAction, TelemetryActions, TelemetryWorkerBuilder, TelemetryWorkerHandle,
};
use futures::future;
use manual_future::ManualFuture;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::select;
use tokio::sync::mpsc::Receiver;
use tokio::task::JoinHandle;

struct MetricData<'a> {
    worker: &'a TelemetryWorkerHandle,
    server: &'a SidecarServer,
    submitted_payloads: ContextKey,
    active_sessions: ContextKey,
}
impl<'a> MetricData<'a> {
    async fn send(&self, key: ContextKey, value: f64) {
        let _ = self
            .worker
            .send_msg(TelemetryActions::AddPoint((value, key, vec![])))
            .await;
    }

    async fn collect_and_send(&self) {
        future::join_all(vec![
            self.send(
                self.submitted_payloads,
                self.server.submitted_payloads.swap(0, Ordering::SeqCst) as f64,
            ),
            self.send(
                self.active_sessions,
                self.server.active_session_count() as f64,
            ),
        ])
        .await;
    }
}

pub fn self_telemetry(
    server: SidecarServer,
    mut shutdown_receiver: Receiver<()>,
) -> JoinHandle<()> {
    if !Config::get().self_telemetry {
        return tokio::spawn(async move {
            shutdown_receiver.recv().await;
        });
    }

    let (future, completer) = ManualFuture::new();
    server
        .self_telemetry_config
        .lock()
        .unwrap()
        .replace(completer);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));

        select! {
            _ = shutdown_receiver.recv() => { },
            config = future => {
                if let Ok((worker, join_handle)) = TelemetryWorkerBuilder::new_fetch_host(
                    "datadog-ipc-helper".to_string(),
                    "php".to_string(),
                    "SIDECAR".to_string(),
                    env!("CARGO_PKG_VERSION").to_string(),
                )
                .spawn_with_config(config)
                .await
                {
                    let metrics = MetricData {
                        worker: &worker,
                        server: &server,
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
                    };

                    let _ = worker
                        .send_msg(TelemetryActions::Lifecycle(LifecycleAction::Start))
                        .await;
                    loop {
                        select! {
                            _ = interval.tick() => {
                                metrics.collect_and_send().await;
                                let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushMetricAggr)).await;
                                let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::FlushData)).await;
                            },
                            _ = shutdown_receiver.recv() => {
                                metrics.collect_and_send().await;
                                let _ = worker.send_msg(TelemetryActions::Lifecycle(LifecycleAction::Stop)).await;
                                let _ = join_handle.await;
                                return
                            },
                        }
                    }
                } else {
                    shutdown_receiver.recv().await;
                }
            },
        }
    })
}
