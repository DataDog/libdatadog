// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Defines a pausable worker to be able to stop background processes before forks

use crate::worker::Worker;
use core::pin::Pin;
use futures::FutureExt;
use libdd_capabilities::spawn::SpawnCapability;
use libdd_capabilities::MaybeSend;
use std::fmt::Display;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::debug;

#[cfg(not(target_arch = "wasm32"))]
type WorkerJoinHandle<T> = Pin<Box<dyn Future<Output = Result<T, PausableWorkerError>> + Send>>;

#[cfg(target_arch = "wasm32")]
type WorkerJoinHandle<T> = Pin<Box<dyn Future<Output = Result<T, PausableWorkerError>>>>;

/// A pausable worker which can be paused and restarted on forks.
///
/// Used to allow a [`super::Worker`] to be paused while saving its state when
/// dropping a tokio runtime to be able to restart with the same state on a new runtime. This is
/// used to stop all threads before a fork to avoid deadlocks in child.
pub enum PausableWorker<T: Worker + MaybeSend + Sync + 'static> {
    Running {
        handle: WorkerJoinHandle<T>,
        stop_token: CancellationToken,
    },
    Paused {
        worker: T,
    },
    InvalidState,
}

impl<T: Worker + MaybeSend + Sync + 'static> std::fmt::Debug for PausableWorker<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running { .. } => f.debug_struct("PausableWorker::Running").finish(),
            Self::Paused { worker } => f
                .debug_struct("PausableWorker::Paused")
                .field("worker", worker)
                .finish(),
            Self::InvalidState => write!(f, "PausableWorker::InvalidState"),
        }
    }
}

#[derive(Debug)]
pub enum PausableWorkerError {
    InvalidState,
    TaskAborted,
}

impl Display for PausableWorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PausableWorkerError::InvalidState => {
                write!(f, "Worker is in an invalid state and must be recreated.")
            }
            PausableWorkerError::TaskAborted => {
                write!(f, "Worker task has been aborted and state has been lost.")
            }
        }
    }
}

impl core::error::Error for PausableWorkerError {}

impl<T: Worker + MaybeSend + Sync + 'static> PausableWorker<T> {
    /// Create a new pausable worker from the given worker.
    pub fn new(worker: T) -> Self {
        Self::Paused { worker }
    }

    /// Start the worker using the given spawn capability.
    ///
    /// The worker's main loop will be spawned via the provided spawner.
    /// `ctx` is the platform-specific runtime context (e.g. `tokio::runtime::Handle`
    /// on native, `()` on wasm).
    pub fn start<S: SpawnCapability>(
        &mut self,
        spawner: &S,
        ctx: &S::RuntimeContext,
    ) -> Result<(), PausableWorkerError>
    where
        S::JoinHandle<T>: 'static,
    {
        match self {
            PausableWorker::Running { .. } => Ok(()),
            PausableWorker::Paused { .. } => {
                debug!(?self, "Starting pausable worker");
                let PausableWorker::Paused { mut worker } =
                    std::mem::replace(self, PausableWorker::InvalidState)
                else {
                    // Unreachable
                    return Ok(());
                };

                // Worker is temporarily in an invalid state, but since this block is failsafe it
                // will be replaced by a valid state.
                let stop_token = CancellationToken::new();
                let cloned_token = stop_token.clone();
                let handle = spawner.spawn(
                    async move {
                        // First iteration using initial_trigger
                        select! {
                            // Always check for cancellation first, to reduce time-to-pause in
                            // case the initial trigger is always ready.
                            biased;
                            _ = cloned_token.cancelled() => {
                                return worker;
                            }
                            _ = worker.initial_trigger() => {
                                worker.run().await;
                            }
                        }

                        // Regular iterations
                        loop {
                            select! {
                                // Always check for cancellation first, to reduce time-to-pause
                                // in case the trigger is always ready.
                                biased;
                                _ = cloned_token.cancelled() => {
                                    break;
                                }
                                _ = worker.trigger() => {
                                    worker.run().await;
                                }
                            }
                        }
                        worker
                    },
                    ctx,
                );

                let safe_handle = AssertUnwindSafe(handle)
                    .catch_unwind()
                    .map(|result| result.map_err(|_| PausableWorkerError::TaskAborted));

                *self = PausableWorker::Running {
                    handle: Box::pin(safe_handle),
                    stop_token,
                };
                Ok(())
            }
            PausableWorker::InvalidState => Err(PausableWorkerError::InvalidState),
        }
    }

    /// Pause the worker and wait for it to complete, storing its state for restart.
    ///
    /// # Errors
    /// Fails if the worker is in an invalid state.
    pub async fn pause(&mut self) -> Result<(), PausableWorkerError> {
        match self {
            PausableWorker::Running { .. } => {
                debug!("Waiting for worker to pause");
                let PausableWorker::Running { handle, stop_token } =
                    std::mem::replace(self, PausableWorker::InvalidState)
                else {
                    // Unreachable
                    return Ok(());
                };

                if !stop_token.is_cancelled() {
                    stop_token.cancel();
                }

                let mut worker = match handle.await {
                    Ok(worker) => worker,
                    Err(e) => {
                        *self = PausableWorker::InvalidState;
                        return Err(e);
                    }
                };
                debug!(?worker, "Worker paused successfully");
                worker.on_pause().await;
                *self = PausableWorker::Paused { worker };
                Ok(())
            }
            PausableWorker::Paused { .. } => Ok(()),
            PausableWorker::InvalidState => Err(PausableWorkerError::InvalidState),
        }
    }

    /// Reset the worker state (e.g. in a fork child).
    pub fn reset(&mut self) {
        if let PausableWorker::Paused { worker } = self {
            worker.reset();
        }
    }

    /// Shutdown the worker.
    pub async fn shutdown(&mut self) {
        if let PausableWorker::Paused { worker } = self {
            worker.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tokio::{runtime::Builder, time::sleep};

    use super::*;
    use crate::shared_runtime::RuntimeSpawner;
    use std::{
        sync::mpsc::{channel, Sender},
        time::Duration,
    };

    /// Test worker incrementing the state and sending it with the sender.
    #[derive(Debug)]
    struct TestWorker {
        state: u32,
        sender: Sender<u32>,
    }

    #[async_trait]
    impl Worker for TestWorker {
        async fn run(&mut self) {
            let _ = self.sender.send(self.state);
            self.state += 1;
        }

        async fn trigger(&mut self) {
            sleep(Duration::from_millis(100)).await;
        }
    }

    #[test]
    fn test_restart() {
        let (sender, receiver) = channel::<u32>();
        let worker = TestWorker { state: 0, sender };
        let runtime = Builder::new_multi_thread().enable_time().build().unwrap();
        let handle = runtime.handle().clone();
        let spawner = RuntimeSpawner;
        let mut pausable_worker = PausableWorker::new(worker);

        pausable_worker.start(&spawner, &handle).unwrap();

        assert_eq!(receiver.recv().unwrap(), 0);
        runtime.block_on(async { pausable_worker.pause().await.unwrap() });
        // Empty the message queue and get the last message
        let mut next_message = 1;
        for message in receiver.try_iter() {
            next_message = message + 1;
        }
        pausable_worker.start(&spawner, &handle).unwrap();
        assert_eq!(receiver.recv().unwrap(), next_message);
    }

    /// Worker that panics on its first `run()` call.
    #[derive(Debug)]
    struct PanickingWorker;

    #[async_trait]
    impl Worker for PanickingWorker {
        async fn run(&mut self) {
            panic!("intentional panic in worker");
        }

        async fn trigger(&mut self) {
            sleep(Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn test_panicking_worker_returns_task_aborted() {
        let runtime = Builder::new_multi_thread().enable_time().build().unwrap();
        let handle = runtime.handle().clone();
        let spawner = RuntimeSpawner;
        let mut pausable_worker = PausableWorker::new(PanickingWorker);

        pausable_worker.start(&spawner, &handle).unwrap();

        let result = runtime.block_on(async { pausable_worker.pause().await });
        assert!(
            matches!(result, Err(PausableWorkerError::TaskAborted)),
            "expected TaskAborted, got {result:?}"
        );
        assert!(
            matches!(pausable_worker, PausableWorker::InvalidState),
            "worker should be in InvalidState after panic"
        );
    }
}
