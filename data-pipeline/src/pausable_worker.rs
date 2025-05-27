// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Defines a pausable worker to be able to stop background processes before forks
use anyhow::{anyhow, Result};
use ddtelemetry::worker::TelemetryWorker;
use tokio::{runtime::Runtime, select, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::{agent_info::AgentInfoFetcher, stats_exporter::StatsExporter};

/// Trait representing a worker which can be wrapped by `PausableWorker`
pub trait Worker {
    /// Main worker loop
    fn run(&mut self) -> impl std::future::Future<Output = ()> + Send;
}

impl Worker for StatsExporter {
    async fn run(&mut self) {
        Self::run(self).await
    }
}

impl Worker for AgentInfoFetcher {
    async fn run(&mut self) {
        Self::run(self).await
    }
}

impl Worker for TelemetryWorker {
    async fn run(&mut self) {
        Self::run(self).await
    }
}

/// A pausable worker which can be paused and restarded on forks.
///
/// # Requirements
/// When paused the worker will exit on the next awaited call. To be able to safely restart the
/// worker must be in a valid state on every call to `.await`.
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
    /// Create a new pausable worker from the given worker.
    pub fn new(worker: T) -> Self {
        Self::Paused { worker }
    }

    /// Start the worker on the given runtime.
    ///
    /// # Errors
    /// Fails if the worker is in an invalid state.
    pub fn start(&mut self, rt: &Runtime) -> Result<()> {
        if let Self::Running { .. } = self {
            Ok(())
        } else if let Self::Paused { mut worker } = std::mem::replace(self, Self::InvalidState) {
            // Worker is temporarly in an invalid state, but since this block is failsafe it will
            // be replaced by a valid state.
            let stop_token = CancellationToken::new();
            let cloned_token = stop_token.clone();
            let handle = rt.spawn(async move {
                select! {
                    _ = worker.run() => {worker}
                    _ = cloned_token.cancelled() => {worker}
                }
            });

            *self = PausableWorker::Running { handle, stop_token };
            Ok(())
        } else {
            Err(anyhow!("Failed to start service"))
        }
    }

    /// Pause the worker saving it's state to be restarted.
    ///
    /// # Errors
    /// Fails if the worker handle has been aborted preventing the worker from being retrieved.
    pub async fn pause(&mut self) -> Result<()> {
        match self {
            PausableWorker::Running { handle, stop_token } => {
                stop_token.cancel();
                if let Ok(worker) = handle.await {
                    *self = PausableWorker::Paused { worker };
                    Ok(())
                } else {
                    // Worker isn't retrieved and can't be restarted.
                    *self = PausableWorker::InvalidState;
                    Err(anyhow!("Failed to stop worker. Worker must be recreated"))
                }
            }
            PausableWorker::Paused { .. } => Ok(()),
            PausableWorker::InvalidState => Err(anyhow!("Worker is in invalid state")),
        }
    }

    /// Wait for the run method of the worker to exit.
    pub async fn join(self) -> Result<()> {
        if let PausableWorker::Running { handle, .. } = self {
            handle.await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tokio::{runtime::Builder, time::sleep};

    use super::*;
    use std::{
        sync::mpsc::{channel, Sender},
        time::Duration,
    };

    /// Test worker incrementing the state and sending it with the sender.
    struct TestWorker {
        state: u32,
        sender: Sender<u32>,
    }

    impl Worker for TestWorker {
        async fn run(&mut self) {
            loop {
                let _ = self.sender.send(self.state);
                self.state += 1;
                sleep(Duration::from_millis(100)).await;
            }
        }
    }

    #[test]
    fn test_restart() {
        let (sender, receiver) = channel::<u32>();
        let worker = TestWorker { state: 0, sender };
        let runtime = Builder::new_multi_thread().enable_time().build().unwrap();
        let mut pausable_worker = PausableWorker::new(worker);

        pausable_worker.start(&runtime).unwrap();

        assert_eq!(receiver.recv().unwrap(), 0);
        runtime.block_on(async { pausable_worker.pause().await.unwrap() });
        pausable_worker.start(&runtime).unwrap();
        assert_eq!(receiver.recv().unwrap(), 1);
    }
}
