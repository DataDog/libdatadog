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
use std::sync::{Arc, Mutex};
use std::{fmt, io};
use tracing::{debug, error};

/// Native-only runtime management, fork safety, and tokio integration.
///
/// Gated once here so individual items inside don't need `#[cfg]`.
#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use libdd_capabilities::spawn::SpawnCapability;
    use libdd_capabilities_impl::NativeSpawnCapability;
    use std::sync::atomic::Ordering;
    use tokio::runtime::{Builder, Runtime};

    fn build_runtime() -> Result<Runtime, io::Error> {
        Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
    }

    impl SharedRuntime {
        pub(in super::super) fn new_native() -> Result<Self, SharedRuntimeError> {
            Ok(Self {
                runtime: Arc::new(Mutex::new(Some(Arc::new(build_runtime()?)))),
                workers: Arc::new(Mutex::new(Vec::new())),
                next_worker_id: AtomicU64::new(1),
            })
        }

        /// Returns a clone of the tokio runtime handle managed by this SharedRuntime.
        ///
        /// # Errors
        /// Returns [`SharedRuntimeError::RuntimeUnavailable`] if the runtime has been shut down.
        pub fn runtime_handle(&self) -> Result<tokio::runtime::Handle, SharedRuntimeError> {
            Ok(self
                .runtime
                .lock_or_panic()
                .as_ref()
                .ok_or(SharedRuntimeError::RuntimeUnavailable)?
                .handle()
                .clone())
        }

        /// Spawn a PausableWorker using the provided spawn capability.
        ///
        /// The worker will be tracked by this SharedRuntime and will be paused/resumed
        /// during fork operations (native only).
        /// If `restart_on_fork` is true, the worker will be reset and restarted when calling
        /// `after_fork_child` else the worker is dropped *without* calling `Worker::shutdown`.
        ///
        /// # Errors
        /// Returns an error if the worker cannot be started.
        pub fn spawn_worker<
            T: Worker + Sync + 'static,
            S: SpawnCapability<RuntimeContext = tokio::runtime::Handle> + 'static,
        >(
            &self,
            worker: T,
            restart_on_fork: bool,
            spawner: &S,
        ) -> Result<WorkerHandle, SharedRuntimeError> {
            let boxed_worker: BoxedWorker = Box::new(worker);
            debug!(?boxed_worker, "Spawning worker on SharedRuntime");
            let mut pausable_worker = PausableWorker::new(boxed_worker);

            // Lock runtime first to synchronize with before_fork (which does
            // runtime.take() then workers.lock()), following the documented mutex
            // lock order. If the runtime has been taken (fork window), skip starting
            // the worker, after_fork_parent/child will start it on the new runtime.
            let runtime_handle = self
                .runtime
                .lock_or_panic()
                .as_ref()
                .map(|rt| rt.handle().clone());
            let mut workers_guard = self.workers.lock_or_panic();

            if let Some(ref handle) = runtime_handle {
                if let Err(e) = pausable_worker.start(spawner, handle) {
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
        pub fn before_fork(&self) {
            debug!("before_fork: pausing all workers");
            if let Some(runtime) = self.runtime.lock_or_panic().take() {
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
            }
        }

        fn restart_runtime(&self) -> Result<(), SharedRuntimeError> {
            let mut runtime_lock = self.runtime.lock_or_panic();
            if runtime_lock.is_none() {
                *runtime_lock = Some(Arc::new(build_runtime()?));
            }
            Ok(())
        }

        /// Hook to be called in the parent process after forking.
        ///
        /// This method restarts workers and resumes normal operation in the parent process.
        /// The runtime may need to be recreated if it was shut down in before_fork.
        ///
        /// # Errors
        /// Returns an error if workers cannot be restarted or the runtime cannot be recreated.
        pub fn after_fork_parent(&self) -> Result<(), SharedRuntimeError> {
            debug!("after_fork_parent: restarting runtime and workers");
            self.restart_runtime()?;

            let runtime_lock = self.runtime.lock_or_panic();
            let handle = runtime_lock
                .as_ref()
                .ok_or(SharedRuntimeError::RuntimeUnavailable)?
                .handle()
                .clone();
            drop(runtime_lock);

            let spawner = NativeSpawnCapability;
            let mut workers_lock = self.workers.lock_or_panic();

            for worker_entry in workers_lock.iter_mut() {
                worker_entry.worker.start(&spawner, &handle)?;
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
        /// Returns an error if the runtime cannot be reinitialized or workers cannot be started.
        pub fn after_fork_child(&self) -> Result<(), SharedRuntimeError> {
            debug!("after_fork_child: reinitializing runtime and workers");
            self.restart_runtime()?;

            let runtime_lock = self.runtime.lock_or_panic();
            let handle = runtime_lock
                .as_ref()
                .ok_or(SharedRuntimeError::RuntimeUnavailable)?
                .handle()
                .clone();
            drop(runtime_lock);

            let spawner = NativeSpawnCapability;
            let mut workers_lock = self.workers.lock_or_panic();

            workers_lock.retain(|entry| entry.restart_on_fork);

            for worker_entry in workers_lock.iter_mut() {
                worker_entry.worker.reset();
                worker_entry.worker.start(&spawner, &handle)?;
            }

            Ok(())
        }

        /// Run a future to completion on the shared runtime, blocking the current thread.
        ///
        /// If the runtime is not available (e.g. after calling before_fork), a temporary
        /// single-threaded runtime is used.
        ///
        /// Not available on wasm32 -- use async paths instead.
        ///
        /// # Errors
        /// Returns an error if it fails to create a fallback runtime.
        pub fn block_on<F: std::future::Future>(&self, f: F) -> Result<F::Output, io::Error> {
            let runtime = match self.runtime.lock_or_panic().as_ref() {
                None => Arc::new(Builder::new_current_thread().enable_all().build()?),
                Some(runtime) => runtime.clone(),
            };
            Ok(runtime.block_on(f))
        }

        /// Shutdown the runtime and all workers synchronously with optional timeout.
        ///
        /// Not available on wasm32 -- use [`shutdown_async`](Self::shutdown_async) instead.
        ///
        /// Worker errors are logged but do not cause the function to fail.
        ///
        /// # Errors
        /// Returns an error only if shutdown times out.
        pub fn shutdown(
            &self,
            timeout: Option<std::time::Duration>,
        ) -> Result<(), SharedRuntimeError> {
            debug!(?timeout, "Shutting down SharedRuntime");
            match self.runtime.lock_or_panic().take() {
                Some(runtime) => {
                    if let Some(timeout) = timeout {
                        match runtime.block_on(async {
                            tokio::time::timeout(timeout, self.shutdown_async()).await
                        }) {
                            Ok(()) => Ok(()),
                            Err(_) => Err(SharedRuntimeError::ShutdownTimedOut(timeout)),
                        }
                    } else {
                        runtime.block_on(self.shutdown_async());
                        Ok(())
                    }
                }
                None => Ok(()),
            }
        }
    }
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

/// A shared runtime that manages PausableWorkers and provides fork safety hooks.
///
/// The SharedRuntime owns a tokio runtime (on native) and tracks PausableWorkers
/// spawned on it. It provides methods to safely pause workers before forking and
/// restart them after fork in both parent and child processes.
///
/// On wasm32, no tokio runtime is created. Workers are spawned via the caller-provided
/// [`SpawnCapability`] which delegates to `spawn_local` on the JS event loop.
///
/// # Mutex lock order
/// When locking both [Self::runtime] and [Self::workers], the mutex must be locked in the order of
/// the fields in the struct. When possible avoid holding both locks simultaneously.
#[derive(Debug)]
pub struct SharedRuntime {
    #[cfg(not(target_arch = "wasm32"))]
    runtime: Arc<Mutex<Option<Arc<tokio::runtime::Runtime>>>>,
    workers: Arc<Mutex<Vec<WorkerEntry>>>,
    next_worker_id: AtomicU64,
}

impl SharedRuntime {
    /// Create a new SharedRuntime.
    ///
    /// On native, this creates a tokio multi-thread runtime. On wasm32, no runtime
    /// is created (workers are spawned on the JS event loop via [`SpawnCapability`]).
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

    /// Spawn a PausableWorker using the provided spawn capability (wasm variant).
    ///
    /// On wasm the runtime context is `()` — workers are spawned on the JS event loop.
    #[cfg(target_arch = "wasm32")]
    pub fn spawn_worker<
        T: Worker + Sync + 'static,
        S: libdd_capabilities::spawn::SpawnCapability<RuntimeContext = ()> + 'static,
    >(
        &self,
        worker: T,
        restart_on_fork: bool,
        spawner: &S,
    ) -> Result<WorkerHandle, SharedRuntimeError> {
        use std::sync::atomic::Ordering;

        let boxed_worker: BoxedWorker = Box::new(worker);
        debug!(?boxed_worker, "Spawning worker on SharedRuntime");
        let mut pausable_worker = PausableWorker::new(boxed_worker);

        let mut workers_guard = self.workers.lock_or_panic();

        if let Err(e) = pausable_worker.start(spawner, &()) {
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
    use libdd_capabilities_impl::NativeSpawnCapability;
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
        let spawner = NativeSpawnCapability;
        let (worker, receiver) = make_test_worker();

        let result = shared_runtime.spawn_worker(worker, true, &spawner);
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
        let spawner = NativeSpawnCapability;
        let (worker, receiver) = make_test_worker();

        let handle = shared_runtime.spawn_worker(worker, true, &spawner).unwrap();
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
        let spawner = NativeSpawnCapability;
        let (worker, receiver) = make_test_worker();

        shared_runtime.spawn_worker(worker, true, &spawner).unwrap();

        // Let the worker run until state > 0 so that preservation is observable
        let mut state_before_fork = 0;
        while state_before_fork == 0 {
            state_before_fork = receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not advance state before fork");
        }

        shared_runtime.before_fork();
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
        let spawner = NativeSpawnCapability;
        let (worker, receiver) = make_test_worker();

        shared_runtime.spawn_worker(worker, true, &spawner).unwrap();

        // Let the worker run until state > 0 so that the reset is observable
        let mut state_before_fork = 0;
        while state_before_fork == 0 {
            state_before_fork = receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not advance state before fork");
        }

        shared_runtime.before_fork();
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
        let spawner = NativeSpawnCapability;
        let (worker, receiver) = make_test_worker();

        shared_runtime.spawn_worker(worker, true, &spawner).unwrap();

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
        let spawner = NativeSpawnCapability;
        let (worker, receiver) = make_test_worker();

        shared_runtime
            .spawn_worker(worker, false, &spawner)
            .unwrap();

        // Wait for the worker to run at least once
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");

        shared_runtime.before_fork();
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
}
