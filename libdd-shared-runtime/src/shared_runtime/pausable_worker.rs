// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Defines a pausable worker to be able to stop background processes before forks

use super::WorkerHandleError;
use crate::weak_waker::WeakWakerFuture;
use crate::worker::Worker;
use core::pin::Pin;
use libdd_capabilities::MaybeSend;
use libdd_capabilities::maybe_send::MaybeSync;
use libdd_capabilities::spawn::SpawnError;
use std::fmt::Display;
use std::future::Future;
use tokio::select;
use tokio::sync::{mpsc, oneshot};
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

/// Command sent to the task running a [`PausableWorker`].
#[derive(Debug)]
pub(crate) enum WorkerCommand {
    /// Stop the running task. Exactly one is sent per run and consumed by the task.
    Pause,
    /// Flush the worker and acknowledge on the sender once done.
    Flush(oneshot::Sender<()>),
}

type CommandSender = mpsc::UnboundedSender<WorkerCommand>;
type CommandReceiver = mpsc::UnboundedReceiver<WorkerCommand>;

/// Output of the worker task: the worker state and the command receiver.
pub(crate) type TaskOutput<T> = (T, CommandReceiver);

/// A pausable worker which can be paused and restarted on forks.
///
/// Used to allow a [`super::Worker`] to be paused while saving its state when
/// dropping a tokio runtime to be able to restart with the same state on a new runtime. This is
/// used to stop all threads before a fork to avoid deadlocks in child.
///
/// The worker task is controlled through a command channel which is kept across pauses. Commands
/// are processed in FIFO order.
pub enum PausableWorker<T: Worker + MaybeSend + MaybeSync + 'static> {
    Running {
        handle: WorkerJoinHandle<TaskOutput<T>>,
        sender: CommandSender,
        pause_requested: bool,
    },
    Paused {
        worker: T,
        sender: CommandSender,
        receiver: CommandReceiver,
    },
    InvalidState,
}

impl<T: Worker + MaybeSend + MaybeSync + 'static> std::fmt::Debug for PausableWorker<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running {
                pause_requested, ..
            } => f
                .debug_struct("PausableWorker::Running")
                .field("pause_requested", pause_requested)
                .finish(),
            Self::Paused { worker, .. } => f
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

/// Main loop of the worker task.
///
/// `trigger`/`initial_trigger` and `recv` are wrapped with [`WeakWakerFuture::new`] so the
/// waker handed out (and potentially shared with code outside of this runtime, e.g. the
/// command sender) does not keep the runtime scheduler alive after this task is dropped.
async fn run_worker<T: Worker>(mut worker: T, mut receiver: CommandReceiver) -> TaskOutput<T> {
    let mut first = true;
    loop {
        select! {
            biased;
            cmd = WeakWakerFuture::new(receiver.recv()) => match cmd {
                Some(WorkerCommand::Pause) | None => break,
                Some(WorkerCommand::Flush(ack)) => {
                    worker.flush().await;
                    let _ = ack.send(());
                }
            },
            _ = WeakWakerFuture::new(async {
                if first {
                    worker.initial_trigger().await
                } else {
                    worker.trigger().await
                }
            }) => {
                worker.run().await;
                first = false;
            }
        }
    }
    (worker, receiver)
}

impl<T: Worker + MaybeSend + MaybeSync + 'static> PausableWorker<T> {
    /// Create a new pausable worker from the given worker.
    pub fn new(worker: T) -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self::Paused {
            worker,
            sender,
            receiver,
        }
    }

    /// Start the worker using the given spawn function.
    ///
    /// The worker's main loop will be spawned via the provided closure.
    /// `SharedRuntime` implementations construct the appropriate platform-specific
    /// closure (tokio on native, spawn_local on wasm).
    pub fn start(
        &mut self,
        spawn_fn: impl FnOnce(WorkerFuture<TaskOutput<T>>) -> WorkerJoinHandle<TaskOutput<T>>,
    ) -> Result<(), PausableWorkerError> {
        match self {
            PausableWorker::Running { .. } => Ok(()),
            PausableWorker::Paused { .. } => {
                debug!(?self, "Starting pausable worker");
                let PausableWorker::Paused {
                    worker,
                    sender,
                    receiver,
                } = std::mem::replace(self, PausableWorker::InvalidState)
                else {
                    // Unreachable
                    return Ok(());
                };
                let handle = spawn_fn(Box::pin(run_worker(worker, receiver)));
                *self = PausableWorker::Running {
                    handle,
                    sender,
                    pause_requested: false,
                };
                Ok(())
            }
            PausableWorker::InvalidState => Err(PausableWorkerError::InvalidState),
        }
    }

    /// Pause the worker and wait for it to complete, storing its state for restart.
    ///
    /// Commands are processed in FIFO order: a flush requested before the pause runs before the
    /// worker stops. Commands requested after the pause stay queued and are processed after the
    /// next [`start`](Self::start). Exactly one pause command is sent per run, so pausing again
    /// (including after a cancelled `pause`) never queues a second one. Pausing an already
    /// paused worker is a no-op.
    ///
    /// # Errors
    /// Fails if the worker handle has been aborted preventing the worker from being retrieved.
    pub async fn pause(&mut self) -> Result<(), PausableWorkerError> {
        let result = match self {
            PausableWorker::Running {
                handle,
                sender,
                pause_requested,
            } => {
                debug!("Waiting for worker to pause");
                if !*pause_requested {
                    // If sending fails the task is gone; the handle result decides the outcome.
                    let _ = sender.send(WorkerCommand::Pause);
                    *pause_requested = true;
                }
                handle.await
            }
            PausableWorker::Paused { .. } => return Ok(()),
            PausableWorker::InvalidState => return Err(PausableWorkerError::InvalidState),
        };
        let PausableWorker::Running { sender, .. } =
            std::mem::replace(self, PausableWorker::InvalidState)
        else {
            // Unreachable
            return Err(PausableWorkerError::InvalidState);
        };
        match result {
            Ok((worker, receiver)) => {
                debug!(?worker, "Worker paused successfully");
                *self = PausableWorker::Paused {
                    worker,
                    sender,
                    receiver,
                };
                Ok(())
            }
            Err(_) => Err(PausableWorkerError::TaskAborted),
        }
    }

    /// Request a flush of the worker.
    ///
    /// The flush is processed asynchronously by the worker task; when the worker is paused the
    /// flush is processed after the next [`start`](Self::start).
    ///
    /// # Errors
    /// Fails if the worker is in an invalid state or the task has been aborted.
    #[cfg(test)]
    pub(crate) fn request_flush(&self) -> Result<FlushToken, PausableWorkerError> {
        self.flush_requester()
            .ok_or(PausableWorkerError::InvalidState)?
            .request_flush()
    }

    /// Returns a requester that sends flush requests to this worker without access to it.
    ///
    /// The command channel lives as long as the worker, across pauses and restarts.
    pub(crate) fn flush_requester(&self) -> Option<FlushRequester> {
        match self {
            PausableWorker::Running { sender, .. } | PausableWorker::Paused { sender, .. } => {
                Some(FlushRequester {
                    sender: sender.clone(),
                })
            }
            PausableWorker::InvalidState => None,
        }
    }

    /// Reset the worker state (e.g. in a fork child).
    pub fn reset(&mut self) {
        if let PausableWorker::Paused { worker, .. } = self {
            worker.reset();
        }
    }

    /// Shutdown the worker.
    ///
    /// Pending flush requests are acknowledged after the worker's shutdown, which flushes by
    /// default.
    pub async fn shutdown(&mut self) {
        if let PausableWorker::Paused {
            worker, receiver, ..
        } = self
        {
            worker.shutdown().await;
            receiver.close();
            while let Ok(cmd) = receiver.try_recv() {
                if let WorkerCommand::Flush(ack) = cmd {
                    let _ = ack.send(());
                }
            }
        }
    }

    /// Returns true if a pause command is queued in the paused receiver.
    #[cfg(test)]
    fn has_queued_pause(&mut self) -> bool {
        let PausableWorker::Paused {
            receiver, sender, ..
        } = self
        else {
            return false;
        };
        let mut pause = false;
        let mut others = Vec::new();
        while let Ok(cmd) = receiver.try_recv() {
            match cmd {
                WorkerCommand::Pause => pause = true,
                other => others.push(other),
            }
        }
        for cmd in others {
            let _ = sender.send(cmd);
        }
        pause
    }
}

/// Sends flush requests to a worker task without going through the worker registry.
#[derive(Clone, Debug)]
pub(crate) struct FlushRequester {
    sender: CommandSender,
}

impl FlushRequester {
    /// Queue a flush request. Fails once the worker has been shut down or dropped.
    pub(crate) fn request_flush(&self) -> Result<FlushToken, PausableWorkerError> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(WorkerCommand::Flush(tx))
            .map_err(|_| PausableWorkerError::TaskAborted)?;
        Ok(FlushToken { receiver: rx })
    }
}

/// Token returned by a flush request, used to wait for the flush to complete.
///
/// If the worker is shut down before processing the request, the token is marked as completed:
/// [`Worker::shutdown`] counts as a flush, so queued flush requests are acknowledged on shutdown.
#[must_use = "a FlushToken should be awaited with wait() to observe flush completion"]
#[derive(Debug)]
pub struct FlushToken {
    receiver: oneshot::Receiver<()>,
}

impl FlushToken {
    /// Wait for the flush to complete.
    ///
    /// # Errors
    /// Returns [`WorkerHandleError::FlushAborted`] if the worker was dropped before flushing.
    pub async fn wait(self) -> Result<(), WorkerHandleError> {
        self.receiver
            .await
            .map_err(|_| WorkerHandleError::FlushAborted)
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tokio::{runtime::Builder, time::sleep};

    use super::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU32, Ordering},
            mpsc::{Receiver, Sender, channel},
        },
        time::Duration,
    };

    /// Test worker incrementing the state and sending it with the sender.
    #[derive(Debug)]
    struct TestWorker {
        state: u32,
        sender: Sender<u32>,
        flushes: Arc<AtomicU32>,
    }

    #[async_trait]
    impl Worker for TestWorker {
        async fn run(&mut self) {
            let _ = self.sender.send(self.state);
            self.state += 1;
        }

        async fn trigger(&mut self) {
            sleep(Duration::from_millis(20)).await;
        }

        async fn flush(&mut self) {
            self.flushes.fetch_add(1, Ordering::SeqCst);
        }
    }

    type Setup = (
        PausableWorker<Box<dyn Worker>>,
        Receiver<u32>,
        Arc<AtomicU32>,
        tokio::runtime::Runtime,
    );

    fn setup() -> Setup {
        let (sender, receiver) = channel::<u32>();
        let flushes = Arc::new(AtomicU32::new(0));
        let worker = TestWorker {
            state: 0,
            sender,
            flushes: flushes.clone(),
        };
        let runtime = Builder::new_multi_thread().enable_time().build().unwrap();
        (
            PausableWorker::new(Box::new(worker)),
            receiver,
            flushes,
            runtime,
        )
    }

    /// Asserts the worker keeps producing consecutive values after `start`.
    fn assert_keeps_running(receiver: &Receiver<u32>) {
        let mut last = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        for _ in 0..3 {
            let next = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(next, last + 1);
            last = next;
        }
    }

    fn drain(receiver: &Receiver<u32>) {
        for _ in receiver.try_iter() {}
    }

    #[test]
    fn test_restart() {
        let (mut pw, receiver, _, runtime) = setup();
        let handle = runtime.handle().clone();
        pw.start(tokio_spawn_fn(&handle)).unwrap();

        assert_eq!(receiver.recv().unwrap(), 0);
        runtime.block_on(async { pw.pause().await.unwrap() });
        // Empty the message queue and get the last message
        let mut next_message = 1;
        for message in receiver.try_iter() {
            next_message = message + 1;
        }
        pw.start(tokio_spawn_fn(&handle)).unwrap();
        assert_eq!(receiver.recv().unwrap(), next_message);
    }

    // A paused then restarted worker keeps running and is not re-paused by a leftover command.
    #[test]
    fn test_pause_start_keeps_running() {
        let (mut pw, receiver, _, runtime) = setup();
        let handle = runtime.handle().clone();
        pw.start(tokio_spawn_fn(&handle)).unwrap();
        receiver.recv().unwrap();
        runtime.block_on(pw.pause()).unwrap();
        assert!(!pw.has_queued_pause());
        drain(&receiver);
        pw.start(tokio_spawn_fn(&handle)).unwrap();
        assert_keeps_running(&receiver);
    }

    // Pausing twice sends a single pause, so the worker keeps running after restart.
    #[test]
    fn test_double_pause() {
        let (mut pw, receiver, _, runtime) = setup();
        let handle = runtime.handle().clone();
        pw.start(tokio_spawn_fn(&handle)).unwrap();
        receiver.recv().unwrap();
        runtime.block_on(pw.pause()).unwrap();
        runtime.block_on(pw.pause()).unwrap();
        assert!(!pw.has_queued_pause());
        drain(&receiver);
        pw.start(tokio_spawn_fn(&handle)).unwrap();
        assert_keeps_running(&receiver);
        runtime.block_on(pw.pause()).unwrap();
        assert!(matches!(pw, PausableWorker::Paused { .. }));
    }

    // A flush requested on a running worker calls `flush` once and resolves its token.
    #[test]
    fn test_flush_while_running() {
        let (mut pw, _receiver, flushes, runtime) = setup();
        pw.start(tokio_spawn_fn(runtime.handle())).unwrap();
        let token = pw.request_flush().unwrap();
        runtime.block_on(token.wait()).unwrap();
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
    }

    // A flush requested while paused runs only after restart and the worker state is preserved.
    #[test]
    fn test_flush_while_paused() {
        let (mut pw, receiver, flushes, runtime) = setup();
        let handle = runtime.handle().clone();
        pw.start(tokio_spawn_fn(&handle)).unwrap();
        receiver.recv().unwrap();
        runtime.block_on(pw.pause()).unwrap();
        let mut next = 1;
        for m in receiver.try_iter() {
            next = m + 1;
        }
        let token = pw.request_flush().unwrap();
        let wait = runtime.spawn(token.wait());
        std::thread::sleep(Duration::from_millis(50));
        assert!(!wait.is_finished());
        assert_eq!(flushes.load(Ordering::SeqCst), 0);
        pw.start(tokio_spawn_fn(&handle)).unwrap();
        runtime.block_on(wait).unwrap().unwrap();
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
        // State preserved across pause
        assert_eq!(receiver.recv().unwrap(), next);
    }

    // Shutdown flushes once and resolves every queued flush token successfully.
    #[test]
    fn test_shutdown_acks_queued_flushes() {
        let (mut pw, _receiver, flushes, runtime) = setup();
        let tokens: Vec<_> = (0..3).map(|_| pw.request_flush().unwrap()).collect();
        runtime.block_on(async {
            pw.shutdown().await;
            for token in tokens {
                token.wait().await.unwrap();
            }
        });
        // Default shutdown flushes once; queued requests are acked by that flush.
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
    }

    // Dropping a worker without shutdown makes pending flush tokens return `FlushAborted`.
    #[test]
    fn test_drop_aborts_flush() {
        let (pw, _receiver, _, runtime) = setup();
        let token = pw.request_flush().unwrap();
        drop(pw);
        assert!(matches!(
            runtime.block_on(token.wait()),
            Err(WorkerHandleError::FlushAborted)
        ));
    }

    // Requesting a flush on a worker in an invalid state returns an error.
    #[test]
    fn test_request_flush_invalid_state() {
        let pw: PausableWorker<Box<dyn Worker>> = PausableWorker::InvalidState;
        assert!(matches!(
            pw.request_flush(),
            Err(PausableWorkerError::InvalidState)
        ));
    }
}
