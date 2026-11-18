// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::worker::Worker;
use futures::stream::{FuturesUnordered, StreamExt};
use libdd_common::MutexExt;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::runtime::{Builder, Runtime};
use tracing::{debug, error};

use super::{
    BlockingRuntime, BoxedWorker, SharedRuntime, SharedRuntimeError, WorkerEntry, WorkerHandle,
    pausable_worker::{PausableWorker, tokio_spawn_fn},
};

fn build_runtime(worker_threads: usize) -> Result<Runtime, io::Error> {
    Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
}

/// Owns a tokio runtime and manages `PausableWorker`s on it.
///
/// Supports the full fork protocol ([`before_fork`](Self::before_fork) /
/// [`after_fork_parent`](Self::after_fork_parent) /
/// [`after_fork_child`](Self::after_fork_child)) and synchronous [`shutdown`](Self::shutdown).
#[derive(Debug)]
pub struct ForkSafeRuntime {
    worker_threads: usize,
    // Lock order: `runtime` must be acquired before `workers`.
    runtime: Arc<Mutex<Option<Arc<Runtime>>>>,
    workers: Arc<Mutex<Vec<WorkerEntry>>>,
    next_worker_id: AtomicU64,
}

impl ForkSafeRuntime {
    /// Creates a `ForkSafeRuntime` with the given number of tokio worker threads.
    pub fn with_worker_threads(worker_threads: usize) -> Result<Self, SharedRuntimeError> {
        let runtime = Arc::new(build_runtime(worker_threads)?);
        Ok(Self {
            worker_threads,
            runtime: Arc::new(Mutex::new(Some(runtime))),
            workers: Arc::new(Mutex::new(Vec::new())),
            next_worker_id: AtomicU64::new(1),
        })
    }

    /// Pauses all workers before `fork()`. Worker pause errors are logged, not propagated.
    pub fn before_fork(&self) {
        debug!("before_fork: pausing all workers");
        let mut runtime_lock = self.runtime.lock_or_panic();
        let Some(runtime) = runtime_lock.take() else {
            return;
        };
        let mut workers_lock = self.workers.lock_or_panic();
        runtime.block_on(async {
            let futures: FuturesUnordered<_> = workers_lock
                .iter_mut()
                .map(|worker_entry| async {
                    worker_entry.honor_discard_claim();
                    if let Err(e) = worker_entry.worker.pause().await {
                        error!("Worker failed to pause before fork: {:?}", e);
                    }
                })
                .collect();

            futures.collect::<()>().await;
        });
    }

    fn restart_runtime(&self) -> Result<(), SharedRuntimeError> {
        let mut runtime_lock = self.runtime.lock_or_panic();
        if runtime_lock.is_none() {
            *runtime_lock = Some(Arc::new(build_runtime(self.worker_threads)?));
        }
        Ok(())
    }

    /// Restarts the runtime and workers in the parent after forking; worker state is preserved.
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

        let mut workers_lock = self.workers.lock_or_panic();

        // Unregister retirements that finished during the fork window.
        workers_lock.retain(|entry| !entry.worker.is_retired());

        for worker_entry in workers_lock.iter_mut() {
            worker_entry.honor_discard_claim();
            if let Err(e) = worker_entry.worker.start(tokio_spawn_fn(&handle)) {
                error!(
                    worker_id = worker_entry.id,
                    "Worker failed to restart after fork in parent: {:?}", e
                )
            }
        }

        Ok(())
    }

    /// Reinitializes the runtime in the child after forking.
    /// Workers with `restart_on_fork = true` are reset and restarted; others are dropped
    /// without shutdown.
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

        let mut workers_lock = self.workers.lock_or_panic();

        // before_fork awaited every pending discard, so retired entries can be dropped here.
        workers_lock.retain(|entry| entry.restart_on_fork && !entry.worker.is_retired());

        for worker_entry in workers_lock.iter_mut() {
            worker_entry.honor_discard_claim();
            worker_entry.worker.reset();
            if let Err(e) = worker_entry.worker.start(tokio_spawn_fn(&handle)) {
                error!(
                    worker_id = worker_entry.id,
                    "Worker failed to restart after fork in parent: {:?}", e
                )
            }
        }

        Ok(())
    }

    /// Shuts down all workers synchronously. Returns `ShutdownTimedOut` if `timeout` is
    /// exceeded.
    pub fn shutdown(&self, timeout: Option<std::time::Duration>) -> Result<(), SharedRuntimeError> {
        debug!(?timeout, "Shutting down ForkSafeRuntime");
        match self.runtime.lock_or_panic().take() {
            Some(runtime) => {
                if let Some(timeout) = timeout {
                    match runtime.block_on(async {
                        tokio::time::timeout(timeout, <Self as SharedRuntime>::shutdown_async(self))
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
        disposition: Arc<std::sync::atomic::AtomicU8>,
        restart_on_fork: bool,
    ) -> WorkerHandle {
        let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);
        workers_guard.push(WorkerEntry {
            id: worker_id,
            disposition: disposition.clone(),
            restart_on_fork,
            worker: pausable_worker,
        });
        WorkerHandle {
            worker_id,
            disposition,
            workers: self.workers.clone(),
        }
    }
}

impl SharedRuntime for ForkSafeRuntime {
    fn new() -> Result<Self, SharedRuntimeError> {
        Self::with_worker_threads(1)
    }

    fn spawn_worker<T: Worker + 'static>(
        &self,
        worker: T,
        restart_on_fork: bool,
    ) -> Result<WorkerHandle, SharedRuntimeError> {
        let boxed_worker: BoxedWorker = Box::new(worker);
        debug!(?boxed_worker, "Spawning worker on ForkSafeRuntime");
        let disposition = Arc::new(std::sync::atomic::AtomicU8::new(super::ACTIVE));
        let mut pausable_worker = PausableWorker::new(boxed_worker, disposition.clone());

        // Hold both locks together (runtime → workers, per struct lock order) so
        // before_fork cannot interleave between start and push. If runtime is already
        // None (fork window), skip start; after_fork_* will pick it up.
        let runtime_guard = self.runtime.lock_or_panic();
        let mut workers_guard = self.workers.lock_or_panic();

        if let Some(rt) = runtime_guard.as_ref() {
            pausable_worker.start(tokio_spawn_fn(rt.handle()))?;
        }

        Ok(self.push_worker(
            &mut workers_guard,
            pausable_worker,
            disposition,
            restart_on_fork,
        ))
    }

    async fn shutdown_async(&self) {
        debug!("Shutting down all workers asynchronously");
        let (workers, retirements) = super::take_for_shutdown(&self.workers);

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
        super::finish_retirements(retirements).await;
    }
}

impl BlockingRuntime for ForkSafeRuntime {
    /// Falls back to a temporary current-thread runtime in the fork window.
    fn block_on<F: std::future::Future>(&self, f: F) -> Result<F::Output, io::Error> {
        let runtime = match self.runtime.lock_or_panic().as_ref() {
            None => Arc::new(Builder::new_current_thread().enable_all().build()?),
            Some(runtime) => runtime.clone(),
        };
        Ok(runtime.block_on(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::Duration;
    use tokio::time::sleep;

    #[derive(Debug)]
    struct TestWorker {
        state: i32,
        sender: Sender<i32>,
    }

    /// Value sent by [`TestWorker::flush`].
    const FLUSH_MARKER: i32 = -2;

    /// Waits until the worker reports a flush.
    fn wait_flush_marker(receiver: &Receiver<i32>) {
        loop {
            let v = receiver
                .recv_timeout(Duration::from_secs(2))
                .expect("worker did not flush");
            if v == FLUSH_MARKER {
                return;
            }
        }
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

        async fn flush(&mut self) {
            let _ = self.sender.send(FLUSH_MARKER);
        }

        async fn shutdown(&mut self) {
            self.state = -1;
            let _ = self.sender.send(self.state);
        }
    }

    /// Worker whose `run` never finishes on its own, like a fetch of an agent that accepts
    /// connections but never answers.
    #[derive(Debug)]
    struct HungWorker;

    #[async_trait]
    impl Worker for HungWorker {
        async fn run(&mut self) {
            sleep(Duration::from_secs(60)).await;
        }

        async fn trigger(&mut self) {}

        fn cancellable_run(&self) -> bool {
            true
        }
    }

    // A fork does not wait for a cancellable `run` to finish.
    #[test]
    fn test_before_fork_cancels_cancellable_run() {
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let _ = shared_runtime.spawn_worker(HungWorker, true).unwrap();
        // Let `run` start.
        std::thread::sleep(Duration::from_millis(50));

        let start = std::time::Instant::now();
        shared_runtime.before_fork();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(shared_runtime.after_fork_parent().is_ok());
    }

    #[test]
    fn test_fork_safe_runtime_creation() {
        let shared_runtime = ForkSafeRuntime::new();
        assert!(shared_runtime.is_ok());
    }

    #[test]
    fn test_spawn_worker() {
        let shared_runtime = ForkSafeRuntime::new().unwrap();
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
        let shared_runtime = ForkSafeRuntime::new().unwrap();
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
        let shared_runtime = ForkSafeRuntime::new().unwrap();
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
        let shared_runtime = ForkSafeRuntime::new().unwrap();
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
        let shared_runtime = ForkSafeRuntime::new().unwrap();
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
        let shared_runtime = ForkSafeRuntime::new().unwrap();
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

    #[test]
    fn test_set_fork_restart_drops_worker_without_shutdown() {
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let handle = shared_runtime.spawn_worker(worker, true).unwrap();

        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");

        shared_runtime.before_fork();
        while receiver.try_recv().is_ok() {}

        handle.set_fork_restart(false).unwrap();
        assert!(shared_runtime.after_fork_child().is_ok());

        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);
        assert!(
            receiver.recv_timeout(Duration::from_millis(200)).is_err(),
            "worker should be dropped without running or shutting down in the fork child"
        );
    }

    /// A single `PausableWorker` in `InvalidState` must
    /// not abort the whole restart loop in `after_fork_parent`
    #[test]
    fn after_fork_parent_skips_invalid_state_workers() {
        let runtime = ForkSafeRuntime::new().unwrap();

        let (good, good_rx) = make_test_worker();
        let _ = runtime.spawn_worker(good, true).unwrap();

        // Second worker — we'll corrupt its entry into InvalidState below,
        // simulating a previously-aborted task.
        let (bad, _bad_rx) = make_test_worker();
        let _ = runtime.spawn_worker(bad, true).unwrap();

        good_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("good worker did not run before fork");

        {
            let mut workers = runtime.workers.lock_or_panic();
            workers[1].worker = PausableWorker::InvalidState;
        }

        runtime.before_fork();

        // Drain good worker queue
        while good_rx.try_recv().is_ok() {}

        let result = runtime.after_fork_parent();

        assert!(
            result.is_ok(),
            "after_fork_parent should not bail on a single InvalidState worker"
        );
        assert!(
            good_rx.recv_timeout(Duration::from_secs(1)).is_ok(),
            "good worker should resume after fork even if a peer is InvalidState"
        );
    }

    // `WorkerHandle::flush` runs the worker flush and its token resolves.
    #[test]
    fn test_worker_handle_flush() {
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();
        let handle = shared_runtime.spawn_worker(worker, true).unwrap();
        let token = handle.flush().unwrap();
        shared_runtime
            .block_on_with_timeout(token.wait(), Duration::from_secs(2))
            .unwrap()
            .unwrap();
        wait_flush_marker(&receiver);
    }

    // A flush requested between `before_fork` and `after_fork_parent` runs only after the restart.
    #[test]
    fn test_flush_during_fork_window() {
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();
        let handle = shared_runtime.spawn_worker(worker, true).unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();

        shared_runtime.before_fork();
        let token = handle.flush().unwrap();
        while let Ok(v) = receiver.try_recv() {
            assert_ne!(v, FLUSH_MARKER, "flush must not run while paused");
        }
        shared_runtime.after_fork_parent().unwrap();

        shared_runtime
            .block_on_with_timeout(token.wait(), Duration::from_secs(2))
            .unwrap()
            .unwrap();
        wait_flush_marker(&receiver);
    }

    // Repeated fork cycles leave the worker running.
    #[test]
    fn test_repeated_fork_cycles_keep_running() {
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();
        let _handle = shared_runtime.spawn_worker(worker, true).unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        for _ in 0..5 {
            shared_runtime.before_fork();
            shared_runtime.after_fork_parent().unwrap();
        }
        while receiver.try_recv().is_ok() {}
        let a = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        let b = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(b, a + 1);
    }

    // Flushing a stopped worker returns `AlreadyStopped`.
    #[test]
    fn test_flush_after_stop() {
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, _receiver) = make_test_worker();
        let handle = shared_runtime.spawn_worker(worker, true).unwrap();
        let other = handle.clone();
        shared_runtime.block_on(handle.stop()).unwrap().unwrap();
        assert!(matches!(
            other.flush(),
            Err(crate::WorkerHandleError::AlreadyStopped)
        ));
    }

    /// Discard and shutdown never touch the registry after awaiting a retirement, so a finished
    /// retirement stays registered until the next discard, fork or shutdown unregisters it.
    fn assert_single_retired(workers: &Mutex<Vec<WorkerEntry>>) {
        let workers = workers.lock_or_panic();
        assert_eq!(workers.len(), 1);
        assert!(workers[0].worker.is_retired());
    }

    // Discarding retires the worker without running its shutdown logic.
    #[test]
    fn test_worker_handle_discard() {
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();
        let handle = shared_runtime.spawn_worker(worker, true).unwrap();
        let other = handle.clone();
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");

        shared_runtime.block_on(handle.discard()).unwrap().unwrap();

        assert_single_retired(&shared_runtime.workers);
        assert!(!receiver.try_iter().any(|v| v == -1 || v == FLUSH_MARKER));
        assert!(matches!(
            other.flush(),
            Err(crate::WorkerHandleError::AlreadyStopped)
        ));
        assert!(matches!(
            shared_runtime.block_on(other.discard()).unwrap(),
            Err(crate::WorkerHandleError::AlreadyStopped)
        ));
        // The repeated discard unregistered the retired entry.
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);
    }

    #[derive(Debug)]
    struct PendingWorker {
        started: Sender<()>,
    }

    #[async_trait]
    impl Worker for PendingWorker {
        async fn run(&mut self) {
            let _ = self.started.send(());
            std::future::pending::<()>().await;
        }
        async fn trigger(&mut self) {}
    }

    #[test]
    fn claimed_discard_is_honored_without_polling_on_shutdown_and_fork() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[derive(Debug)]
        struct CountWorker {
            started: Sender<()>,
            runs: Arc<AtomicUsize>,
            shutdowns: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl Worker for CountWorker {
            async fn run(&mut self) {
                self.runs.fetch_add(1, Ordering::SeqCst);
                let _ = self.started.send(());
                std::future::pending::<()>().await;
            }
            async fn trigger(&mut self) {}
            async fn shutdown(&mut self) {
                self.shutdowns.fetch_add(1, Ordering::SeqCst);
            }
        }

        for fork in ["none", "parent", "child"] {
            let runtime = ForkSafeRuntime::new().unwrap();
            let (started, receiver) = channel();
            let runs = Arc::new(AtomicUsize::new(0));
            let shutdowns = Arc::new(AtomicUsize::new(0));
            let handle = runtime
                .spawn_worker(
                    CountWorker {
                        started,
                        runs: runs.clone(),
                        shutdowns: shutdowns.clone(),
                    },
                    true,
                )
                .unwrap();
            receiver.recv_timeout(Duration::from_secs(1)).unwrap();
            handle.claim_discard().unwrap();
            assert!(handle.flush().is_err());
            assert!(runtime.block_on(handle.clone().stop()).unwrap().is_err());
            // No discard future is constructed or polled here.
            if fork != "none" {
                runtime.before_fork();
                if fork == "parent" {
                    runtime.after_fork_parent().unwrap();
                } else {
                    runtime.after_fork_child().unwrap();
                }
            }
            runtime
                .block_on_with_timeout(runtime.shutdown_async(), Duration::from_secs(1))
                .unwrap();
            assert_eq!(runs.load(Ordering::SeqCst), 1);
            assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn claim_during_fork_window_prevents_restart_and_shutdown() {
        for child in [false, true] {
            let runtime = ForkSafeRuntime::new().unwrap();
            let (worker, receiver) = make_test_worker();
            let handle = runtime.spawn_worker(worker, true).unwrap();
            receiver.recv_timeout(Duration::from_secs(1)).unwrap();
            runtime.before_fork();
            while receiver.try_recv().is_ok() {}
            handle.claim_discard().unwrap();
            if child {
                runtime.after_fork_child().unwrap();
            } else {
                runtime.after_fork_parent().unwrap();
            }
            runtime
                .block_on_with_timeout(runtime.shutdown_async(), Duration::from_secs(1))
                .unwrap();
            assert!(
                receiver.try_recv().is_err(),
                "claimed worker restarted or shut down"
            );
        }
    }

    #[test]
    fn test_discard_cancels_pending_run_within_budget() {
        let runtime = ForkSafeRuntime::new().unwrap();
        let (started, receiver) = channel();
        let handle = runtime
            .spawn_worker(PendingWorker { started }, true)
            .unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        runtime
            .block_on_with_timeout(handle.discard(), Duration::from_secs(1))
            .expect("discard waited for a pending run")
            .unwrap();
        assert_single_retired(&runtime.workers);
    }

    #[test]
    fn test_discard_failed_task_releases_registry_ownership() {
        #[derive(Debug)]
        struct FailedWorker {
            dropped: Sender<()>,
        }
        impl Drop for FailedWorker {
            fn drop(&mut self) {
                let _ = self.dropped.send(());
            }
        }
        #[async_trait]
        impl Worker for FailedWorker {
            async fn trigger(&mut self) {}
            async fn run(&mut self) {
                panic!("simulated worker failure");
            }
        }
        let runtime = ForkSafeRuntime::new().unwrap();
        let (dropped, receiver) = channel();
        let handle = runtime
            .spawn_worker(FailedWorker { dropped }, true)
            .unwrap();
        receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(matches!(
            runtime.block_on(handle.discard()).unwrap(),
            Err(crate::WorkerHandleError::WorkerError(
                super::super::pausable_worker::PausableWorkerError::TaskAborted
            ))
        ));
        assert_single_retired(&runtime.workers);
        // The failed worker is not leaked: the next shutdown unregisters it.
        runtime.block_on(runtime.shutdown_async()).unwrap();
        assert!(runtime.workers.lock_or_panic().is_empty());
    }

    // Hold destruction open to deterministically cancel the caller's wait before retirement
    // completes. No worker is allowed to escape fork/shutdown tracking during that interval.
    fn abandoned_discard(action: &str) {
        #[derive(Debug)]
        struct GatedDrop {
            started: Sender<()>,
            dropping: Sender<()>,
            release: std::sync::Mutex<Receiver<()>>,
            dropped: Sender<()>,
        }
        impl Drop for GatedDrop {
            fn drop(&mut self) {
                let _ = self.dropping.send(());
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5));
                let _ = self.dropped.send(());
            }
        }
        #[async_trait]
        impl Worker for GatedDrop {
            async fn run(&mut self) {
                let _ = self.started.send(());
                std::future::pending::<()>().await;
            }
            async fn trigger(&mut self) {}
            async fn shutdown(&mut self) {
                panic!("discarded worker must never shut down");
            }
        }
        let runtime = ForkSafeRuntime::new().unwrap();
        let retained_runtime = runtime.runtime.lock_or_panic().as_ref().unwrap().clone();
        let (started, started_rx) = channel();
        let (dropping, dropping_rx) = channel();
        let (release, release_rx) = channel();
        let (dropped, dropped_rx) = channel();
        let handle = runtime
            .spawn_worker(
                GatedDrop {
                    started,
                    dropping,
                    release: std::sync::Mutex::new(release_rx),
                    dropped,
                },
                true,
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let other = handle.clone();
        // Poll once, request cancellation, then abandon the caller's future.
        let mut discard = Box::pin(handle.discard());
        let mut context = std::task::Context::from_waker(futures::task::noop_waker_ref());
        assert!(std::future::Future::poll(discard.as_mut(), &mut context).is_pending());
        drop(discard);
        dropping_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(runtime.workers.lock_or_panic().len(), 1);
        assert!(matches!(
            other.flush(),
            Err(crate::WorkerHandleError::AlreadyStopped)
        ));
        assert!(matches!(
            other.set_fork_restart(true),
            Err(crate::WorkerHandleError::AlreadyStopped)
        ));
        assert!(matches!(
            runtime.block_on(other.clone().stop()).unwrap(),
            Err(crate::WorkerHandleError::AlreadyStopped)
        ));

        if action == "retry" || action == "cancelled_shutdown" {
            if action == "cancelled_shutdown" {
                // Cancelling shutdown must not drop the retirement it was waiting for.
                let mut shutdown = Box::pin(runtime.shutdown_async());
                assert!(std::future::Future::poll(shutdown.as_mut(), &mut context).is_pending());
                drop(shutdown);
                assert_eq!(runtime.workers.lock_or_panic().len(), 1);
            }
            let mut retry = Box::pin(other.discard());
            assert!(std::future::Future::poll(retry.as_mut(), &mut context).is_pending());
            release.send(()).unwrap();
            runtime
                .block_on_with_timeout(retry, Duration::from_secs(1))
                .unwrap()
                .unwrap();
            dropped_rx
                .try_recv()
                .expect("retry returned before worker destruction");
        } else {
            std::thread::scope(|scope| {
                let (finished, finished_rx) = channel();
                let (hook_started, hook_started_rx) = channel();
                let runtime_ref = &runtime;
                scope.spawn(move || {
                    hook_started.send(()).unwrap();
                    match action {
                        "fork" => runtime_ref.before_fork(),
                        "shutdown" => runtime_ref.block_on(runtime_ref.shutdown_async()).unwrap(),
                        _ => unreachable!(),
                    }
                    // Retirement must finish even with another Arc keeping the executor alive.
                    dropped_rx
                        .try_recv()
                        .expect("lifecycle hook returned before worker destruction");
                    finished.send(()).unwrap();
                });
                hook_started_rx
                    .recv_timeout(Duration::from_secs(1))
                    .expect("lifecycle hook did not start");
                if action == "fork" {
                    // before_fork holds the registry lock while it waits for retirement.
                    let deadline = std::time::Instant::now() + Duration::from_secs(1);
                    while !matches!(
                        runtime.workers.try_lock(),
                        Err(std::sync::TryLockError::WouldBlock)
                    ) {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "lifecycle hook did not enter retirement"
                        );
                        std::thread::yield_now();
                    }
                } else {
                    // Shutdown leaves the discarding entry registered, so its wait is not
                    // visible in the registry. Give it time to reach the wait.
                    std::thread::sleep(Duration::from_millis(50));
                }
                assert!(finished_rx.try_recv().is_err());
                release.send(()).unwrap();
                finished_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            });
        }
        if action == "fork" {
            // The retired entry is unregistered, not restarted.
            runtime.after_fork_parent().unwrap();
            assert!(runtime.workers.lock_or_panic().is_empty());
            runtime.before_fork();
            runtime.after_fork_child().unwrap();
            assert!(runtime.workers.lock_or_panic().is_empty());
        } else {
            assert_single_retired(&runtime.workers);
            // The next shutdown unregisters the retired entry.
            runtime.block_on(runtime.shutdown_async()).unwrap();
            assert!(runtime.workers.lock_or_panic().is_empty());
        }
        drop(retained_runtime);
    }

    #[test]
    fn test_before_fork_finishes_abandoned_discard() {
        abandoned_discard("fork");
    }
    #[test]
    fn test_shutdown_finishes_abandoned_discard() {
        abandoned_discard("shutdown");
    }
    #[test]
    fn test_retry_finishes_abandoned_discard() {
        abandoned_discard("retry");
    }
    #[test]
    fn test_retry_finishes_discard_after_cancelled_shutdown() {
        abandoned_discard("cancelled_shutdown");
    }

    // Discard between before_fork and after_fork_parent, while the runtime is gone, drops the
    // paused worker without shutting it down, and the parent does not restart it.
    #[test]
    fn test_discard_during_fork_window() {
        let runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();
        let handle = runtime.spawn_worker(worker, true).unwrap();
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");
        runtime.before_fork();
        assert!(runtime.runtime.lock_or_panic().is_none());

        // block_on falls back to a temporary runtime in the fork window.
        runtime.block_on(handle.discard()).unwrap().unwrap();
        assert_single_retired(&runtime.workers);
        // after_fork_parent unregisters the retired entry instead of restarting it.
        runtime.after_fork_parent().unwrap();
        assert!(runtime.workers.lock_or_panic().is_empty());

        // The worker owned the only sender, so the channel disconnects once it is dropped.
        loop {
            match receiver.recv_timeout(Duration::from_secs(1)) {
                Ok(v) => assert!(v != -1 && v != FLUSH_MARKER, "worker shut down or flushed"),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => panic!("worker still alive"),
            }
        }
    }

    /// Polls `inner` once, then holds every later poll until `go` is signalled.
    struct GateAfterFirstPoll<F> {
        inner: std::pin::Pin<Box<F>>,
        polled: Option<Sender<()>>,
        go: Receiver<()>,
    }

    impl<F: std::future::Future> std::future::Future for GateAfterFirstPoll<F> {
        type Output = F::Output;
        fn poll(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<F::Output> {
            let this = self.get_mut();
            if let Some(polled) = this.polled.take() {
                let poll = this.inner.as_mut().poll(cx);
                let _ = polled.send(());
                return poll;
            }
            let _ = this.go.recv_timeout(Duration::from_secs(5));
            this.inner.as_mut().poll(cx)
        }
    }

    // A thread blocked in discard is woken when before_fork finishes the retirement. It must
    // not modify the registry once before_fork has released it: the host calls fork() right
    // then, and a lock held at that instant would deadlock after_fork_child. An already-retired
    // bystander entry detects any post-await unregistering, not only of the waiter's own entry.
    #[test]
    fn test_live_discard_waiter_does_not_modify_registry_after_before_fork() {
        #[derive(Debug)]
        struct GatedDrop {
            started: Sender<()>,
            dropping: Sender<()>,
            release: Mutex<Receiver<()>>,
        }
        impl Drop for GatedDrop {
            fn drop(&mut self) {
                let _ = self.dropping.send(());
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5));
            }
        }
        #[async_trait]
        impl Worker for GatedDrop {
            async fn run(&mut self) {
                let _ = self.started.send(());
                std::future::pending::<()>().await;
            }
            async fn trigger(&mut self) {}
        }

        // GatedDrop's destructor blocks one tokio worker thread; the other runs the bystander.
        let runtime = ForkSafeRuntime::with_worker_threads(2).unwrap();
        let (started, started_rx) = channel();
        let (dropping, dropping_rx) = channel();
        let (release, release_rx) = channel();
        let handle = runtime
            .spawn_worker(
                GatedDrop {
                    started,
                    dropping,
                    release: Mutex::new(release_rx),
                },
                true,
            )
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let worker_id = handle.worker_id;

        let (polled, polled_rx) = channel();
        let (go, go_rx) = channel();
        std::thread::scope(|scope| {
            let runtime_ref = &runtime;
            // The live waiter: blocked in discard on its own thread.
            let waiter = scope.spawn(move || {
                runtime_ref.block_on(GateAfterFirstPoll {
                    inner: Box::pin(handle.discard()),
                    polled: Some(polled),
                    go: go_rx,
                })
            });
            polled_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            dropping_rx.recv_timeout(Duration::from_secs(1)).unwrap();

            // Retire a bystander after the waiter's first poll, so only post-await code could
            // unregister it.
            let (bystander_started, bystander_rx) = channel();
            let bystander = runtime
                .spawn_worker(
                    PendingWorker {
                        started: bystander_started,
                    },
                    true,
                )
                .unwrap();
            bystander_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            runtime
                .block_on_with_timeout(bystander.discard(), Duration::from_secs(1))
                .unwrap()
                .unwrap();

            let fork = scope.spawn(move || runtime_ref.before_fork());
            // before_fork holds the registry lock while it waits for the retirement.
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            while !matches!(
                runtime.workers.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "before_fork did not wait for the retirement"
                );
                std::thread::yield_now();
            }
            release.send(()).unwrap();
            fork.join().unwrap();

            // before_fork returned and released the lock: the host would call fork() now.
            // Let the woken waiter finish its discard in that window.
            go.send(()).unwrap();
            waiter.join().unwrap().unwrap().unwrap();
        });

        // The waiter left both retired entries for the fork hooks to unregister.
        {
            let workers = runtime.workers.lock_or_panic();
            assert_eq!(workers.len(), 2);
            assert!(workers.iter().any(|entry| entry.id == worker_id));
            assert!(workers.iter().all(|entry| entry.worker.is_retired()));
        }
        runtime.after_fork_child().unwrap();
        assert!(runtime.workers.lock_or_panic().is_empty());
    }

    // A claim stops a running worker before its next run, even if no discard is ever polled.
    #[test]
    fn test_claim_stops_running_worker_without_polling() {
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();
        let handle = shared_runtime.spawn_worker(worker, false).unwrap();
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)), Ok(0));

        handle.claim_discard().unwrap();
        // TestWorker runs every 100 ms, so an ignored claim keeps sending.
        assert_eq!(
            receiver.recv_timeout(Duration::from_millis(500)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );

        // Shutdown destroys the worker without running its shutdown logic.
        shared_runtime.shutdown(None).unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
        );
    }

    // A flush queued before the claim is aborted instead of running.
    #[test]
    fn test_claim_aborts_flush_queued_before_claim() {
        #[derive(Debug)]
        struct GatedRun {
            events: Sender<&'static str>,
            release: Arc<tokio::sync::Notify>,
        }
        #[async_trait]
        impl Worker for GatedRun {
            async fn trigger(&mut self) {
                sleep(Duration::from_millis(10)).await;
            }
            async fn run(&mut self) {
                let _ = self.events.send("run");
                self.release.notified().await;
            }
            async fn flush(&mut self) {
                let _ = self.events.send("flush");
            }
            async fn shutdown(&mut self) {
                let _ = self.events.send("shutdown");
            }
        }

        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (events, events_rx) = channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let handle = shared_runtime
            .spawn_worker(
                GatedRun {
                    events,
                    release: release.clone(),
                },
                false,
            )
            .unwrap();
        assert_eq!(events_rx.recv_timeout(Duration::from_secs(2)), Ok("run"));

        // The run is blocked, so the flush stays queued until after the claim.
        let token = handle.flush().unwrap();
        handle.claim_discard().unwrap();
        release.notify_one();

        assert!(matches!(
            shared_runtime
                .block_on_with_timeout(token.wait(), Duration::from_secs(2))
                .unwrap(),
            Err(crate::WorkerHandleError::FlushAborted)
        ));
        assert_eq!(
            events_rx.recv_timeout(Duration::from_millis(300)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
    }

    // A claim made in the fork window is honored by after_fork_* destroying the paused worker
    // right away, not when a later discard or shutdown polls its completion.
    #[test]
    fn test_after_fork_destroys_worker_claimed_in_fork_window() {
        #[derive(Debug)]
        struct DropEvent(Sender<&'static str>);
        impl Drop for DropEvent {
            fn drop(&mut self) {
                let _ = self.0.send("dropped");
            }
        }
        #[async_trait]
        impl Worker for DropEvent {
            async fn trigger(&mut self) {
                sleep(Duration::from_millis(10)).await;
            }
            async fn run(&mut self) {}
            async fn shutdown(&mut self) {
                let _ = self.0.send("shutdown");
            }
        }

        for child in [false, true] {
            let shared_runtime = ForkSafeRuntime::new().unwrap();
            let (events, events_rx) = channel();
            let handle = shared_runtime
                .spawn_worker(DropEvent(events), true)
                .unwrap();

            shared_runtime.before_fork();
            handle.claim_discard().unwrap();
            if child {
                shared_runtime.after_fork_child().unwrap();
            } else {
                shared_runtime.after_fork_parent().unwrap();
            }

            assert_eq!(events_rx.try_recv(), Ok("dropped"), "child: {child}");
            assert_eq!(
                events_rx.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Disconnected),
                "child: {child}"
            );
        }
    }

    // A worker destructor that panics during discard fails that discard and leaves the
    // registry usable for every other worker.
    #[test]
    fn test_panicking_destructor_does_not_poison_registry() {
        #[derive(Debug)]
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("worker destructor panicked");
            }
        }
        #[async_trait]
        impl Worker for PanicOnDrop {
            async fn trigger(&mut self) {
                sleep(Duration::from_millis(10)).await;
            }
            async fn run(&mut self) {}
        }

        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let doomed = shared_runtime.spawn_worker(PanicOnDrop, false).unwrap();
        let (worker, receiver) = make_test_worker();
        let other = shared_runtime.spawn_worker(worker, false).unwrap();

        // Paused workers are dropped inside the discard completion.
        shared_runtime.before_fork();
        assert!(matches!(
            shared_runtime.block_on(doomed.discard()).unwrap(),
            Err(crate::WorkerHandleError::WorkerError(
                super::super::pausable_worker::PausableWorkerError::TaskAborted
            ))
        ));
        // after_fork_parent checks retirement with the registry lock held.
        shared_runtime.after_fork_parent().unwrap();
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 1);

        let token = other.flush().unwrap();
        shared_runtime
            .block_on_with_timeout(token.wait(), Duration::from_secs(2))
            .unwrap()
            .unwrap();
        wait_flush_marker(&receiver);
    }
}
