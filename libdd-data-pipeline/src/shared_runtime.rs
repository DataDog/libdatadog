// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! SharedRuntime for managing PausableWorkers across fork boundaries.
//!
//! This module provides a SharedRuntime that manages a tokio runtime and allows
//! spawning PausableWorkers on it. It also provides hooks for safely handling
//! fork operations by pausing workers before fork and restarting them appropriately
//! in parent and child processes.

use crate::pausable_worker::{PausableWorker, PausableWorkerError};
use libdd_common::{worker::Worker, MutexExt};
use std::fmt;
use std::sync::{Arc, Mutex};
use tokio::runtime::{Builder, Runtime};

/// Type alias for a boxed worker trait object that can be used with PausableWorker.
type BoxedWorker = Box<dyn Worker + Send + Sync>;

/// Errors that can occur when using SharedRuntime.
#[derive(Debug)]
pub enum SharedRuntimeError {
    /// The runtime is not available or in an invalid state.
    RuntimeUnavailable,
    /// Failed to acquire a lock on internal state.
    LockFailed(String),
    /// A worker operation failed.
    WorkerError(PausableWorkerError),
    /// Failed to create or manage the tokio runtime.
    RuntimeCreation(std::io::Error),
    /// A generic error occurred.
    Other(String),
}

impl fmt::Display for SharedRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SharedRuntimeError::RuntimeUnavailable => {
                write!(f, "Runtime is not available or in an invalid state")
            }
            SharedRuntimeError::LockFailed(msg) => write!(f, "Failed to acquire lock: {}", msg),
            SharedRuntimeError::WorkerError(err) => write!(f, "Worker error: {}", err),
            SharedRuntimeError::RuntimeCreation(err) => {
                write!(f, "Failed to create runtime: {}", err)
            }
            SharedRuntimeError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for SharedRuntimeError {}

impl From<PausableWorkerError> for SharedRuntimeError {
    fn from(err: PausableWorkerError) -> Self {
        SharedRuntimeError::WorkerError(err)
    }
}

impl From<std::io::Error> for SharedRuntimeError {
    fn from(err: std::io::Error) -> Self {
        SharedRuntimeError::RuntimeCreation(err)
    }
}

/// A shared runtime that manages PausableWorkers and provides fork safety hooks.
///
/// The SharedRuntime owns a tokio runtime and tracks PausableWorkers spawned on it.
/// It provides methods to safely pause workers before forking and restart them
/// after fork in both parent and child processes.
pub struct SharedRuntime {
    runtime: Arc<Mutex<Option<Arc<Runtime>>>>,
    workers: Arc<Mutex<Vec<PausableWorker<BoxedWorker>>>>,
}

impl std::fmt::Debug for SharedRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedRuntime")
            .field("runtime", &self.runtime)
            .field("workers", &"<opaque>")
            .finish()
    }
}

impl SharedRuntime {
    /// Create a new SharedRuntime with a default multi-threaded tokio runtime.
    ///
    /// # Errors
    /// Returns an error if the tokio runtime cannot be created.
    pub fn new() -> Result<Self, SharedRuntimeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;

        Ok(Self {
            runtime: Arc::new(Mutex::new(Some(Arc::new(runtime)))),
            workers: Arc::new(Mutex::new(Vec::new())),
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
    ) -> Result<(), SharedRuntimeError> {
        let boxed_worker: BoxedWorker = Box::new(worker);
        let mut pausable_worker = PausableWorker::new(boxed_worker);

        let runtime_lock = self.runtime.lock_or_panic();

        // If the runtime is not available, it's added to the worker list and will be started when
        // the runtime is recreated.
        if let Some(runtime) = runtime_lock.as_ref() {
            pausable_worker.start(runtime)?;
        }

        let mut workers_lock = self.workers.lock_or_panic();
        workers_lock.push(pausable_worker);

        Ok(())
    }

    /// Hook to be called before forking.
    ///
    /// This method pauses all workers and prepares the runtime for forking.
    /// It ensures that no background tasks are running when the fork occurs,
    /// preventing potential deadlocks in the child process.
    ///
    /// # Errors
    /// Returns an error if workers cannot be paused or the runtime is in an invalid state.
    pub fn before_fork(&self) -> Result<(), SharedRuntimeError> {
        if let Some(runtime) = self.runtime.lock_or_panic().take() {
            runtime.block_on(async {
                let mut workers_lock = self.workers.lock_or_panic();

                // First signal all workers to pause, then wait for each one to stop.
                for pausable_worker in workers_lock.iter_mut() {
                    pausable_worker.request_pause()?;
                }

                for pausable_worker in workers_lock.iter_mut() {
                    pausable_worker.join().await?;
                }
                Ok::<(), PausableWorkerError>(())
            })?;
        }
        Ok(())
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
        self.restart_runtime()?;

        let runtime_lock = self.runtime.lock_or_panic();
        let runtime = runtime_lock
            .as_ref()
            .ok_or(SharedRuntimeError::RuntimeUnavailable)?;

        let mut workers_lock = self.workers.lock_or_panic();

        // Restart all workers
        for pausable_worker in workers_lock.iter_mut() {
            pausable_worker.start(runtime)?;
        }

        Ok(())
    }

    /// Hook to be called in the child process after forking.
    ///
    /// This method reinitializes the runtime and workers in the child process.
    /// A new runtime must be created since tokio runtimes cannot be safely forked.
    /// Workers can optionally be restarted to resume operations in the child.
    ///
    /// # Errors
    /// Returns an error if the runtime cannot be reinitialized or workers cannot be started.
    pub fn after_fork_child(&self) -> Result<(), SharedRuntimeError> {
        self.restart_runtime()?;

        let runtime_lock = self.runtime.lock_or_panic();
        let runtime = runtime_lock
            .as_ref()
            .ok_or(SharedRuntimeError::RuntimeUnavailable)?;

        let mut workers_lock = self.workers.lock_or_panic();

        // Restart all workers in child process
        for pausable_worker in workers_lock.iter_mut() {
            pausable_worker.reset();
            pausable_worker.start(runtime)?;
        }

        Ok(())
    }

    /// Get a reference to the underlying runtime.
    ///
    /// This allows external code to spawn additional tasks on the runtime if needed.
    ///
    /// # Errors
    /// Returns None if the runtime is not available (e.g., during fork operations).
    pub fn runtime(&self) -> Arc<Runtime> {
        match self.runtime.lock_or_panic().as_ref() {
            None => Arc::new(Builder::new_current_thread().enable_all().build().unwrap()),
            Some(runtime) => runtime.clone(),
        }
    }

    /// Shutdown the runtime and all workers.
    ///
    /// This should be called during application shutdown to cleanly stop all
    /// background workers and the runtime.
    ///
    /// Note: The runtime itself is not dropped by this method to avoid issues with
    /// dropping a runtime from within an async context. The runtime will be dropped
    /// when the SharedRuntime is dropped from a synchronous context.
    ///
    /// # Errors
    /// Returns an error if workers cannot be stopped.
    pub async fn shutdown(&self) -> Result<(), SharedRuntimeError> {
        let mut workers_lock = self.workers.lock_or_panic();

        // Pause all workers
        for pausable_worker in workers_lock.iter_mut() {
            pausable_worker.pause().await?;
        }

        // Note: We don't drop the runtime here because dropping a runtime from
        // within an async context causes a panic. The runtime will be properly
        // cleaned up when SharedRuntime is dropped from a synchronous context.

        Ok(())
    }
}

impl Default for SharedRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create default SharedRuntime")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::mpsc::{channel, Sender};
    use std::time::Duration;
    use tokio::time::sleep;

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

        // TODO: Complete this test once spawn_worker properly stores workers
        let result = shared_runtime.spawn_worker(worker);
        assert!(result.is_ok());
    }

    #[test]
    fn test_before_and_after_fork_parent() {
        // Run in a separate thread to ensure we're not in any async context
        let handle = std::thread::spawn(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let shared_runtime = SharedRuntime::new().unwrap();

            // Test before_fork
            assert!(shared_runtime.before_fork().is_ok());

            // Test after_fork_parent (synchronous)
            assert!(shared_runtime.after_fork_parent().is_ok());

            // Clean shutdown
            rt.block_on(async {
                assert!(shared_runtime.shutdown().await.is_ok());
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
