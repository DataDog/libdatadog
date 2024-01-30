use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use ddtelemetry::{
    data::metrics::{MetricNamespace, MetricType},
    metrics::ContextKey,
    worker::{LifecycleAction, TelemetryActions, TelemetryWorkerBuilder, TelemetryWorkerHandle},
};
use futures::{
    future::{BoxFuture, Shared},
    Future, FutureExt,
};
use manual_future::ManualFuture;
use tokio::{select, sync::mpsc::Receiver, task::JoinHandle, time::Interval};

use crate::interface::SidecarServer;

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
        futures::future::join_all(vec![
            self.send(
                self.submitted_payloads,
                self.server.submitted_payloads.swap(0, Ordering::Relaxed) as f64,
            ),
            self.send(
                self.active_sessions,
                self.server.active_session_count() as f64,
            ),
        ])
        .await;
    }
}

pub struct Watchdog {
    interval: tokio::time::Interval,
    max_memory_usage_bytes: usize, 
    shutdown_receiver: Receiver<()>,
}

#[derive(Clone)]
pub struct WatchdogHandle {
    handle: Shared<BoxFuture<'static, Option<()>>>,
    pub mem_usage_bytes: Arc<AtomicUsize>,
}

impl WatchdogHandle {
    pub async fn wait_for_shutdown(&self) {
        self.handle.clone().await;
    }
}

impl Watchdog {
    pub fn from_receiver(shutdown_receiver: Receiver<()>) -> Self {
        Watchdog {
            interval: tokio::time::interval(Duration::from_secs(60)),
            max_memory_usage_bytes: 1 * 1024 * 1024 * 1024, // 1 GB
            shutdown_receiver,
        }
    }

    pub fn spawn_watchdog(mut self) -> WatchdogHandle {
        let mem_usage_bytes = Arc::new(AtomicUsize::new(0));
        let handle_mem_usage_bytes = mem_usage_bytes.clone();

        let join_handle = tokio::spawn(async move {
            mem_usage_bytes.store(0, Ordering::Relaxed);

            loop {
                select! {
                    _ = self.interval.tick() => {
                        let current_mem_usage_bytes = memory_stats::memory_stats()
                        .map(|s| s.physical_mem)
                        .unwrap_or(0);
                        mem_usage_bytes.store(current_mem_usage_bytes, Ordering::Relaxed);

                        if current_mem_usage_bytes > self.max_memory_usage_bytes {
                            std::thread::spawn(||{
                                // wait 5 seconds to give metrics a chance to flush - then kill the process
                                std::thread::sleep(Duration::from_secs(5)); 
                                std::process::exit(1);
                            });
                            return
                        }
                        
                    },
                    _ = self.shutdown_receiver.recv() => {
                        return
                    },
                }
            }
        });
        WatchdogHandle {
            handle: join_handle.map(Result::ok).boxed().shared(),
            mem_usage_bytes: handle_mem_usage_bytes,
        }
    }
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
                self.watchdog_handle.wait_for_shutdown();
                return;
            }
        };

        let metrics = MetricData {
            worker: &worker,
            server: &self.server,
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

struct SelfTelemetryWorker {}

impl SelfTelemetryWorker {
    // pub async fn setup_with_config(config: ddtelemetry::config::Config) -> anyhow::Result<Self> {

    // }
}
