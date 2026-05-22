// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! SharedRuntime for managing [`PausableWorker`]s across fork boundaries.
//!
//! This module provides a SharedRuntime that manages a tokio runtime and allows
//! spawning PausableWorkers on it. It also provides hooks for safely handling
//! fork operations by pausing workers before fork and restarting them appropriately
//! in parent and child processes.

pub(crate) mod pausable_worker;

use crate::worker::Worker;
use futures::stream::{FuturesUnordered, StreamExt};
use libdd_common::MutexExt;
use pausable_worker::{PausableWorker, PausableWorkerError};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use std::{fmt, io};
use tracing::{debug, error};

/// Native-only runtime management, fork safety, and tokio integration.
///
/// Gated once here so individual items inside don't need `#[cfg]`.
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use pausable_worker::tokio_spawn_fn;
    use std::sync::atomic::Ordering;
    use tokio::runtime::{Builder, Handle, Runtime};

    fn build_runtime() -> Result<Runtime, io::Error> {
        Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
    }

    /// The tokio runtime that backs a [`SharedRuntime`].
    ///
    /// Two flavours coexist:
    /// - [`Self::Owned`]: a tokio runtime created and owned by the `SharedRuntime`. Used by FFI
    ///   callers and any code that constructs its own runtime up-front. Supports fork hooks, sync
    ///   `block_on`, sync `shutdown`.
    /// - [`Self::Borrowed`]: a handle to a tokio runtime owned by the caller (typically a Rust
    ///   application's host runtime). Used when the caller is itself an async tokio program and we
    ///   must integrate with their executor instead of spinning up our own. Borrowed mode does
    ///   **not** support fork hooks, sync `block_on`, or sync `shutdown` — callers should use the
    ///   async/condvar shutdown paths instead.
    #[derive(Debug, Clone)]
    pub(super) enum RuntimeBacking {
        Owned(Arc<Runtime>),
        Borrowed(Handle),
    }

    impl RuntimeBacking {
        pub(super) fn handle(&self) -> Handle {
            match self {
                Self::Owned(rt) => rt.handle().clone(),
                Self::Borrowed(h) => h.clone(),
            }
        }

        pub(super) fn is_borrowed(&self) -> bool {
            matches!(self, Self::Borrowed(_))
        }
    }

    impl SharedRuntime {
        pub(in super::super) fn new_native() -> Result<Self, SharedRuntimeError> {
            Ok(Self {
                runtime: Arc::new(Mutex::new(Some(RuntimeBacking::Owned(Arc::new(
                    build_runtime()?,
                ))))),
                workers: Arc::new(Mutex::new(Vec::new())),
                next_worker_id: AtomicU64::new(1),
                shutdown_tracker: Arc::new(ShutdownTracker::default()),
            })
        }

        /// Create a `SharedRuntime` that borrows an externally-owned tokio runtime via
        /// the given [`Handle`].
        ///
        /// The borrowed runtime is **not** owned by the `SharedRuntime`; dropping the
        /// `SharedRuntime` does not stop or drain it. Use this when the caller is itself
        /// an async tokio program (e.g. a Rust web service using `dd-trace-rs`) and you
        /// want libdatadog's workers to share the host runtime rather than spinning up
        /// a second tokio runtime in the same process.
        ///
        /// # Trade-offs vs. owned mode
        /// Borrowed mode does **not** support:
        /// - Fork safety: [`before_fork`](Self::before_fork),
        ///   [`after_fork_parent`](Self::after_fork_parent), and
        ///   [`after_fork_child`](Self::after_fork_child) all return
        ///   [`SharedRuntimeError::ForkUnsupportedInBorrowedMode`]. Rust applications rarely fork
        ///   after init; if you need fork-safety use owned mode.
        /// - Synchronous [`block_on`](Self::block_on): the caller is already inside a tokio runtime
        ///   and would deadlock; this method returns
        ///   [`SharedRuntimeError::BlockOnNotSupportedInBorrowedMode`].
        /// - Synchronous [`shutdown`](Self::shutdown): same reason — use
        ///   [`trigger_shutdown_signal`](Self::trigger_shutdown_signal) +
        ///   [`wait_shutdown_done`](Self::wait_shutdown_done) (sync) or
        ///   [`shutdown_async`](Self::shutdown_async) instead.
        pub fn from_handle(handle: Handle) -> Self {
            Self {
                runtime: Arc::new(Mutex::new(Some(RuntimeBacking::Borrowed(handle)))),
                workers: Arc::new(Mutex::new(Vec::new())),
                next_worker_id: AtomicU64::new(1),
                shutdown_tracker: Arc::new(ShutdownTracker::default()),
            }
        }

        /// Returns a clone of the tokio runtime handle managed by this SharedRuntime.
        ///
        /// Works for both owned and borrowed mode.
        ///
        /// # Errors
        /// Returns [`SharedRuntimeError::RuntimeUnavailable`] if the runtime has been shut
        /// down (owned mode only; borrowed mode hands out the externally-owned handle as
        /// long as this `SharedRuntime` itself hasn't been dropped).
        pub fn runtime_handle(&self) -> Result<Handle, SharedRuntimeError> {
            Ok(self
                .runtime
                .lock_or_panic()
                .as_ref()
                .ok_or(SharedRuntimeError::RuntimeUnavailable)?
                .handle())
        }

        /// Spawn a PausableWorker on this runtime.
        ///
        /// The worker will be tracked by this SharedRuntime and (in owned mode) will be
        /// paused/resumed during fork operations. In borrowed mode `restart_on_fork` is
        /// ignored because fork hooks are unsupported.
        /// If `restart_on_fork` is true, the worker will be reset and restarted when calling
        /// `after_fork_child` else the worker is dropped *without* calling `Worker::shutdown`.
        ///
        /// # Errors
        /// Returns an error if the worker cannot be started.
        pub fn spawn_worker<T: Worker + Sync + 'static>(
            &self,
            worker: T,
            restart_on_fork: bool,
        ) -> Result<WorkerHandle, SharedRuntimeError> {
            let boxed_worker: BoxedWorker = Box::new(worker);
            debug!(?boxed_worker, "Spawning worker on SharedRuntime");
            let mut pausable_worker = PausableWorker::new(boxed_worker);

            // Lock runtime first, then workers, following the documented mutex
            // lock order (matches before_fork). Both guards are held across
            // start+push so that before_fork cannot interleave between them:
            // otherwise before_fork could take the runtime, drop it, and miss
            // our (not-yet-pushed) worker, leaving us with a worker running on
            // a torn-down runtime that before_fork never paused. If the
            // runtime has been taken (fork window already passed), we skip
            // starting; after_fork_parent/child will start the worker on the
            // new runtime.
            let runtime_guard = self.runtime.lock_or_panic();
            let mut workers_guard = self.workers.lock_or_panic();

            if let Some(backing) = runtime_guard.as_ref() {
                let handle = backing.handle();
                if let Err(e) = pausable_worker.start(tokio_spawn_fn(&handle)) {
                    return Err(e.into());
                }
            }

            let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);

            workers_guard.push(WorkerEntry {
                id: worker_id,
                restart_on_fork,
                worker: pausable_worker,
            });

            Ok(WorkerHandle {
                worker_id,
                workers: self.workers.clone(),
            })
        }

        /// Hook to be called before forking.
        ///
        /// This method pauses all workers and prepares the runtime for forking.
        /// It ensures that no background tasks are running when the fork occurs,
        /// preventing potential deadlocks in the child process.
        ///
        /// Worker errors are logged but do not cause the function to fail.
        /// If the worker fails to pause it is dropped without calling shutdown.
        ///
        /// # Errors
        /// Returns [`SharedRuntimeError::ForkUnsupportedInBorrowedMode`] in borrowed mode.
        /// Rust applications rarely fork after init; if you need fork-safety use owned
        /// mode (i.e. construct the `SharedRuntime` via [`SharedRuntime::new`]).
        pub fn before_fork(&self) -> Result<(), SharedRuntimeError> {
            debug!("before_fork: pausing all workers");
            let runtime = {
                let mut runtime_lock = self.runtime.lock_or_panic();
                match runtime_lock.as_ref() {
                    Some(RuntimeBacking::Borrowed(_)) => {
                        return Err(SharedRuntimeError::ForkUnsupportedInBorrowedMode);
                    }
                    Some(RuntimeBacking::Owned(_)) => match runtime_lock.take() {
                        Some(RuntimeBacking::Owned(rt)) => rt,
                        _ => return Ok(()),
                    },
                    None => return Ok(()),
                }
            };
            let mut workers_lock = self.workers.lock_or_panic();
            runtime.block_on(async {
                let futures: FuturesUnordered<_> = workers_lock
                    .iter_mut()
                    .map(|worker_entry| async {
                        if let Err(e) = worker_entry.worker.pause().await {
                            error!("Worker failed to pause before fork: {:?}", e);
                        }
                    })
                    .collect();

                futures.collect::<()>().await;
            });
            Ok(())
        }

        fn restart_runtime(&self) -> Result<(), SharedRuntimeError> {
            let mut runtime_lock = self.runtime.lock_or_panic();
            match runtime_lock.as_ref() {
                Some(RuntimeBacking::Borrowed(_)) => {
                    Err(SharedRuntimeError::ForkUnsupportedInBorrowedMode)
                }
                Some(RuntimeBacking::Owned(_)) => Ok(()),
                None => {
                    *runtime_lock = Some(RuntimeBacking::Owned(Arc::new(build_runtime()?)));
                    Ok(())
                }
            }
        }

        /// Hook to be called in the parent process after forking.
        ///
        /// This method restarts workers and resumes normal operation in the parent process.
        /// The runtime may need to be recreated if it was shut down in before_fork.
        ///
        /// # Errors
        /// Returns [`SharedRuntimeError::ForkUnsupportedInBorrowedMode`] in borrowed mode,
        /// or another error if workers cannot be restarted or the runtime cannot be recreated.
        pub fn after_fork_parent(&self) -> Result<(), SharedRuntimeError> {
            debug!("after_fork_parent: restarting runtime and workers");
            self.restart_runtime()?;

            let handle = self.runtime_handle()?;

            let mut workers_lock = self.workers.lock_or_panic();

            for worker_entry in workers_lock.iter_mut() {
                worker_entry.worker.start(tokio_spawn_fn(&handle))?;
            }

            Ok(())
        }

        /// Hook to be called in the child process after forking.
        ///
        /// This method reinitializes the runtime and workers in the child process.
        /// A new runtime must be created since tokio runtimes cannot be safely forked.
        /// Workers are reset and restarted to resume operations in the child.
        ///
        /// # Errors
        /// Returns [`SharedRuntimeError::ForkUnsupportedInBorrowedMode`] in borrowed mode,
        /// or another error if the runtime cannot be reinitialized or workers cannot be started.
        pub fn after_fork_child(&self) -> Result<(), SharedRuntimeError> {
            debug!("after_fork_child: reinitializing runtime and workers");
            self.restart_runtime()?;

            let handle = self.runtime_handle()?;

            let mut workers_lock = self.workers.lock_or_panic();

            workers_lock.retain(|entry| entry.restart_on_fork);

            for worker_entry in workers_lock.iter_mut() {
                worker_entry.worker.reset();
                worker_entry.worker.start(tokio_spawn_fn(&handle))?;
            }

            Ok(())
        }

        /// Run a future to completion on the shared runtime, blocking the current thread.
        ///
        /// If the runtime is not available (e.g. after calling before_fork), a temporary
        /// single-threaded runtime is used.
        ///
        /// Not available on wasm32 — use async paths instead.
        ///
        /// # Errors
        /// Returns an [`io::Error`] with kind [`io::ErrorKind::Unsupported`] in borrowed
        /// mode: the caller is already inside their own tokio runtime and blocking on it
        /// would deadlock. Use an async API instead. Also returns an [`io::Error`] if
        /// the fallback single-threaded runtime cannot be created (owned mode,
        /// post-fork window).
        pub fn block_on<F: std::future::Future>(&self, f: F) -> Result<F::Output, io::Error> {
            let runtime = match self.runtime.lock_or_panic().as_ref() {
                Some(RuntimeBacking::Borrowed(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "SharedRuntime::block_on is not supported in borrowed mode; the \
                         caller is already inside a tokio runtime — use an async API instead",
                    ));
                }
                Some(RuntimeBacking::Owned(rt)) => rt.clone(),
                None => Arc::new(Builder::new_current_thread().enable_all().build()?),
            };
            Ok(runtime.block_on(f))
        }

        /// Shutdown the runtime and all workers synchronously with optional timeout.
        ///
        /// Not available on wasm32 — use [`shutdown_async`](Self::shutdown_async) instead.
        ///
        /// Worker errors are logged but do not cause the function to fail.
        ///
        /// # Errors
        /// Returns [`SharedRuntimeError::SyncShutdownNotSupportedInBorrowedMode`] in
        /// borrowed mode — sync callers there should use
        /// [`trigger_shutdown_signal`](Self::trigger_shutdown_signal) +
        /// [`wait_shutdown_done`](Self::wait_shutdown_done) instead.
        /// Returns [`SharedRuntimeError::ShutdownTimedOut`] if shutdown times out.
        pub fn shutdown(&self, timeout: Option<Duration>) -> Result<(), SharedRuntimeError> {
            debug!(?timeout, "Shutting down SharedRuntime");
            let runtime = {
                let mut runtime_lock = self.runtime.lock_or_panic();
                match runtime_lock.as_ref() {
                    Some(RuntimeBacking::Borrowed(_)) => {
                        return Err(SharedRuntimeError::SyncShutdownNotSupportedInBorrowedMode);
                    }
                    Some(RuntimeBacking::Owned(_)) => match runtime_lock.take() {
                        Some(RuntimeBacking::Owned(rt)) => rt,
                        _ => return Ok(()),
                    },
                    None => return Ok(()),
                }
            };
            if let Some(timeout) = timeout {
                match runtime
                    .block_on(async { tokio::time::timeout(timeout, self.shutdown_async()).await })
                {
                    Ok(()) => Ok(()),
                    Err(_) => Err(SharedRuntimeError::ShutdownTimedOut(timeout)),
                }
            } else {
                runtime.block_on(self.shutdown_async());
                Ok(())
            }
        }

        /// Whether this `SharedRuntime` is in borrowed mode (constructed via
        /// [`SharedRuntime::from_handle`]).
        /// Returns whether this runtime is borrowed (constructed from a host
        /// [`tokio::runtime::Handle`]) or owned.
        ///
        /// Poison-tolerant: if the internal mutex was poisoned by a previous panic the
        /// inner state is still readable and we honor it rather than propagating the
        /// poison; the alternative would be a panicky bool accessor in code paths that
        /// can't surface an error.
        pub fn is_borrowed(&self) -> bool {
            match self.runtime.lock() {
                Ok(guard) => guard.as_ref().is_some_and(|b| b.is_borrowed()),
                Err(poison) => poison
                    .into_inner()
                    .as_ref()
                    .is_some_and(|b| b.is_borrowed()),
            }
        }

        /// Initiate shutdown of every currently-registered worker **without blocking**.
        ///
        /// Pairs with [`wait_shutdown_done`](Self::wait_shutdown_done) to give sync code
        /// (e.g. `Drop` impls of higher-level exporters) a way to coordinate worker
        /// shutdown without calling `block_on` on the underlying tokio runtime — which is
        /// essential in borrowed mode where the caller is already a tokio worker thread.
        ///
        /// Concretely this:
        /// 1. Snapshots and removes every registered worker.
        /// 2. Records the snapshot's size as the "expected" completion count in the shutdown
        ///    tracker (idempotent: subsequent calls add to the expected count).
        /// 3. Spawns one tokio task per worker on the underlying runtime that pauses then shuts
        ///    down the worker and bumps the tracker on completion.
        ///
        /// Workers spawned *after* this call returns are not tracked; shutdown is
        /// considered terminal for the dd-trace-rs use case.
        ///
        /// # Errors
        /// Returns [`SharedRuntimeError::RuntimeUnavailable`] if the runtime has already
        /// been taken (e.g. after a previous sync `shutdown` in owned mode), or
        /// [`SharedRuntimeError::LockFailed`] if an internal mutex was poisoned by a
        /// previous panic.
        pub fn trigger_shutdown_signal(&self) -> Result<(), SharedRuntimeError> {
            let handle = self.runtime_handle()?;

            let workers = {
                let mut workers_lock = self.workers.lock().map_err(|e| {
                    SharedRuntimeError::LockFailed(format!("workers mutex poisoned: {e}"))
                })?;
                std::mem::take(&mut *workers_lock)
            };
            let count = workers.len();

            {
                let mut state = self.shutdown_tracker.state.lock().map_err(|e| {
                    SharedRuntimeError::LockFailed(format!(
                        "shutdown tracker state mutex poisoned: {e}"
                    ))
                })?;
                state.triggered = true;
                state.expected = state.expected.saturating_add(count);
            }

            // If no workers were registered, wake any pre-existing waiter so they don't
            // block forever expecting a notify.
            if count == 0 {
                self.shutdown_tracker.cv.notify_all();
                return Ok(());
            }

            for mut entry in workers {
                let tracker = self.shutdown_tracker.clone();
                handle.spawn(async move {
                    if let Err(e) = entry.worker.pause().await {
                        error!("Worker failed to pause on shutdown trigger: {:?}", e);
                    } else {
                        entry.worker.shutdown().await;
                    }
                    // Recover from a poisoned mutex rather than panic: panicking here
                    // would skip the counter bump and leave `wait_shutdown_done`
                    // waiting forever for a sibling task that may have already poisoned
                    // the lock. The inner counter is still readable through the
                    // returned guard.
                    let mut state = match tracker.state.lock() {
                        Ok(guard) => guard,
                        Err(poison) => {
                            error!(
                                "Shutdown tracker state mutex poisoned; counters may be inaccurate"
                            );
                            poison.into_inner()
                        }
                    };
                    state.completed = state.completed.saturating_add(1);
                    tracker.cv.notify_all();
                });
            }
            Ok(())
        }

        /// Block the calling thread until every worker triggered by
        /// [`trigger_shutdown_signal`](Self::trigger_shutdown_signal) has reported
        /// completion, or `timeout` elapses.
        ///
        /// Safe from any sync context — including from inside a tokio worker thread of
        /// the borrowed/host runtime — because it relies on a `Condvar` rather than
        /// `block_on`. Idempotent; returns immediately if shutdown has already completed.
        ///
        /// # Errors
        /// Returns [`SharedRuntimeError::ShutdownTimedOut`] if `timeout` elapses before
        /// all triggered workers complete.
        pub fn wait_shutdown_done(&self, timeout: Duration) -> Result<(), SharedRuntimeError> {
            // Tolerate poison on the initial lock by extracting the inner guard. The
            // counter state is plain data — there's no invariant a poisoned previous
            // holder could have broken — so it's safe to proceed and the result is
            // strictly better than panicking and leaking the caller's shutdown signal.
            let state = self
                .shutdown_tracker
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let (_state, res) = self
                .shutdown_tracker
                .cv
                .wait_timeout_while(state, timeout, |s| s.completed < s.expected)
                .unwrap_or_else(|err| err.into_inner());
            if res.timed_out() {
                Err(SharedRuntimeError::ShutdownTimedOut(timeout))
            } else {
                Ok(())
            }
        }
    }

    impl Drop for SharedRuntime {
        fn drop(&mut self) {
            // In borrowed mode we don't own the runtime so we cannot block on its
            // shutdown — and blocking on a host tokio worker would deadlock. Best effort:
            // signal cancellation to every still-registered worker (fire-and-forget) and
            // let the host runtime reap the tasks on its own teardown.
            //
            // In owned mode, leaving cleanup to `drop(Arc<Runtime>)` is fine: tokio will
            // drop the runtime and abort in-flight tasks. Callers that need graceful
            // shutdown should call `shutdown(...)` explicitly.
            //
            // Critical: a Drop impl must not panic. Every lock acquisition below is
            // poison-tolerant — if a sibling thread previously poisoned the mutex we
            // recover the inner value and degrade gracefully rather than risking a
            // double-panic and abort.
            let borrowed = match self.runtime.lock() {
                Ok(guard) => guard.as_ref().is_some_and(|b| b.is_borrowed()),
                Err(poison) => poison
                    .into_inner()
                    .as_ref()
                    .is_some_and(|b| b.is_borrowed()),
            };
            if !borrowed {
                return;
            }
            let workers = {
                let mut guard = match self.workers.lock() {
                    Ok(g) => g,
                    Err(poison) => poison.into_inner(),
                };
                std::mem::take(&mut *guard)
            };
            if workers.is_empty() {
                return;
            }
            let Ok(handle) = self.runtime_handle() else {
                return;
            };
            for mut entry in workers {
                handle.spawn(async move {
                    if let Err(e) = entry.worker.pause().await {
                        debug!("Worker failed to pause during borrowed-mode Drop: {:?}", e);
                    }
                });
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
use native::RuntimeBacking;

/// Tracks how many workers have completed shutdown after [`trigger_shutdown_signal`](
/// SharedRuntime::trigger_shutdown_signal).
///
/// Mirrors the `TraceBuffer::wait_shutdown_done` Condvar pattern so sync callers can wait
/// without `block_on`.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
struct ShutdownTracker {
    state: Mutex<ShutdownState>,
    cv: Condvar,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
struct ShutdownState {
    /// Total workers we're awaiting completion from. Bumped by `trigger_shutdown_signal`
    /// each time it runs (idempotent: shutdown is terminal so this only ever grows).
    expected: usize,
    /// Bumped by each per-worker shutdown task once `Worker::shutdown` returns.
    completed: usize,
    /// Set true by the first `trigger_shutdown_signal`. Currently informational; could be
    /// used in debug builds to assert that no further workers are spawned post-trigger.
    #[allow(dead_code)]
    triggered: bool,
}

type BoxedWorker = Box<dyn Worker + Sync>;

#[derive(Debug)]
struct WorkerEntry {
    id: u64,
    restart_on_fork: bool,
    worker: PausableWorker<BoxedWorker>,
}

/// Handle to a worker registered on a [`SharedRuntime`].
///
/// This handle can be used to stop the worker.
///
/// # Warning
/// If every clone of this handle is dropped without calling [`WorkerHandle::stop`], the worker
/// remains registered on the [`SharedRuntime`] and can only be torn down by shutting the
/// runtime down. Workers are expected to detect that their input channel has been closed and
/// park themselves to avoid spinning, but they will not be freed until the runtime stops.
#[must_use = "dropping a WorkerHandle without calling stop() leaks the worker until the SharedRuntime is shut down"]
#[derive(Clone, Debug)]
pub struct WorkerHandle {
    worker_id: u64,
    workers: Arc<Mutex<Vec<WorkerEntry>>>,
}

#[derive(Debug)]
pub enum WorkerHandleError {
    AlreadyStopped,
    WorkerError(PausableWorkerError),
}

impl fmt::Display for WorkerHandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyStopped => {
                write!(f, "Worker has already been stopped")
            }
            Self::WorkerError(err) => write!(f, "Worker error: {}", err),
        }
    }
}

impl std::error::Error for WorkerHandleError {}

impl From<PausableWorkerError> for WorkerHandleError {
    fn from(err: PausableWorkerError) -> Self {
        Self::WorkerError(err)
    }
}

impl WorkerHandle {
    /// Stop the worker and execute the shutdown logic.
    ///
    /// # Errors
    /// Returns an error if the worker has already been stopped.
    ///
    /// # Cancel safety
    /// This function is *NOT* cancel safe and shouldn't be called in [Worker::trigger].
    /// If cancelled, the stopped worker can end up in an invalid state if a fork occurs while
    /// stopping.
    pub async fn stop(self) -> Result<(), WorkerHandleError> {
        let mut worker = {
            let mut workers_lock = self.workers.lock_or_panic();
            let Some(position) = workers_lock
                .iter()
                .position(|entry| entry.id == self.worker_id)
            else {
                return Err(WorkerHandleError::AlreadyStopped);
            };
            let WorkerEntry { worker, .. } = workers_lock.swap_remove(position);
            worker
        };
        worker.pause().await?;
        worker.shutdown().await;
        Ok(())
    }
}

/// Errors that can occur when using SharedRuntime.
#[derive(Debug)]
pub enum SharedRuntimeError {
    /// The runtime is not available or in an invalid state.
    RuntimeUnavailable,
    /// Failed to acquire a lock on internal state.
    LockFailed(String),
    /// A worker operation failed.
    WorkerError(PausableWorkerError),
    /// Failed to create the tokio runtime.
    RuntimeCreation(io::Error),
    /// Shutdown timed out.
    ShutdownTimedOut(Duration),
    /// Fork hooks (`before_fork`/`after_fork_parent`/`after_fork_child`) were called on a
    /// `SharedRuntime` constructed via [`SharedRuntime::from_handle`]. Fork-safety is an
    /// owned-runtime-only feature; callers that need it must use [`SharedRuntime::new`].
    #[cfg(not(target_arch = "wasm32"))]
    ForkUnsupportedInBorrowedMode,
    /// Sync `SharedRuntime::shutdown` was called on a borrowed-mode runtime. Use
    /// [`SharedRuntime::trigger_shutdown_signal`] +
    /// [`SharedRuntime::wait_shutdown_done`] (sync) or
    /// [`SharedRuntime::shutdown_async`] (async) instead.
    #[cfg(not(target_arch = "wasm32"))]
    SyncShutdownNotSupportedInBorrowedMode,
}

impl fmt::Display for SharedRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeUnavailable => {
                write!(f, "Runtime is not available or in an invalid state")
            }
            Self::LockFailed(msg) => write!(f, "Failed to acquire lock: {}", msg),
            Self::WorkerError(err) => write!(f, "Worker error: {}", err),
            Self::RuntimeCreation(err) => {
                write!(f, "Failed to create runtime: {}", err)
            }
            Self::ShutdownTimedOut(duration) => {
                write!(f, "Shutdown timed out after {:?}", duration)
            }
            #[cfg(not(target_arch = "wasm32"))]
            Self::ForkUnsupportedInBorrowedMode => write!(
                f,
                "Fork hooks are not supported on a SharedRuntime created via from_handle; \
                 use SharedRuntime::new to opt back into owned mode + fork safety"
            ),
            #[cfg(not(target_arch = "wasm32"))]
            Self::SyncShutdownNotSupportedInBorrowedMode => write!(
                f,
                "Sync SharedRuntime::shutdown is not supported in borrowed mode; use \
                 trigger_shutdown_signal + wait_shutdown_done (sync) or shutdown_async \
                 (async) instead"
            ),
        }
    }
}

impl std::error::Error for SharedRuntimeError {}

impl From<PausableWorkerError> for SharedRuntimeError {
    fn from(err: PausableWorkerError) -> Self {
        SharedRuntimeError::WorkerError(err)
    }
}

impl From<io::Error> for SharedRuntimeError {
    fn from(err: io::Error) -> Self {
        SharedRuntimeError::RuntimeCreation(err)
    }
}

/// A shared runtime that manages PausableWorkers and provides fork safety hooks.
///
/// The SharedRuntime owns or borrows a tokio runtime (on native) and tracks
/// PausableWorkers spawned on it. It provides methods to safely pause workers before
/// forking and restart them after fork in both parent and child processes (owned mode
/// only).
///
/// On wasm32, no tokio runtime is created. Workers are spawned via `spawn_local`
/// on the JS event loop.
///
/// # Construction
/// - [`SharedRuntime::new`]: owned mode. The runtime is created here and owned by the
///   `SharedRuntime`. Supports fork safety, sync `block_on`, sync `shutdown`. Used by FFI callers
///   and test code.
/// - [`SharedRuntime::from_handle`]: borrowed mode. The tokio runtime is owned by the caller; the
///   `SharedRuntime` just shares its [`Handle`](tokio::runtime::Handle). Does **not** support fork
///   safety, sync `block_on`, or sync `shutdown`. Used by Rust apps where libdatadog should
///   integrate with the caller's host runtime (e.g. dd-trace-rs from a tokio-based web service).
///
/// # Mutex lock order
/// When locking both [Self::runtime] and [Self::workers], the mutex must be locked in the order of
/// the fields in the struct. When possible avoid holding both locks simultaneously.
#[derive(Debug)]
pub struct SharedRuntime {
    #[cfg(not(target_arch = "wasm32"))]
    runtime: Arc<Mutex<Option<RuntimeBacking>>>,
    workers: Arc<Mutex<Vec<WorkerEntry>>>,
    next_worker_id: AtomicU64,
    #[cfg(not(target_arch = "wasm32"))]
    shutdown_tracker: Arc<ShutdownTracker>,
}

impl SharedRuntime {
    /// Create a new SharedRuntime.
    ///
    /// On native, this creates a tokio multi-thread runtime. On wasm32, no runtime
    /// is created (workers are spawned on the JS event loop via `spawn_local`).
    ///
    /// # Errors
    /// Returns an error if the tokio runtime cannot be created (native only).
    pub fn new() -> Result<Self, SharedRuntimeError> {
        debug!("Creating new SharedRuntime");

        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::new_native()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Ok(Self {
                workers: Arc::new(Mutex::new(Vec::new())),
                next_worker_id: AtomicU64::new(1),
            })
        }
    }

    /// Spawn a PausableWorker on the JS event loop (wasm variant).
    #[cfg(target_arch = "wasm32")]
    pub fn spawn_worker<T: Worker + Sync + 'static>(
        &self,
        worker: T,
        restart_on_fork: bool,
    ) -> Result<WorkerHandle, SharedRuntimeError> {
        use std::sync::atomic::Ordering;

        let boxed_worker: BoxedWorker = Box::new(worker);
        debug!(?boxed_worker, "Spawning worker on SharedRuntime");
        let mut pausable_worker = PausableWorker::new(boxed_worker);

        let mut workers_guard = self.workers.lock_or_panic();

        if let Err(e) = pausable_worker.start(|future| {
            use futures_util::FutureExt;
            let (remote, handle) = future.remote_handle();
            wasm_bindgen_futures::spawn_local(remote);
            Box::pin(async { Ok(handle.await) })
        }) {
            return Err(e.into());
        }

        let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);

        workers_guard.push(WorkerEntry {
            id: worker_id,
            restart_on_fork,
            worker: pausable_worker,
        });

        Ok(WorkerHandle {
            worker_id,
            workers: self.workers.clone(),
        })
    }

    /// Shutdown all workers asynchronously.
    ///
    /// This should be called during application shutdown to cleanly stop all
    /// background workers and the runtime.
    ///
    /// Worker errors are logged but do not cause the function to fail.
    pub async fn shutdown_async(&self) {
        debug!("Shutting down all workers asynchronously");
        let workers = {
            let mut workers_lock = self.workers.lock_or_panic();
            std::mem::take(&mut *workers_lock)
        };

        let futures: FuturesUnordered<_> = workers
            .into_iter()
            .map(|mut worker_entry| async move {
                if let Err(e) = worker_entry.worker.pause().await {
                    error!("Worker failed to shutdown: {:?}", e);
                    return;
                }
                worker_entry.worker.shutdown().await;
            })
            .collect();

        futures.collect::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::time::Duration;
    use tokio::time::sleep;

    #[derive(Debug)]
    struct TestWorker {
        state: i32,
        sender: Sender<i32>,
    }

    fn make_test_worker() -> (TestWorker, Receiver<i32>) {
        let (sender, receiver) = channel::<i32>();
        (TestWorker { state: 0, sender }, receiver)
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

        fn reset(&mut self) {
            self.state = 0;
        }

        async fn shutdown(&mut self) {
            self.state = -1;
            let _ = self.sender.send(self.state);
        }
    }

    #[test]
    fn test_shared_runtime_creation() {
        let shared_runtime = SharedRuntime::new();
        assert!(shared_runtime.is_ok());
    }

    #[test]
    fn test_spawn_worker() {
        let shared_runtime = SharedRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let result = shared_runtime.spawn_worker(worker, true);
        assert!(result.is_ok());
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 1);

        // Verify the worker is actually running by receiving its first output
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run"),
            0
        );
    }

    #[test]
    fn test_worker_handle_stop() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let shared_runtime = SharedRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let handle = shared_runtime.spawn_worker(worker, true).unwrap();
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 1);

        // Wait for at least one run before stopping
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");

        rt.block_on(async {
            assert!(handle.stop().await.is_ok());
        });

        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);

        // Drain all messages after stop — the last one must be the shutdown sentinel
        let mut last = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown did not send a value");
        while let Ok(v) = receiver.try_recv() {
            last = v;
        }
        assert_eq!(last, -1);
    }

    #[test]
    fn test_before_and_after_fork_parent() {
        let shared_runtime = SharedRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let _ = shared_runtime.spawn_worker(worker, true).unwrap();

        // Let the worker run until state > 0 so that preservation is observable
        let mut state_before_fork = 0;
        while state_before_fork == 0 {
            state_before_fork = receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not advance state before fork");
        }

        shared_runtime.before_fork().unwrap();
        // Drain pre-fork buffered messages now that the worker is paused
        while receiver.try_recv().is_ok() {}

        assert!(shared_runtime.after_fork_parent().is_ok());

        // State must be preserved (not reset) after fork in the parent
        let after_fork_value = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not resume after fork");
        assert!(
            after_fork_value > state_before_fork,
            "after_fork_parent should preserve state: got {after_fork_value}, expected > {state_before_fork}"
        );
    }

    #[test]
    fn test_after_fork_child() {
        let shared_runtime = SharedRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let _ = shared_runtime.spawn_worker(worker, true).unwrap();

        // Let the worker run until state > 0 so that the reset is observable
        let mut state_before_fork = 0;
        while state_before_fork == 0 {
            state_before_fork = receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not advance state before fork");
        }

        shared_runtime.before_fork().unwrap();
        // Drain pre-fork buffered messages now that the worker is paused
        while receiver.try_recv().is_ok() {}

        assert!(shared_runtime.after_fork_child().is_ok());

        // State must be reset to 0 in the child
        let after_fork_value = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not resume after fork child");
        assert_eq!(
            after_fork_value, 0,
            "after_fork_child should reset state to 0, got {after_fork_value}"
        );
    }

    #[test]
    fn test_shutdown() {
        let shared_runtime = SharedRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let _ = shared_runtime.spawn_worker(worker, true).unwrap();

        // Wait for at least one run before shutting down
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");

        shared_runtime.shutdown(None).unwrap();

        // Drain all messages after shutdown — the last one must be the shutdown sentinel
        let mut last = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown did not send a value");
        while let Ok(v) = receiver.try_recv() {
            last = v;
        }
        assert_eq!(last, -1);
    }

    #[test]
    fn test_after_fork_child_drops_worker_not_restart_on_fork() {
        let shared_runtime = SharedRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let _ = shared_runtime.spawn_worker(worker, false).unwrap();

        // Wait for the worker to run at least once
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");

        shared_runtime.before_fork().unwrap();
        // Drain buffered messages now that the worker is paused
        while receiver.try_recv().is_ok() {}

        assert!(shared_runtime.after_fork_child().is_ok());

        // Worker must be removed from the list
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);

        // Worker must not produce any more messages (not restarted, not shut down)
        assert!(
            receiver.recv_timeout(Duration::from_millis(200)).is_err(),
            "worker should not run or shut down after fork in child when restart_on_fork is false"
        );
    }

    /// Smoke test for borrowed mode: workers spawned on a `SharedRuntime` created from a
    /// host runtime's `Handle` run on that host runtime and can be shut down via the
    /// Condvar-based [`SharedRuntime::wait_shutdown_done`] without `block_on`.
    #[test]
    fn test_from_handle_borrowed_shutdown_wait() {
        // Host tokio runtime — borrowed by the SharedRuntime.
        let host = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let shared_runtime = SharedRuntime::from_handle(host.handle().clone());
        assert!(shared_runtime.is_borrowed());

        let (worker, receiver) = make_test_worker();
        let _ = shared_runtime.spawn_worker(worker, true).unwrap();

        // Wait for the worker to advance at least once on the host runtime.
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run on host runtime");

        shared_runtime
            .trigger_shutdown_signal()
            .expect("trigger_shutdown_signal failed");
        shared_runtime
            .wait_shutdown_done(Duration::from_secs(5))
            .expect("shutdown did not complete in time");

        // Drain remaining messages — the last one must be the shutdown sentinel (-1).
        let mut last = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown sentinel was not produced");
        while let Ok(v) = receiver.try_recv() {
            last = v;
        }
        assert_eq!(last, -1);
    }

    /// Fork hooks and sync `block_on`/`shutdown` are unsupported in borrowed mode.
    #[test]
    fn test_borrowed_mode_unsupported_apis() {
        let host = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let shared_runtime = SharedRuntime::from_handle(host.handle().clone());

        assert!(matches!(
            shared_runtime.before_fork(),
            Err(SharedRuntimeError::ForkUnsupportedInBorrowedMode)
        ));
        assert!(matches!(
            shared_runtime.after_fork_parent(),
            Err(SharedRuntimeError::ForkUnsupportedInBorrowedMode)
        ));
        assert!(matches!(
            shared_runtime.after_fork_child(),
            Err(SharedRuntimeError::ForkUnsupportedInBorrowedMode)
        ));
        assert!(matches!(
            shared_runtime.shutdown(None),
            Err(SharedRuntimeError::SyncShutdownNotSupportedInBorrowedMode)
        ));

        let err = shared_runtime
            .block_on(async {})
            .expect_err("block_on should fail in borrowed mode");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    /// `wait_shutdown_done` returns `Ok` immediately when no workers were ever registered
    /// (expected == completed == 0).
    #[test]
    fn test_wait_shutdown_done_no_workers() {
        let shared_runtime = SharedRuntime::new().unwrap();
        shared_runtime.trigger_shutdown_signal().unwrap();
        shared_runtime
            .wait_shutdown_done(Duration::from_secs(1))
            .expect("wait_shutdown_done should return immediately with no workers");
    }
}
