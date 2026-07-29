// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::pausable_worker::{tokio_spawn_fn, PausableWorker};
use super::{
    BlockingRuntime, BoxedWorker, SharedRuntime, SharedRuntimeError, WorkerEntry, WorkerHandle,
};
use crate::worker::Worker;
use futures::stream::{FuturesUnordered, StreamExt};
use libdd_common::MutexExt;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, warn};

/// Non-fork-safe [`SharedRuntime`] implementation.
///
/// The internal `Arc<tokio::runtime::Runtime>` is either library-built
/// ([`BasicRuntime::new`] / [`BasicRuntime::with_worker_threads`]) or
/// caller-provided ([`BasicRuntime::from_handle`]). The `Arc` keeps the runtime
/// alive for the lifetime of this struct even if the caller drops their clone of
/// the handle.
///
/// This type does **not** implement the fork protocol. If the process forks,
/// use [`crate::ForkSafeRuntime`] instead — or handle the fork at the
/// outer runtime layer that owns the tokio runtime passed in.
#[derive(Debug)]
pub struct BasicRuntime {
    runtime: Arc<tokio::runtime::Runtime>,
    workers: Arc<Mutex<Vec<WorkerEntry>>>,
    next_worker_id: AtomicU64,
}

impl BasicRuntime {
    /// Creates a new `BasicRuntime` backed by a library-built multi-thread tokio runtime
    /// with the given number of worker threads.
    pub fn with_worker_threads(worker_threads: usize) -> Result<Self, SharedRuntimeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(worker_threads)
            .enable_all()
            .build()?;
        Ok(Self::from_handle(Arc::new(runtime)))
    }

    /// Creates a new `BasicRuntime` wrapping a caller-provided runtime.
    ///
    /// The runtime is held via `Arc`, so it stays alive for the lifetime of this struct
    /// even if the caller drops their clone.
    pub fn from_handle(runtime: Arc<tokio::runtime::Runtime>) -> Self {
        Self {
            runtime,
            workers: Arc::new(Mutex::new(Vec::new())),
            next_worker_id: AtomicU64::new(1),
        }
    }

    fn push_worker(
        &self,
        workers_guard: &mut std::sync::MutexGuard<Vec<WorkerEntry>>,
        pausable_worker: PausableWorker<BoxedWorker>,
    ) -> WorkerHandle {
        let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);
        workers_guard.push(WorkerEntry {
            id: worker_id,
            restart_on_fork: false,
            worker: pausable_worker,
        });
        WorkerHandle {
            worker_id,
            workers: self.workers.clone(),
        }
    }
}

impl SharedRuntime for BasicRuntime {
    fn new() -> Result<Self, SharedRuntimeError> {
        Self::with_worker_threads(1)
    }

    fn spawn_worker<T: Worker + Sync + 'static>(
        &self,
        worker: T,
        restart_on_fork: bool,
    ) -> Result<WorkerHandle, SharedRuntimeError> {
        if restart_on_fork {
            warn!(
                "restart_on_fork is ignored on BasicRuntime: regular mode is not fork-safe; \
                 use ForkSafeRuntime if you need fork hooks"
            );
        }

        let boxed_worker: BoxedWorker = Box::new(worker);
        debug!(?boxed_worker, "Spawning worker on BasicRuntime");
        let mut pausable_worker = PausableWorker::new(boxed_worker);

        // Hold the workers lock across start+push so a concurrent shutdown_async cannot
        // drain the registry between starting the task and recording it — which would
        // otherwise leave a live worker behind that shutdown_async never paused.
        let mut workers_guard = self.workers.lock_or_panic();
        pausable_worker.start(tokio_spawn_fn(self.runtime.handle()))?;
        Ok(self.push_worker(&mut workers_guard, pausable_worker))
    }

    async fn shutdown_async(&self) {
        debug!("Shutting down all workers on BasicRuntime");
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

impl BlockingRuntime for BasicRuntime {
    fn block_on<F: std::future::Future>(&self, f: F) -> Result<F::Output, io::Error> {
        Ok(self.runtime.block_on(f))
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

        async fn shutdown(&mut self) {
            self.state = -1;
            let _ = self.sender.send(self.state);
        }
    }

    fn new_outer_runtime() -> Arc<tokio::runtime::Runtime> {
        Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("failed to build outer runtime for BasicRuntime test"),
        )
    }

    #[test]
    fn test_new_lib_built_runtime_spawn_worker_runs() {
        let shared_runtime = BasicRuntime::new().expect("BasicRuntime::new");
        let (worker, receiver) = make_test_worker();

        let _handle = shared_runtime.spawn_worker(worker, false).unwrap();

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run on library-built BasicRuntime"),
            0
        );
    }

    #[test]
    fn test_with_worker_threads_spawn_worker_runs() {
        let shared_runtime =
            BasicRuntime::with_worker_threads(2).expect("BasicRuntime::with_worker_threads");
        let (worker, receiver) = make_test_worker();

        let _handle = shared_runtime.spawn_worker(worker, false).unwrap();

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run on multi-thread BasicRuntime"),
            0
        );
    }

    #[test]
    fn test_spawn_worker_ignores_restart_on_fork() {
        let rt = new_outer_runtime();
        let shared_runtime = BasicRuntime::from_handle(rt);
        let (worker, receiver) = make_test_worker();

        let _handle = shared_runtime
            .spawn_worker(worker, true)
            .expect("restart_on_fork=true should be silently ignored on BasicRuntime");

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run when restart_on_fork was passed"),
            0
        );
    }

    #[test]
    fn test_from_handle_spawn_worker_runs() {
        let rt = new_outer_runtime();
        let shared_runtime = BasicRuntime::from_handle(rt);
        let (worker, receiver) = make_test_worker();

        let _handle = shared_runtime.spawn_worker(worker, false).unwrap();

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run on BasicRuntime"),
            0
        );
    }

    #[test]
    fn test_shutdown_async_stops_workers_only() {
        let rt = new_outer_runtime();
        let shared_runtime = BasicRuntime::from_handle(rt.clone());
        let (worker, receiver) = make_test_worker();

        let _handle = shared_runtime.spawn_worker(worker, false).unwrap();
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run before shutdown");

        rt.block_on(shared_runtime.shutdown_async());

        // The shutdown sentinel from Worker::shutdown.
        let mut last = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown did not send a value");
        while let Ok(v) = receiver.try_recv() {
            last = v;
        }
        assert_eq!(last, -1);
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);

        // The outer runtime must still be usable after shutdown_async.
        rt.block_on(async { sleep(Duration::from_millis(10)).await });
    }

    #[test]
    fn test_keeps_runtime_alive_after_caller_drops() {
        let rt = new_outer_runtime();
        let shared_runtime = BasicRuntime::from_handle(rt.clone());
        // Caller drops their clone; the Arc inside BasicRuntime
        // must keep the runtime alive so spawn_worker still works.
        drop(rt);
        let (worker, receiver) = make_test_worker();

        let _handle = shared_runtime.spawn_worker(worker, false).unwrap();

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run after caller dropped its runtime clone"),
            0
        );
    }
}
