// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Defines a pausable worker to be able to stop background processes before forks

use ddcommon::worker::Worker;
use std::fmt::Display;
use tokio::{
    runtime::Runtime,
    select,
    task::{JoinError, JoinHandle},
};
use tokio_util::sync::CancellationToken;

/// A pausable worker which can be paused and restarted on forks.
///
/// Used to allow a [`ddcommon::worker::Worker`] to be paused while saving its state when dropping
/// a tokio runtime to be able to restart with the same state on a new runtime. This is used to
/// stop all threads before a fork to avoid deadlocks in child.
///
/// # Time-to-pause
/// This loop should yield regularly to reduce time-to-pause. See [`tokio::task::yield_now`].
///
/// # Cancellation safety
/// The main loop can be interrupted at any yield point (`.await`ed call). The state of the worker
/// at this point will be saved and used to restart the worker. To be able to safely restart, the
/// worker must be in a valid state on every call to `.await`.
/// See [`tokio::select#cancellation-safety`] for more details.
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

impl<T: Worker + Send + Sync + 'static> PausableWorker<T> {
    /// Create a new pausable worker from the given worker.
    pub fn new(worker: T) -> Self {
        Self::Paused { worker }
    }

    /// Start the worker on the given runtime.
    ///
    /// The worker's main loop will be run on the runtime.
    ///
    /// # Errors
    /// Fails if the worker is in an invalid state.
    pub fn start(&mut self, rt: &Runtime) -> Result<(), PausableWorkerError> {
        if let Self::Running { .. } = self {
            Ok(())
        } else if let Self::Paused { mut worker } = std::mem::replace(self, Self::InvalidState) {
            // Worker is temporarily in an invalid state, but since this block is failsafe it will
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
            Err(PausableWorkerError::InvalidState)
        }
    }

    /// Pause the worker saving it's state to be restarted.
    ///
    /// # Errors
    /// Fails if the worker handle has been aborted preventing the worker from being retrieved.
    pub async fn pause(&mut self) -> Result<(), PausableWorkerError> {
        match self {
            PausableWorker::Running { handle, stop_token } => {
                stop_token.cancel();
                if let Ok(worker) = handle.await {
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

    /// Wait for the run method of the worker to exit.
    pub async fn join(self) -> Result<(), JoinError> {
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
        // Empty the message queue and get the last message
        let mut next_message = 1;
        loop {
            if let Ok(message) = receiver.try_recv() {
                next_message = message + 1;
            } else {
                break;
            }
        }
        pausable_worker.start(&runtime).unwrap();
        assert_eq!(receiver.recv().unwrap(), next_message);
    }
}
