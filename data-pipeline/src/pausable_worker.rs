//! Defines a pausable worker to be able to stop background processes before forks
use ddtelemetry::worker::TelemetryWorker;
use tokio::{runtime::Runtime, select, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{agent_info::AgentInfoFetcher, stats_exporter::StatsExporter};

pub trait Worker {
    /// Main worker loop
    fn run(&mut self) -> impl std::future::Future<Output = ()> + Send;
    /// Hook called on the worker when pausing.
    /// The worker can be paused on any await call. This hook can be use to clean the worker
    /// state after pausing.
    fn on_pause(&mut self);
}

impl Worker for StatsExporter {
    async fn run(&mut self) {
        Self::run(self).await
    }
    fn on_pause(&mut self) {}
}

impl Worker for AgentInfoFetcher {
    async fn run(&mut self) {
        Self::run(self).await
    }
    fn on_pause(&mut self) {}
}

impl Worker for TelemetryWorker {
    async fn run(&mut self) {
        Self::run(self).await
    }
    fn on_pause(&mut self) {
        self.cleanup();
    }
}

/// A pausable worker which can be paused and restarded on forks.
#[derive(Debug)]
pub enum PausableWorker<T: Worker + Send + Sync + 'static> {
    Running {
        handle: JoinHandle<T>,
        stop_token: CancellationToken,
    },
    Paused {
        worker: T,
    },
    InvalidState,
}

impl<T: Worker + Send + Sync + 'static> PausableWorker<T> {
    pub fn new(worker: T) -> Self {
        Self::Paused { worker }
    }

    pub fn start(&mut self, rt: &Runtime) {
        if let Self::Paused { mut worker } = std::mem::replace(self, Self::InvalidState) {
            let stop_token = CancellationToken::new();
            let cloned_token = stop_token.clone();
            let handle = rt.spawn(async move {
                select! {
                    _ = worker.run() => {worker}
                    _ = cloned_token.cancelled() => {worker}
                }
            });

            *self = PausableWorker::Running { handle, stop_token };
        }
    }

    pub async fn stop(&mut self) {
        if let PausableWorker::Running { handle, stop_token } = self {
            stop_token.cancel();
            let worker = handle.await.unwrap();
            worker.on_pause();
            *self = PausableWorker::Paused { worker };
        }
    }

    /// Wait for the run method of the worker to exit.
    pub async fn join(&mut self) {
        if let PausableWorker::Running { handle, stop_token } = self {
            let worker = handle.await.unwrap();
            *self = PausableWorker::Paused { worker };
        }
    }
}
