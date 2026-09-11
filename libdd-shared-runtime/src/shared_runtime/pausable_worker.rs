// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Defines a pausable worker to be able to stop background processes before forks

use crate::weak_waker::WeakWakerFuture;
use crate::worker::Worker;
use core::pin::Pin;
use libdd_capabilities::spawn::SpawnError;
use libdd_capabilities::MaybeSend;
use std::fmt::Display;
use std::future::Future;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::debug;

#[cfg(not(target_arch = "wasm32"))]
type WorkerFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
#[cfg(target_arch = "wasm32")]
type WorkerFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

#[cfg(not(target_arch = "wasm32"))]
type WorkerJoinHandle<T> = Pin<Box<dyn Future<Output = Result<T, SpawnError>> + Send>>;
#[cfg(target_arch = "wasm32")]
type WorkerJoinHandle<T> = Pin<Box<dyn Future<Output = Result<T, SpawnError>>>>;

/// Build the spawn closure used by [`PausableWorker::start`] on native, backed by
/// `tokio::runtime::Handle::spawn`. Maps tokio's `JoinError` into the
/// executor-agnostic [`SpawnError`].
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn tokio_spawn_fn<T: Send + 'static>(
    handle: &tokio::runtime::Handle,
) -> impl FnOnce(WorkerFuture<T>) -> WorkerJoinHandle<T> {
    let h = handle.clone();
    move |future| {
        let jh = h.spawn(future);
        Box::pin(async { jh.await.map_err(|e| SpawnError::new(e.to_string())) })
    }
}

/// A pausable worker which can be paused and restarted on forks.
///
/// Used to allow a [`super::Worker`] to be paused while saving its state when
/// dropping a tokio runtime to be able to restart with the same state on a new runtime. This is
/// used to stop all threads before a fork to avoid deadlocks in child.
pub enum PausableWorker<T: Worker + MaybeSend + Sync + 'static> {
    Running {
        handle: WorkerJoinHandle<T>,
        /// Cancelled by [`Self::pause`]. Only guards `trigger`/`initial_trigger`, never
        /// `Worker::run`, so a normal pause (before a fork, or on ordinary shutdown) always lets
        /// an in-flight `run` finish.
        stop_token: CancellationToken,
        /// Cancelled by [`Self::discard`]. Unlike `stop_token`, this one also guards `Worker::run`
        /// itself, so a runtime identity refresh can cut off an in-flight run instead of waiting
        /// for it. Kept as a separate token so `discard`'s stronger cancellation never affects
        /// `pause`.
        discard_token: CancellationToken,
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

    /// Start the worker using the given spawn function.
    ///
    /// The worker's main loop will be spawned via the provided closure.
    /// `SharedRuntime` implementations construct the appropriate platform-specific
    /// closure (tokio on native, spawn_local on wasm).
    pub fn start(
        &mut self,
        spawn_fn: impl FnOnce(WorkerFuture<T>) -> WorkerJoinHandle<T>,
    ) -> Result<(), PausableWorkerError> {
        match self {
            PausableWorker::Running { .. } => Ok(()),
            PausableWorker::Paused { worker: _ } => {
                debug!(?self, "Starting pausable worker");
                let PausableWorker::Paused { mut worker } =
                    std::mem::replace(self, PausableWorker::InvalidState)
                else {
                    // Unreachable
                    return Ok(());
                };

                let stop_token = CancellationToken::new();
                // Runtime identity refresh needs a stronger cancellation path than normal pause:
                // it must discard in-flight work inherited from a process snapshot. Keep this
                // separate from `stop_token` so normal pause/fork behavior still waits for
                // `Worker::run` to finish.
                let discard_token = CancellationToken::new();
                let cloned_token = stop_token.clone();
                let cloned_discard_token = discard_token.clone();
                let future = Box::pin(async move {
                    // First iteration using initial_trigger.
                    //
                    // `trigger`/`initial_trigger` are wrapped with [`WeakWakerFuture::new`] so the
                    // waker handed out to the worker (and potentially shared with code
                    // outside of this runtime, e.g. the non-runtime end of a channel) does
                    // not keep the runtime scheduler alive after this task is dropped.
                    select! {
                        biased;
                        _ = cloned_discard_token.cancelled() => {
                            return worker;
                        }
                        _ = cloned_token.cancelled() => {
                            return worker;
                        }
                        _ = WeakWakerFuture::new(worker.initial_trigger()) => {
                        }
                    }
                    select! {
                        biased;
                        _ = cloned_discard_token.cancelled() => {
                            return worker;
                        }
                        _ = worker.run() => {}
                    }

                    // Regular iterations
                    loop {
                        select! {
                            biased;
                            _ = cloned_discard_token.cancelled() => {
                                break;
                            }
                            _ = cloned_token.cancelled() => {
                                break;
                            }
                            _ = WeakWakerFuture::new(worker.trigger()) => {
                            }
                        }
                        select! {
                            biased;
                            _ = cloned_discard_token.cancelled() => {
                                break;
                            }
                            _ = worker.run() => {}
                        }
                    }
                    worker
                });

                let handle = spawn_fn(future);

                *self = PausableWorker::Running {
                    handle,
                    stop_token,
                    discard_token,
                };
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
                let PausableWorker::Running {
                    handle, stop_token, ..
                } = std::mem::replace(self, PausableWorker::InvalidState)
                else {
                    // Unreachable
                    return Ok(());
                };

                if !stop_token.is_cancelled() {
                    stop_token.cancel();
                }

                if let Ok(worker) = handle.await {
                    debug!(?worker, "Worker paused successfully");
                    *self = PausableWorker::Paused { worker };
                    Ok(())
                } else {
                    *self = PausableWorker::InvalidState;
                    Err(PausableWorkerError::TaskAborted)
                }
            }
            PausableWorker::Paused { .. } => Ok(()),
            PausableWorker::InvalidState => Err(PausableWorkerError::InvalidState),
        }
    }

    /// Stop the worker for a runtime identity refresh and discard its inherited state, without
    /// running the ordinary shutdown flush path.
    ///
    /// Calls [`Worker::discard`] rather than [`Worker::reset`]: this worker instance is never run
    /// again, whereas `reset` is written for a worker that keeps running afterward (e.g. a forked
    /// child). Using `reset` here would incorrectly reopen resources (e.g. a channel) that a
    /// permanently discarded worker should instead close.
    pub(super) async fn discard(&mut self) -> Result<(), PausableWorkerError> {
        match self {
            PausableWorker::Running { .. } => {
                debug!("Waiting for worker to discard");
                let PausableWorker::Running {
                    handle,
                    discard_token,
                    ..
                } = std::mem::replace(self, PausableWorker::InvalidState)
                else {
                    // Unreachable
                    return Ok(());
                };

                if !discard_token.is_cancelled() {
                    discard_token.cancel();
                }

                if let Ok(mut worker) = handle.await {
                    worker.discard();
                    debug!(?worker, "Worker discarded successfully");
                    *self = PausableWorker::Paused { worker };
                    Ok(())
                } else {
                    *self = PausableWorker::InvalidState;
                    Err(PausableWorkerError::TaskAborted)
                }
            }
            PausableWorker::Paused { worker } => {
                worker.discard();
                Ok(())
            }
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

    /// A worker whose `run` can be held "in flight" for a controlled duration, used to test
    /// `pause`/`discard` racing against an active `run` call.
    ///
    /// `TestWorker` above can't exercise that race: its `run` returns immediately, so there is
    /// no window in which `pause`/`discard` can observe it as in-flight. Each lifecycle method
    /// reports itself on `sender` so a test can assert both whether `run` was allowed to finish
    /// and the order lifecycle methods ran in.
    #[derive(Debug)]
    struct TestInFlightWorker {
        sender: Sender<&'static str>,
        run_duration: Duration,
    }

    #[async_trait]
    impl Worker for TestInFlightWorker {
        async fn run(&mut self) {
            let _ = self.sender.send("run-started");
            sleep(self.run_duration).await;
            let _ = self.sender.send("run-finished");
        }

        async fn trigger(&mut self) {
            std::future::pending::<()>().await;
        }

        async fn initial_trigger(&mut self) {}

        fn reset(&mut self) {
            let _ = self.sender.send("reset");
        }

        async fn shutdown(&mut self) {
            let _ = self.sender.send("shutdown");
        }
    }

    #[test]
    fn test_restart() {
        let (sender, receiver) = channel::<u32>();
        let worker = TestWorker { state: 0, sender };
        let runtime = Builder::new_multi_thread().enable_time().build().unwrap();
        let handle = runtime.handle().clone();
        let mut pausable_worker: PausableWorker<Box<dyn Worker + Sync>> =
            PausableWorker::new(Box::new(worker));

        pausable_worker.start(tokio_spawn_fn(&handle)).unwrap();

        assert_eq!(receiver.recv().unwrap(), 0);
        runtime.block_on(async { pausable_worker.pause().await.unwrap() });
        // Empty the message queue and get the last message
        let mut next_message = 1;
        for message in receiver.try_iter() {
            next_message = message + 1;
        }
        pausable_worker.start(tokio_spawn_fn(&handle)).unwrap();
        assert_eq!(receiver.recv().unwrap(), next_message);
    }

    #[test]
    fn test_pause_waits_for_in_flight_run() {
        let (sender, receiver) = channel::<&'static str>();
        let worker = TestInFlightWorker {
            sender,
            run_duration: Duration::from_millis(100),
        };
        let runtime = Builder::new_multi_thread().enable_time().build().unwrap();
        let handle = runtime.handle().clone();
        let mut pausable_worker: PausableWorker<Box<dyn Worker + Sync>> =
            PausableWorker::new(Box::new(worker));

        pausable_worker.start(tokio_spawn_fn(&handle)).unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            "run-started"
        );

        runtime.block_on(async { pausable_worker.pause().await.unwrap() });

        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            "run-finished"
        );
        assert!(
            receiver.recv_timeout(Duration::from_millis(200)).is_err(),
            "pause must not reset or shutdown the worker"
        );
    }

    #[test]
    fn test_discard_cancels_in_flight_run_and_resets() {
        let (sender, receiver) = channel::<&'static str>();
        let worker = TestInFlightWorker {
            sender,
            run_duration: Duration::from_secs(60),
        };
        let runtime = Builder::new_multi_thread().enable_time().build().unwrap();
        let handle = runtime.handle().clone();
        let mut pausable_worker: PausableWorker<Box<dyn Worker + Sync>> =
            PausableWorker::new(Box::new(worker));

        pausable_worker.start(tokio_spawn_fn(&handle)).unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            "run-started"
        );

        runtime.block_on(async { pausable_worker.discard().await.unwrap() });

        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            "reset"
        );
        assert!(
            receiver.recv_timeout(Duration::from_millis(200)).is_err(),
            "discard must cancel the in-flight run and must not shutdown the worker"
        );
    }
}
