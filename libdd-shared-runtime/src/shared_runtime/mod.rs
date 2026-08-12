// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! [`SharedRuntime`] trait and its implementations: [`ForkSafeRuntime`], [`BasicRuntime`],
//! and [`LocalRuntime`].

pub(crate) mod pausable_worker;

#[cfg(not(target_arch = "wasm32"))]
mod basic;
#[cfg(not(target_arch = "wasm32"))]
mod fork_safe;
#[cfg(target_arch = "wasm32")]
mod local;

#[cfg(not(target_arch = "wasm32"))]
pub use basic::BasicRuntime;
#[cfg(not(target_arch = "wasm32"))]
pub use fork_safe::ForkSafeRuntime;
#[cfg(target_arch = "wasm32")]
pub use local::LocalRuntime;

use crate::worker::Worker;
use libdd_capabilities::MaybeSend;
use libdd_common::MutexExt;
use pausable_worker::{PausableWorker, PausableWorkerError};
use std::sync::{Arc, Mutex};
use std::{fmt, io};

/// A worker registered on a [`SharedRuntime`].
pub(crate) type BoxedWorker = Box<dyn Worker + Sync>;

#[derive(Debug)]
pub(crate) struct WorkerEntry {
    pub(crate) id: u64,
    pub(crate) restart_on_fork: bool,
    pub(crate) worker: PausableWorker<BoxedWorker>,
}

/// Common interface for all [`SharedRuntime`] implementations.
///
/// # Choosing an implementation
///
/// | Situation | Runtime |
/// |-----------|---------|
/// | Native, host process may call `fork(2)` | [`ForkSafeRuntime`] |
/// | Native, caller owns a tokio runtime and wants to share it | [`BasicRuntime`] |
/// | Wasm / single-threaded JS event loop | [`LocalRuntime`] |
///
/// Sync entry points (e.g. a blocking `build` or `send`) additionally require
/// `R: `[`BlockingRuntime`], which only the native implementations satisfy.
pub trait SharedRuntime {
    /// Creates a new instance of the runtime with default configuration.
    ///
    /// Used as a fallback by callers (e.g. [`crate::shared_runtime`] consumers) that want to
    /// auto-construct a runtime when one was not supplied; concrete runtimes additionally
    /// expose richer inherent constructors (e.g. `with_worker_threads`, `from_handle`).
    fn new() -> Result<Self, SharedRuntimeError>
    where
        Self: Sized;

    /// Spawns a worker. `restart_on_fork = true` causes `ForkSafeRuntime::after_fork_child`
    /// to reset and restart it; `false` drops it without calling shutdown. [`BasicRuntime`]
    /// and [`LocalRuntime`] ignore this flag — they do not implement a fork protocol.
    fn spawn_worker<T: Worker + Sync + 'static>(
        &self,
        worker: T,
        restart_on_fork: bool,
    ) -> Result<WorkerHandle, SharedRuntimeError>;

    /// Shuts down all tracked workers. The runtime itself is not torn down — call
    /// [`ForkSafeRuntime::shutdown`] (native only) to also drop the tokio runtime.
    fn shutdown_async(&self) -> impl std::future::Future<Output = ()> + MaybeSend + '_
    where
        Self: Sync;
}

/// Error returned by [`BlockingRuntime::block_on_with_timeout`].
#[derive(Debug)]
pub enum BlockOnTimeoutError {
    /// The executor could not be accessed or constructed.
    Io(io::Error),
    /// The deadline elapsed before the future completed.
    TimedOut(std::time::Duration),
}

impl fmt::Display for BlockOnTimeoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "Executor error: {}", err),
            Self::TimedOut(duration) => write!(f, "Timed out after {:?}", duration),
        }
    }
}

impl std::error::Error for BlockOnTimeoutError {}

impl From<io::Error> for BlockOnTimeoutError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Returns true if `payload` is the panic that Tokio raises when it polls a timer on a
/// runtime built without `enable_time`.
///
/// Tokio has no fallible constructor for this case. [`BlockingRuntime::block_on_with_timeout`]
/// matches the panic message to convert the panic into a [`BlockOnTimeoutError`] instead of
/// letting the panic abort the process.
#[cfg(not(target_arch = "wasm32"))]
fn is_timers_disabled_panic(payload: &(dyn std::any::Any + Send)) -> bool {
    // Tokio runs the timer driver as an internal task and re-raises that task's panic
    // through `resume_unwind` on a boxed `JoinError` payload. The `JoinError` payload nests
    // one `Box<dyn Any + Send>` inside the outer payload caught here, so this function
    // unwraps the inner payload before matching on the message.
    if let Some(inner) = payload.downcast_ref::<Box<dyn std::any::Any + Send>>() {
        return is_timers_disabled_panic(inner.as_ref());
    }
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    matches!(message, Some(message) if message.contains("timers are disabled"))
}

/// Extension of [`SharedRuntime`] for runtimes that can block the current thread on a future.
#[cfg(not(target_arch = "wasm32"))]
pub trait BlockingRuntime: SharedRuntime {
    /// Drives `f` to completion, blocking the current thread.
    ///
    /// Returns an [`io::Error`] if the executor cannot be accessed or constructed.
    fn block_on<F: std::future::Future>(&self, f: F) -> Result<F::Output, io::Error>;

    /// Drives `f` to completion, blocking the current thread, but gives up once `timeout`
    /// elapses.
    ///
    /// This method drives the deadline on the same executor as [`block_on`](Self::block_on).
    ///
    /// The bound is cooperative: the executor can only check the deadline when `f` yields.
    /// A future that never yields, or that blocks the thread outside of `.await`, can run
    /// past `timeout` before this method returns.
    ///
    /// This method requires a build with unwinding panics. A `panic = "abort"` build cannot
    /// catch the panic that a timerless runtime raises (see below), and aborts the process
    /// instead of returning [`BlockOnTimeoutError::Io`].
    fn block_on_with_timeout<F: std::future::Future>(
        &self,
        f: F,
        timeout: std::time::Duration,
    ) -> Result<F::Output, BlockOnTimeoutError> {
        // When the executor has no timer driver (e.g. a caller-supplied runtime built without
        // `enable_time`), tokio::time::timeout panics instead of returning an error. The
        // default block_on implementations are tokio-backed, so the panic reaches this
        // catch_unwind. Catching the panic here converts it to an error, so a misconfigured
        // executor cannot abort a process that calls block_on_with_timeout across the FFI
        // boundary.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.block_on(async move { tokio::time::timeout(timeout, f).await })
        }));
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(payload) if is_timers_disabled_panic(&payload) => {
                return Err(BlockOnTimeoutError::Io(io::Error::other(
                    "block_on_with_timeout requires a runtime with timers enabled",
                )));
            }
            Err(payload) => std::panic::resume_unwind(payload),
        };
        outcome?.map_err(|_| BlockOnTimeoutError::TimedOut(timeout))
    }
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
    pub(crate) worker_id: u64,
    pub(crate) workers: Arc<Mutex<Vec<WorkerEntry>>>,
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

/// Errors that can occur when using a `SharedRuntime` implementation.
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
    ShutdownTimedOut(std::time::Duration),
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
