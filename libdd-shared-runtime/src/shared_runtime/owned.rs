// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use crate::worker::Worker;
    use futures::stream::{FuturesUnordered, StreamExt};
    use libdd_common::MutexExt;
    use std::io;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::runtime::{Builder, Runtime};
    use tokio_util::sync::CancellationToken;
    use tracing::{debug, error};

    use super::super::{
        pausable_worker::{tokio_spawn_fn, PausableWorker},
        BoxedWorker, SharedRuntime, SharedRuntimeError, WorkerEntry, WorkerHandle,
    };

    /// Flavor of tokio runtime constructed by [`OwnedSharedRuntime`].
    #[derive(Debug, Clone)]
    pub enum OwnedKind {
        /// Multi-threaded runtime with the given worker-thread count.
        MultiThread { worker_threads: usize },
        /// Current-thread runtime; a dedicated driver thread is spawned so tasks make progress.
        CurrentThread,
    }

    impl Default for OwnedKind {
        fn default() -> Self {
            Self::MultiThread { worker_threads: 1 }
        }
    }

    fn build_runtime_for(kind: &OwnedKind) -> Result<Runtime, io::Error> {
        match kind {
            OwnedKind::MultiThread { worker_threads } => Builder::new_multi_thread()
                .worker_threads(*worker_threads)
                .enable_all()
                .build(),
            OwnedKind::CurrentThread => Builder::new_current_thread().enable_all().build(),
        }
    }

    struct CurrentThreadDriver {
        cancel: CancellationToken,
        thread: std::thread::JoinHandle<()>,
    }

    impl std::fmt::Debug for CurrentThreadDriver {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("CurrentThreadDriver")
                .finish_non_exhaustive()
        }
    }

    fn spawn_current_thread_driver(
        rt: &Arc<Runtime>,
    ) -> Result<CurrentThreadDriver, SharedRuntimeError> {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let rt = rt.clone();
        let thread = std::thread::Builder::new()
            .name("shared-runtime-driver".into())
            .spawn(move || {
                rt.block_on(cancel_clone.cancelled());
            })
            .map_err(SharedRuntimeError::RuntimeCreation)?;
        Ok(CurrentThreadDriver { cancel, thread })
    }

    fn stop_current_thread_driver(d: CurrentThreadDriver) {
        d.cancel.cancel();
        if let Err(e) = d.thread.join() {
            error!(
                "current-thread driver thread panicked while joining: {:?}",
                e
            );
        }
    }

    /// Owns a tokio runtime and manages [`PausableWorker`]s on it.
    ///
    /// Supports the full fork protocol ([`before_fork`](Self::before_fork) /
    /// [`after_fork_parent`](Self::after_fork_parent) /
    /// [`after_fork_child`](Self::after_fork_child)) and synchronous [`shutdown`](Self::shutdown).
    ///
    /// # Mutex lock order
    /// `runtime` → `driver` → `workers`. Avoid holding multiple locks simultaneously.
    #[derive(Debug)]
    pub struct OwnedSharedRuntime {
        kind: OwnedKind,
        runtime: Arc<Mutex<Option<Arc<Runtime>>>>,
        driver: Mutex<Option<CurrentThreadDriver>>,
        workers: Arc<Mutex<Vec<WorkerEntry>>>,
        next_worker_id: AtomicU64,
    }

    impl OwnedSharedRuntime {
        /// Creates a multi-thread runtime with 1 worker thread.
        pub fn new() -> Result<Self, SharedRuntimeError> {
            debug!("Creating new OwnedSharedRuntime");
            Self::with_kind(OwnedKind::default())
        }

        /// Creates an `OwnedSharedRuntime` with the given runtime flavor.
        /// For [`OwnedKind::CurrentThread`] a dedicated driver thread is also spawned.
        pub fn with_kind(kind: OwnedKind) -> Result<Self, SharedRuntimeError> {
            let runtime = Arc::new(build_runtime_for(&kind)?);
            let driver = if matches!(kind, OwnedKind::CurrentThread) {
                Some(spawn_current_thread_driver(&runtime)?)
            } else {
                None
            };
            Ok(Self {
                kind,
                runtime: Arc::new(Mutex::new(Some(runtime))),
                driver: Mutex::new(driver),
                workers: Arc::new(Mutex::new(Vec::new())),
                next_worker_id: AtomicU64::new(1),
            })
        }

        /// Returns the runtime handle, or [`SharedRuntimeError::RuntimeUnavailable`] if shut down.
        pub fn runtime_handle(&self) -> Result<tokio::runtime::Handle, SharedRuntimeError> {
            Ok(self
                .runtime
                .lock_or_panic()
                .as_ref()
                .ok_or(SharedRuntimeError::RuntimeUnavailable)?
                .handle()
                .clone())
        }

        /// Pauses all workers before `fork()`. Worker pause errors are logged, not propagated.
        pub fn before_fork(&self) {
            debug!("before_fork: pausing all workers");
            if let Some(runtime) = self.runtime.lock_or_panic().take() {
                // Stop the current-thread driver first (if any) so we can drive
                // the runtime ourselves via block_on below. For MultiThread this
                // is a no-op.
                if let Some(driver) = self.driver.lock_or_panic().take() {
                    stop_current_thread_driver(driver);
                }
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

        fn restart_runtime_for_kind(&self) -> Result<(), SharedRuntimeError> {
            let mut runtime_lock = self.runtime.lock_or_panic();
            if runtime_lock.is_none() {
                let rt = Arc::new(build_runtime_for(&self.kind)?);
                if matches!(self.kind, OwnedKind::CurrentThread) {
                    let driver = spawn_current_thread_driver(&rt)?;
                    *self.driver.lock_or_panic() = Some(driver);
                }
                *runtime_lock = Some(rt);
            }
            Ok(())
        }

        /// Restarts the runtime and workers in the parent after forking; worker state is preserved.
        pub fn after_fork_parent(&self) -> Result<(), SharedRuntimeError> {
            debug!("after_fork_parent: restarting runtime and workers");
            self.restart_runtime_for_kind()?;

            let runtime_lock = self.runtime.lock_or_panic();
            let handle = runtime_lock
                .as_ref()
                .ok_or(SharedRuntimeError::RuntimeUnavailable)?
                .handle()
                .clone();
            drop(runtime_lock);

            let mut workers_lock = self.workers.lock_or_panic();

            for worker_entry in workers_lock.iter_mut() {
                worker_entry.worker.start(tokio_spawn_fn(&handle))?;
            }

            Ok(())
        }

        /// Reinitializes the runtime in the child after forking.
        /// Workers with `restart_on_fork = true` are reset and restarted; others are dropped
        /// without shutdown.
        pub fn after_fork_child(&self) -> Result<(), SharedRuntimeError> {
            debug!("after_fork_child: reinitializing runtime and workers");
            self.restart_runtime_for_kind()?;

            let runtime_lock = self.runtime.lock_or_panic();
            let handle = runtime_lock
                .as_ref()
                .ok_or(SharedRuntimeError::RuntimeUnavailable)?
                .handle()
                .clone();
            drop(runtime_lock);

            let mut workers_lock = self.workers.lock_or_panic();

            workers_lock.retain(|entry| entry.restart_on_fork);

            for worker_entry in workers_lock.iter_mut() {
                worker_entry.worker.reset();
                worker_entry.worker.start(tokio_spawn_fn(&handle))?;
            }

            Ok(())
        }

        /// Blocks on the owned runtime. Falls back to a temporary current-thread runtime in the
        /// fork window.
        pub fn block_on<F: std::future::Future>(&self, f: F) -> Result<F::Output, io::Error> {
            let runtime = match self.runtime.lock_or_panic().as_ref() {
                None => Arc::new(Builder::new_current_thread().enable_all().build()?),
                Some(runtime) => runtime.clone(),
            };
            Ok(runtime.block_on(f))
        }

        /// Shuts down all workers synchronously. Returns `ShutdownTimedOut` if `timeout` is
        /// exceeded.
        pub fn shutdown(
            &self,
            timeout: Option<std::time::Duration>,
        ) -> Result<(), SharedRuntimeError> {
            debug!(?timeout, "Shutting down OwnedSharedRuntime");
            match self.runtime.lock_or_panic().take() {
                Some(runtime) => {
                    // Stop the driver (if any) before driving the runtime
                    // ourselves via block_on.
                    if let Some(driver) = self.driver.lock_or_panic().take() {
                        stop_current_thread_driver(driver);
                    }
                    if let Some(timeout) = timeout {
                        match runtime.block_on(async {
                            tokio::time::timeout(
                                timeout,
                                <Self as SharedRuntime>::shutdown_async(self),
                            )
                            .await
                        }) {
                            Ok(()) => Ok(()),
                            Err(_) => Err(SharedRuntimeError::ShutdownTimedOut(timeout)),
                        }
                    } else {
                        runtime.block_on(<Self as SharedRuntime>::shutdown_async(self));
                        Ok(())
                    }
                }
                None => Ok(()),
            }
        }

        fn push_worker(
            &self,
            workers_guard: &mut std::sync::MutexGuard<Vec<WorkerEntry>>,
            pausable_worker: PausableWorker<BoxedWorker>,
            restart_on_fork: bool,
        ) -> WorkerHandle {
            let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);
            workers_guard.push(WorkerEntry {
                id: worker_id,
                restart_on_fork,
                worker: pausable_worker,
            });
            WorkerHandle {
                worker_id,
                workers: self.workers.clone(),
            }
        }
    }

    impl SharedRuntime for OwnedSharedRuntime {
        fn spawn_worker<T: Worker + Sync + 'static>(
            &self,
            worker: T,
            restart_on_fork: bool,
        ) -> Result<WorkerHandle, SharedRuntimeError> {
            let boxed_worker: BoxedWorker = Box::new(worker);
            debug!(?boxed_worker, "Spawning worker on OwnedSharedRuntime");
            let mut pausable_worker = PausableWorker::new(boxed_worker);

            // Hold both locks together (runtime → workers, per struct lock order) so
            // before_fork cannot interleave between start and push. If runtime is already
            // None (fork window), skip start; after_fork_* will pick it up.
            let runtime_guard = self.runtime.lock_or_panic();
            let mut workers_guard = self.workers.lock_or_panic();

            if let Some(rt) = runtime_guard.as_ref() {
                pausable_worker.start(tokio_spawn_fn(rt.handle()))?;
            }

            Ok(self.push_worker(&mut workers_guard, pausable_worker, restart_on_fork))
        }

        fn runtime_handle(&self) -> Result<tokio::runtime::Handle, SharedRuntimeError> {
            OwnedSharedRuntime::runtime_handle(self)
        }

        async fn shutdown_async(&self) {
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
        fn test_owned_runtime_creation() {
            let shared_runtime = OwnedSharedRuntime::new();
            assert!(shared_runtime.is_ok());
        }

        #[test]
        fn test_spawn_worker() {
            let shared_runtime = OwnedSharedRuntime::new().unwrap();
            let (worker, receiver) = make_test_worker();

            let result = shared_runtime.spawn_worker(worker, true);
            assert!(result.is_ok());
            assert_eq!(shared_runtime.workers.lock_or_panic().len(), 1);

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
            let shared_runtime = OwnedSharedRuntime::new().unwrap();
            let (worker, receiver) = make_test_worker();

            let handle = shared_runtime.spawn_worker(worker, true).unwrap();
            assert_eq!(shared_runtime.workers.lock_or_panic().len(), 1);

            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run");

            rt.block_on(async {
                assert!(handle.stop().await.is_ok());
            });

            assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);

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
            let shared_runtime = OwnedSharedRuntime::new().unwrap();
            let (worker, receiver) = make_test_worker();

            let _ = shared_runtime.spawn_worker(worker, true).unwrap();

            let mut state_before_fork = 0;
            while state_before_fork == 0 {
                state_before_fork = receiver
                    .recv_timeout(Duration::from_secs(1))
                    .expect("worker did not advance state before fork");
            }

            shared_runtime.before_fork();
            while receiver.try_recv().is_ok() {}

            assert!(shared_runtime.after_fork_parent().is_ok());

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
            let shared_runtime = OwnedSharedRuntime::new().unwrap();
            let (worker, receiver) = make_test_worker();

            let _ = shared_runtime.spawn_worker(worker, true).unwrap();

            let mut state_before_fork = 0;
            while state_before_fork == 0 {
                state_before_fork = receiver
                    .recv_timeout(Duration::from_secs(1))
                    .expect("worker did not advance state before fork");
            }

            shared_runtime.before_fork();
            while receiver.try_recv().is_ok() {}

            assert!(shared_runtime.after_fork_child().is_ok());

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
            let shared_runtime = OwnedSharedRuntime::new().unwrap();
            let (worker, receiver) = make_test_worker();

            let _ = shared_runtime.spawn_worker(worker, true).unwrap();

            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run");

            shared_runtime.shutdown(None).unwrap();

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
            let shared_runtime = OwnedSharedRuntime::new().unwrap();
            let (worker, receiver) = make_test_worker();

            let _ = shared_runtime.spawn_worker(worker, false).unwrap();

            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run");

            shared_runtime.before_fork();
            while receiver.try_recv().is_ok() {}

            assert!(shared_runtime.after_fork_child().is_ok());

            assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);

            assert!(
                receiver.recv_timeout(Duration::from_millis(200)).is_err(),
                "worker should not run or shut down after fork in child when restart_on_fork is false"
            );
        }

        // --- OwnedKind::CurrentThread tests ---

        #[test]
        fn test_current_thread_spawn_worker_runs() {
            let shared_runtime = OwnedSharedRuntime::with_kind(OwnedKind::CurrentThread).unwrap();
            let (worker, receiver) = make_test_worker();

            let _ = shared_runtime.spawn_worker(worker, true).unwrap();

            // Worker can only make progress if the driver thread is alive.
            assert_eq!(
                receiver
                    .recv_timeout(Duration::from_secs(1))
                    .expect("worker did not run on current-thread runtime"),
                0
            );

            shared_runtime.shutdown(None).unwrap();
        }

        #[test]
        fn test_current_thread_shutdown_stops_driver() {
            let shared_runtime = OwnedSharedRuntime::with_kind(OwnedKind::CurrentThread).unwrap();
            let (worker, receiver) = make_test_worker();

            let _ = shared_runtime.spawn_worker(worker, true).unwrap();
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run");

            shared_runtime.shutdown(None).unwrap();

            assert!(matches!(
                shared_runtime.runtime_handle(),
                Err(SharedRuntimeError::RuntimeUnavailable)
            ));
            assert!(shared_runtime.driver.lock_or_panic().is_none());
        }

        #[test]
        fn test_current_thread_fork_cycle_respawns_driver() {
            let shared_runtime = OwnedSharedRuntime::with_kind(OwnedKind::CurrentThread).unwrap();
            let (worker, receiver) = make_test_worker();

            let _ = shared_runtime.spawn_worker(worker, true).unwrap();
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run pre-fork");

            shared_runtime.before_fork();
            while receiver.try_recv().is_ok() {}

            assert!(shared_runtime.driver.lock_or_panic().is_none());
            assert!(shared_runtime.after_fork_parent().is_ok());
            assert!(shared_runtime.driver.lock_or_panic().is_some());

            // Worker resumption is only possible if the driver was re-spawned.
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not resume after fork on current-thread runtime");

            shared_runtime.shutdown(None).unwrap();
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use crate::worker::Worker;
    use futures::stream::{FuturesUnordered, StreamExt};
    use libdd_common::MutexExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tracing::{debug, error};

    use super::super::{
        pausable_worker::PausableWorker, BoxedWorker, SharedRuntime, SharedRuntimeError,
        WorkerEntry, WorkerHandle,
    };

    /// Owns workers running via `spawn_local` on wasm32.
    #[derive(Debug)]
    pub struct OwnedSharedRuntime {
        workers: Arc<Mutex<Vec<WorkerEntry>>>,
        next_worker_id: AtomicU64,
    }

    impl OwnedSharedRuntime {
        pub fn new() -> Result<Self, SharedRuntimeError> {
            debug!("Creating new OwnedSharedRuntime (wasm)");
            Ok(Self {
                workers: Arc::new(Mutex::new(Vec::new())),
                next_worker_id: AtomicU64::new(1),
            })
        }

        fn push_worker(
            &self,
            workers_guard: &mut std::sync::MutexGuard<Vec<WorkerEntry>>,
            pausable_worker: PausableWorker<BoxedWorker>,
            restart_on_fork: bool,
        ) -> WorkerHandle {
            let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);
            workers_guard.push(WorkerEntry {
                id: worker_id,
                restart_on_fork,
                worker: pausable_worker,
            });
            WorkerHandle {
                worker_id,
                workers: self.workers.clone(),
            }
        }
    }

    impl SharedRuntime for OwnedSharedRuntime {
        fn spawn_worker<T: Worker + Sync + 'static>(
            &self,
            worker: T,
            restart_on_fork: bool,
        ) -> Result<WorkerHandle, SharedRuntimeError> {
            let boxed_worker: BoxedWorker = Box::new(worker);
            debug!(
                ?boxed_worker,
                "Spawning worker on OwnedSharedRuntime (wasm)"
            );
            let mut pausable_worker = PausableWorker::new(boxed_worker);
            let mut workers_guard = self.workers.lock_or_panic();

            pausable_worker.start(|future| {
                use futures_util::FutureExt;
                let (remote, handle) = future.remote_handle();
                wasm_bindgen_futures::spawn_local(remote);
                Box::pin(async { Ok(handle.await) })
            })?;

            Ok(self.push_worker(&mut workers_guard, pausable_worker, restart_on_fork))
        }

        async fn shutdown_async(&self) {
            debug!("Shutting down all workers asynchronously (wasm)");
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
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
