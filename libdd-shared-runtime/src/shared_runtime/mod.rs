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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::{fmt, io};
use tokio::runtime::{Builder, Runtime};
use tracing::{debug, error};

type BoxedWorker = Box<dyn Worker + Send + Sync>;

#[derive(Debug)]
struct WorkerEntry {
    id: u64,
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
/// The SharedRuntime owns a tokio runtime and tracks PausableWorkers spawned on it.
/// It provides methods to safely pause workers before forking and restart them
/// after fork in both parent and child processes.
///
/// # Mutex lock order
/// When locking both [Self::runtime] and [Self::workers], the mutex must be locked in the order of
/// the fields in the struct. When possible avoid holding both locks simultaneously.
#[derive(Debug)]
pub struct SharedRuntime {
    runtime: Arc<Mutex<Option<Arc<Runtime>>>>,
    workers: Arc<Mutex<Vec<WorkerEntry>>>,
    next_worker_id: AtomicU64,
}

impl SharedRuntime {
    /// Create a new SharedRuntime with a default multi-threaded tokio runtime.
    ///
    /// # Errors
    /// Returns an error if the tokio runtime cannot be created.
    pub fn new() -> Result<Self, SharedRuntimeError> {
        debug!("Creating new SharedRuntime");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;

        Ok(Self {
            runtime: Arc::new(Mutex::new(Some(Arc::new(runtime)))),
            workers: Arc::new(Mutex::new(Vec::new())),
            next_worker_id: AtomicU64::new(1),
        })
    }

    /// Spawn a PausableWorker on this runtime.
    ///
    /// The worker will be tracked by this SharedRuntime and will be paused/resumed
    /// during fork operations.
    ///
    /// # Errors
    /// Returns an error if the runtime is not available or the worker cannot be started.
    pub fn spawn_worker<T: Worker + Send + Sync + 'static>(
        &self,
        worker: T,
    ) -> Result<WorkerHandle, SharedRuntimeError> {
        let boxed_worker: BoxedWorker = Box::new(worker);
        debug!(?boxed_worker, "Spawning worker on SharedRuntime");
        let mut pausable_worker = PausableWorker::new(boxed_worker);
        let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);

        {
            let runtime_lock = self.runtime.lock_or_panic();

            // If the runtime is not available, it's added to the worker list and will be started
            // when the runtime is recreated.
            if let Some(runtime) = runtime_lock.as_ref() {
                pausable_worker.start(runtime)?;
            }
        }

        let mut workers_lock = self.workers.lock_or_panic();
        workers_lock.push(WorkerEntry {
            id: worker_id,
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
            *runtime_lock = Some(Arc::new(
                Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_all()
                    .build()?,
            ));
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
        let runtime = runtime_lock
            .as_ref()
            .ok_or(SharedRuntimeError::RuntimeUnavailable)?
            .clone();
        drop(runtime_lock);

        let mut workers_lock = self.workers.lock_or_panic();

        // Restart all workers
        for worker_entry in workers_lock.iter_mut() {
            worker_entry.worker.start(&runtime)?;
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
        let runtime = runtime_lock
            .as_ref()
            .ok_or(SharedRuntimeError::RuntimeUnavailable)?
            .clone();
        drop(runtime_lock);

        let mut workers_lock = self.workers.lock_or_panic();

        // Restart all workers in child process
        for worker_entry in workers_lock.iter_mut() {
            worker_entry.worker.reset();
            worker_entry.worker.start(&runtime)?;
        }

        Ok(())
    }

    /// Run a future to completion on the shared runtime, blocking the current thread.
    ///
    /// If the runtime is not available (e.g. after calling before_fork), a temporary
    /// single-threaded runtime is used.
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
    /// Worker errors are logged but do not cause the function to fail.
    ///
    /// # Errors
    /// Returns an error only if shutdown times out.
    pub fn shutdown(&self, timeout: Option<std::time::Duration>) -> Result<(), SharedRuntimeError> {
        debug!(?timeout, "Shutting down SharedRuntime");
        match self.runtime.lock_or_panic().take() {
            Some(runtime) => {
                let result = if let Some(timeout) = timeout {
                    match runtime.block_on(async {
                        tokio::time::timeout(timeout, self.shutdown_async()).await
                    }) {
                        Ok(()) => Ok(()),
                        Err(_) => Err(SharedRuntimeError::ShutdownTimedOut(timeout)),
                    }
                } else {
                    runtime.block_on(self.shutdown_async());
                    Ok(())
                };
                result
            }
            None => Ok(()), // The runtime is not running so there's nothing to shutdown
        }
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
    use std::sync::mpsc::{channel, Sender};
    use std::time::Duration;
    use tokio::time::sleep;

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
    fn test_shared_runtime_creation() {
        let shared_runtime = SharedRuntime::new();
        assert!(shared_runtime.is_ok());
    }

    #[test]
    fn test_spawn_worker() {
        let shared_runtime = SharedRuntime::new().unwrap();
        let (sender, _receiver) = channel::<u32>();
        let worker = TestWorker { state: 0, sender };

        let result = shared_runtime.spawn_worker(worker);
        assert!(result.is_ok());
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 1);
    }

    #[test]
    fn test_worker_handle_stop_removes_worker() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let shared_runtime = SharedRuntime::new().unwrap();
        let (sender, _receiver) = channel::<u32>();
        let worker = TestWorker { state: 0, sender };

        let handle = shared_runtime.spawn_worker(worker).unwrap();
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 1);

        rt.block_on(async {
            assert!(handle.stop().await.is_ok());
        });

        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);
    }

    #[test]
    fn test_before_and_after_fork_parent() {
        // Run in a separate thread to ensure we're not in any async context
        let handle = std::thread::spawn(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let shared_runtime = SharedRuntime::new().unwrap();

            // Test before_fork
            shared_runtime.before_fork();

            // Test after_fork_parent (synchronous)
            assert!(shared_runtime.after_fork_parent().is_ok());

            // Clean shutdown
            rt.block_on(async {
                shared_runtime.shutdown_async().await;
            });
        });

        handle.join().expect("Thread panicked");
    }

    #[test]
    fn test_after_fork_child() {
        // Test after_fork_child in a non-async context
        let shared_runtime = SharedRuntime::new().unwrap();

        // This should succeed as we're not in an async context
        assert!(shared_runtime.after_fork_child().is_ok());
    }
}
