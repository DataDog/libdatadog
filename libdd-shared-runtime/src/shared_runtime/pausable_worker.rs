// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Defines a pausable worker to be able to stop background processes before forks

use crate::worker::Worker;
use libdd_capabilities::MaybeSend;
use std::fmt::Display;
use tokio::{runtime::Runtime, select, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// A pausable worker which can be paused and restarted on forks.
///
/// Used to allow a [`super::Worker`] to be paused while saving its state when
/// dropping a tokio runtime to be able to restart with the same state on a new runtime. This is
/// used to stop all threads before a fork to avoid deadlocks in child.
#[derive(Debug)]
pub enum PausableWorker<T: Worker + MaybeSend + Sync + 'static> {
    Running {
        handle: JoinHandle<T>,
        stop_token: CancellationToken,
    },
    Paused {
        worker: T,
    },
    InvalidState,
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

    /// Start the worker on the given runtime.
    ///
    /// The worker's main loop will be run on the runtime.
    pub fn start(&mut self, rt: &Runtime) -> Result<(), PausableWorkerError> {
        #[cfg(target_arch = "wasm32")]
        return Ok(());
        #[cfg(not(target_arch = "wasm32"))]
        match self {
            PausableWorker::Running { .. } => Ok(()),
            PausableWorker::Paused { worker } => {
                debug!(?worker, "Starting pausable worker");
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
                let handle = rt.spawn(async move {
                    // First iteration using initial_trigger
                    select! {
                            // Always check for cancellation first, to reduce time-to-pause in case
                            // the initial trigger is always ready.
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
                            // Always check for cancellation first, to reduce time-to-pause in case
                            // the trigger is always ready.
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
                });

                *self = PausableWorker::Running { handle, stop_token };
                Ok(())
            }
            PausableWorker::InvalidState => Err(PausableWorkerError::InvalidState),
        }
    }

    /// Pause the worker and wait for it to complete, storing its state for restart.
    ///
    /// # Errors
    /// Fails if the worker handle has been aborted preventing the worker from being retrieved.
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

                if let Ok(mut worker) = handle.await {
                    debug!(?worker, "Worker paused successfully");
                    worker.on_pause().await;
                    *self = PausableWorker::Paused { worker };
                    Ok(())
                } else {
                    // The task has been aborted and the worker can't be retrieved.
                    *self = PausableWorker::InvalidState;
                    Err(PausableWorkerError::TaskAborted)
                }
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
        let mut pausable_worker = PausableWorker::new(worker);

        pausable_worker.start(&runtime).unwrap();

        assert_eq!(receiver.recv().unwrap(), 0);
        runtime.block_on(async { pausable_worker.pause().await.unwrap() });
        // Empty the message queue and get the last message
        let mut next_message = 1;
        for message in receiver.try_iter() {
            next_message = message + 1;
        }
        pausable_worker.start(&runtime).unwrap();
        assert_eq!(receiver.recv().unwrap(), next_message);
    }
}
