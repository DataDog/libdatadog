// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Defines a pausable worker to be able to stop background processes before forks

use super::WorkerHandleError;
use crate::weak_waker::WeakWakerFuture;
use crate::worker::Worker;
use core::pin::Pin;
use futures_util::FutureExt;
use futures_util::future::Shared;
use libdd_capabilities::MaybeSend;
use libdd_capabilities::maybe_send::MaybeSync;
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
type WorkerJoinHandle<T> = Pin<Box<dyn Future<Output = Result<T, TaskJoinError>> + Send>>;
#[cfg(target_arch = "wasm32")]
type WorkerJoinHandle<T> = Pin<Box<dyn Future<Output = Result<T, TaskJoinError>>>>;

/// Cancels a running worker task for discard. Native uses tokio's abort handle; on wasm32,
/// dropping the task's remote handle cancels it, so no separate handle is needed.
#[cfg(not(target_arch = "wasm32"))]
pub(super) type TaskAbort = tokio::task::AbortHandle;
#[cfg(target_arch = "wasm32")]
pub(super) type TaskAbort = ();

/// Why a worker task ended without returning its state.
// Only the native tokio spawn closure constructs these; the wasm32 closure always succeeds.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Debug)]
pub(crate) enum TaskJoinError {
    /// The executor cancelled the task (discard or runtime shutdown).
    Cancelled,
    /// The task failed, e.g. it panicked.
    Failed,
}

/// Build the spawn closure used by [`PausableWorker::start`] on native, backed by
/// `tokio::runtime::Handle::spawn`. Maps tokio's `JoinError` into [`TaskJoinError`].
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn tokio_spawn_fn<T: Send + 'static>(
    handle: &tokio::runtime::Handle,
) -> impl FnOnce(WorkerFuture<T>) -> (WorkerJoinHandle<T>, TaskAbort) {
    let h = handle.clone();
    move |future| {
        let jh = h.spawn(future);
        let abort = jh.abort_handle();
        let join: WorkerJoinHandle<T> = Box::pin(async {
            jh.await.map_err(|e| {
                if e.is_cancelled() {
                    TaskJoinError::Cancelled
                } else {
                    TaskJoinError::Failed
                }
            })
        });
        (join, abort)
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

pub(crate) type DiscardFuture = Shared<WorkerFuture<Result<(), PausableWorkerError>>>;

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
        // `()` on wasm32, where discard cancels by dropping `handle` instead.
        #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
        abort: TaskAbort,
    },
    Paused {
        worker: T,
        sender: CommandSender,
        receiver: CommandReceiver,
    },
    Discarding {
        completion: DiscardFuture,
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
            Self::Discarding { .. } => write!(f, "PausableWorker::Discarding"),
            Self::InvalidState => write!(f, "PausableWorker::InvalidState"),
        }
    }
}

#[derive(Clone, Debug)]
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
        spawn_fn: impl FnOnce(
            WorkerFuture<TaskOutput<T>>,
        ) -> (WorkerJoinHandle<TaskOutput<T>>, TaskAbort),
    ) -> Result<(), PausableWorkerError> {
        match self {
            PausableWorker::Running { .. } | PausableWorker::Discarding { .. } => Ok(()),
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
                let (handle, abort) = spawn_fn(Box::pin(run_worker(worker, receiver)));
                *self = PausableWorker::Running {
                    handle,
                    sender,
                    pause_requested: false,
                    abort,
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
                ..
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
            PausableWorker::Discarding { completion } => return completion.clone().await,
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
    pub(crate) fn request_flush(&self) -> Result<FlushToken, PausableWorkerError> {
        let sender = match self {
            PausableWorker::Running { sender, .. } | PausableWorker::Paused { sender, .. } => {
                sender
            }
            PausableWorker::Discarding { .. } | PausableWorker::InvalidState => {
                return Err(PausableWorkerError::InvalidState);
            }
        };
        let (tx, rx) = oneshot::channel();
        sender
            .send(WorkerCommand::Flush(tx))
            .map_err(|_| PausableWorkerError::TaskAborted)?;
        Ok(FlushToken { receiver: rx })
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

    /// Cancel in-flight work and retain the completion future until the worker is dropped.
    ///
    /// Cancellation drops the entire worker, so run/flush state is never resumed. Keeping a
    /// clone here lets fork and shutdown await destruction even if the original caller stops
    /// waiting. A task that completed a pause before cancellation is dropped through its handle.
    pub(crate) fn request_discard(&mut self) -> DiscardFuture {
        let completion: WorkerFuture<Result<(), PausableWorkerError>> =
            match std::mem::replace(self, Self::InvalidState) {
                #[cfg(not(target_arch = "wasm32"))]
                Self::Running { handle, abort, .. } => {
                    // Tokio drops the task's future before the join handle resolves.
                    abort.abort();
                    Box::pin(async move {
                        match handle.await {
                            Ok(_) | Err(TaskJoinError::Cancelled) => Ok(()),
                            Err(TaskJoinError::Failed) => Err(PausableWorkerError::TaskAborted),
                        }
                    })
                }
                #[cfg(target_arch = "wasm32")]
                Self::Running { handle, sender, .. } => Box::pin(async move {
                    // Dropping the remote handle cancels the task on a later executor turn. The
                    // task owns the only command receiver, so `closed` resolves once the worker
                    // has been destroyed.
                    drop(handle);
                    sender.closed().await;
                    Ok(())
                }),
                Self::Discarding { completion } => {
                    *self = Self::Discarding {
                        completion: completion.clone(),
                    };
                    return completion;
                }
                // Drop the worker on first poll, after the caller releases the registry lock.
                Self::Paused {
                    worker,
                    sender,
                    receiver,
                } => Box::pin(async move {
                    drop((worker, sender, receiver));
                    Ok(())
                }),
                Self::InvalidState => Box::pin(async { Err(PausableWorkerError::InvalidState) }),
            };
        let completion = completion.shared();
        *self = Self::Discarding {
            completion: completion.clone(),
        };
        completion
    }

    pub(crate) fn is_discarding(&self) -> bool {
        matches!(self, Self::Discarding { .. })
    }

    /// True once a discard has finished destroying the worker.
    pub(crate) fn is_retired(&self) -> bool {
        matches!(self, Self::Discarding { completion } if completion.peek().is_some())
    }

    /// The retirement future of a worker that is being discarded.
    pub(crate) fn discard_completion(&self) -> Option<DiscardFuture> {
        match self {
            Self::Discarding { completion } => Some(completion.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    async fn discard(mut self) -> Result<(), PausableWorkerError> {
        self.request_discard().await
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

    /// Worker whose first `run` blocks until released, counting flushes and shutdowns.
    #[derive(Debug)]
    struct BlockingWorker {
        triggered: bool,
        run_started: Sender<()>,
        release_run: Arc<tokio::sync::Notify>,
        runs: Arc<AtomicU32>,
        flushes: Arc<AtomicU32>,
        shutdowns: Arc<AtomicU32>,
    }

    #[async_trait]
    impl Worker for BlockingWorker {
        async fn run(&mut self) {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let _ = self.run_started.send(());
            self.release_run.notified().await;
        }

        async fn trigger(&mut self) {
            if self.triggered {
                std::future::pending::<()>().await;
            }
            self.triggered = true;
        }

        async fn flush(&mut self) {
            self.flushes.fetch_add(1, Ordering::SeqCst);
        }

        async fn shutdown(&mut self) {
            self.shutdowns.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct BlockingSetup {
        worker: PausableWorker<Box<dyn Worker>>,
        run_started: Receiver<()>,
        runs: Arc<AtomicU32>,
        flushes: Arc<AtomicU32>,
        shutdowns: Arc<AtomicU32>,
    }

    fn blocking_setup() -> BlockingSetup {
        let (sender, run_started) = channel();
        let release_run = Arc::new(tokio::sync::Notify::new());
        let runs = Arc::new(AtomicU32::new(0));
        let flushes = Arc::new(AtomicU32::new(0));
        let shutdowns = Arc::new(AtomicU32::new(0));
        let worker = BlockingWorker {
            triggered: false,
            run_started: sender,
            release_run: release_run.clone(),
            runs: runs.clone(),
            flushes: flushes.clone(),
            shutdowns: shutdowns.clone(),
        };
        BlockingSetup {
            worker: PausableWorker::new(Box::new(worker)),
            run_started,
            runs,
            flushes,
            shutdowns,
        }
    }

    // Discard cancels a pending run without releasing it, flushing, or running shutdown.
    #[test]
    fn test_discard_skips_queued_flush_and_shutdown() {
        let mut setup = blocking_setup();
        let runtime = Builder::new_multi_thread().enable_time().build().unwrap();
        setup
            .worker
            .start(tokio_spawn_fn(runtime.handle()))
            .unwrap();
        setup
            .run_started
            .recv_timeout(Duration::from_secs(2))
            .expect("worker did not run");

        let token = setup.worker.request_flush().unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(1), setup.worker.discard())
                .await
                .expect("discard waited for the blocked run")
                .unwrap();
        });

        assert_eq!(setup.runs.load(Ordering::SeqCst), 1);
        assert_eq!(setup.flushes.load(Ordering::SeqCst), 0);
        assert_eq!(setup.shutdowns.load(Ordering::SeqCst), 0);
        assert!(matches!(
            runtime.block_on(token.wait()),
            Err(WorkerHandleError::FlushAborted)
        ));
    }

    // Discarding a paused worker drops it without flushing or shutting it down.
    #[test]
    fn test_discard_paused_worker() {
        let setup = blocking_setup();
        let runtime = Builder::new_current_thread().build().unwrap();
        let token = setup.worker.request_flush().unwrap();

        runtime.block_on(setup.worker.discard()).unwrap();

        assert_eq!(setup.runs.load(Ordering::SeqCst), 0);
        assert_eq!(setup.flushes.load(Ordering::SeqCst), 0);
        assert_eq!(setup.shutdowns.load(Ordering::SeqCst), 0);
        assert!(matches!(
            runtime.block_on(token.wait()),
            Err(WorkerHandleError::FlushAborted)
        ));
    }
    #[test]
    fn test_discard_cancels_in_flight_flush() {
        #[derive(Debug)]
        struct PendingFlush {
            started: Sender<()>,
            shutdowns: Arc<AtomicU32>,
        }
        #[async_trait]
        impl Worker for PendingFlush {
            async fn trigger(&mut self) {
                std::future::pending::<()>().await;
            }
            async fn run(&mut self) {}
            async fn flush(&mut self) {
                let _ = self.started.send(());
                std::future::pending::<()>().await;
            }
            async fn shutdown(&mut self) {
                self.shutdowns.fetch_add(1, Ordering::SeqCst);
            }
        }
        let runtime = Builder::new_multi_thread().enable_time().build().unwrap();
        let (started, receiver) = channel();
        let shutdowns = Arc::new(AtomicU32::new(0));
        let mut worker = PausableWorker::new(PendingFlush {
            started,
            shutdowns: shutdowns.clone(),
        });
        worker.start(tokio_spawn_fn(runtime.handle())).unwrap();
        let token = worker.request_flush().unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(1), worker.discard())
                .await
                .expect("discard waited for the blocked flush")
                .unwrap();
            assert!(matches!(
                token.wait().await,
                Err(WorkerHandleError::FlushAborted)
            ));
        });
        assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
    }
}
