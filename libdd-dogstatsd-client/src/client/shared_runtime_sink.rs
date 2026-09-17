// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Provides a buffered sink running on a [libdd_shared_runtime::SharedRuntime]

use anyhow::anyhow;
use async_trait::async_trait;
use cadence::{MetricSink, SinkStats};
use libdd_common::Endpoint;
use libdd_shared_runtime::{worker::Worker, SharedRuntime, WorkerHandle};
use std::fmt;
use std::io;
use std::panic::RefUnwindSafe;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::error;

use super::{sink, QUEUE_SIZE};

/// A [`MetricSink`] that offloads sent metrics to a [`SharedRuntime`].
#[derive(Clone)]
pub struct SharedRuntimeMetricSink {
    sender: mpsc::Sender<String>,
    sink: Arc<dyn MetricSink + Send + Sync + RefUnwindSafe>,
}

impl fmt::Debug for SharedRuntimeMetricSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedRuntimeMetricSink {{ {:?} }}", self.sender)
    }
}

impl MetricSink for SharedRuntimeMetricSink {
    fn emit(&self, metric: &str) -> io::Result<usize> {
        let len = metric.len();
        self.sender
            .try_send(metric.to_owned())
            .map(|_| len)
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "dogstatsd channel full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "dogstatsd channel closed")
                }
            })
    }

    fn flush(&self) -> Result<(), std::io::Error> {
        self.sink.flush()
    }

    fn stats(&self) -> SinkStats {
        self.sink.stats()
    }
}

/// A [`Worker`] that drains metrics from a channel and forwards them to a
/// wrapped [`MetricSink`] (e.g. `UdpMetricSink`).
pub struct MetricSinkWorker {
    receiver: mpsc::Receiver<String>,
    sink: Arc<dyn MetricSink + Send + Sync + RefUnwindSafe>,
    pending: Option<String>,
}

impl std::fmt::Debug for MetricSinkWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricSinkWorker")
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl Worker for MetricSinkWorker {
    /// Awaits the next metric from the channel, storing it in `pending`.
    ///
    /// If the channel is closed this future never resolves, allowing the
    /// SharedRuntime to cancel it cleanly at the next yield point.
    async fn trigger(&mut self) {
        match self.receiver.recv().await {
            Some(metric) => self.pending = Some(metric),
            None => {
                // Channel closed — park until cancelled by the runtime.
                std::future::pending::<()>().await;
            }
        }
    }

    /// Forwards the single metric stored by `trigger` to the wrapped sink.
    async fn run(&mut self) {
        if let Some(metric) = self.pending.take() {
            if let Err(e) = self.sink.emit(&metric) {
                error!(?e, "MetricSinkWorker: failed to emit metric");
            }
        }
    }

    /// Drains any remaining queued metrics and flushes the wrapped sink.
    async fn shutdown(&mut self) {
        // Forward the metric that was sitting in `pending`, if any.
        if let Some(metric) = self.pending.take() {
            if let Err(e) = self.sink.emit(&metric) {
                error!(?e, "MetricSinkWorker: failed to emit metric on shutdown");
            }
        }
        // Drain the channel.
        while let Ok(metric) = self.receiver.try_recv() {
            if let Err(e) = self.sink.emit(&metric) {
                error!(?e, "MetricSinkWorker: failed to emit metric on shutdown");
            }
        }
        if let Err(e) = self.sink.flush() {
            error!(?e, "MetricSinkWorker: failed to flush sink on shutdown");
        }
    }

    /// Reset the worker in the child process after a fork.
    fn reset(&mut self) {
        self.pending = None;
        // Drain the channel
        while self.receiver.try_recv().is_ok() {}
    }
}

pub fn create_shared_runtime_sink(
    endpoint: &Endpoint,
    runtime: &impl SharedRuntime,
) -> anyhow::Result<(SharedRuntimeMetricSink, WorkerHandle)> {
    let (tx, rx) = mpsc::channel(QUEUE_SIZE);

    let sink: Arc<dyn MetricSink + Send + Sync + RefUnwindSafe> = match endpoint.url.scheme_str() {
        #[cfg(unix)]
        Some("unix") => Arc::new(sink::create_unix_sink(endpoint)?),
        _ => Arc::new(sink::create_udp_sink(endpoint)?),
    };

    let sink_worker = MetricSinkWorker {
        receiver: rx,
        sink: sink.clone(),
        pending: None,
    };

    let handle = runtime
        .spawn_worker(sink_worker, true)
        .map_err(|e| anyhow!("failed to spawn MetricSinkWorker: {e}"))?;

    let shared_sink = SharedRuntimeMetricSink { sender: tx, sink };

    Ok((shared_sink, handle))
}
