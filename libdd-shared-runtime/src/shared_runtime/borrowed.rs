// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::pausable_worker::{tokio_spawn_fn, PausableWorker};
use super::{BoxedWorker, SharedRuntime, SharedRuntimeError, WorkerEntry, WorkerHandle};
use crate::worker::Worker;
use futures::stream::{FuturesUnordered, StreamExt};
use libdd_common::MutexExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{debug, error, warn};

/// A [`SharedRuntime`] that exposes a caller-provided `tokio::runtime::Runtime`.
///
/// Holds an `Arc<Runtime>` so the runtime stays alive as long as this struct does,
/// even if the original owner drops their clone. The runtime itself is not affected
/// by operations on this struct — only the workers tracked here are. Borrowed-mode
/// is **not** fork-safe and does not provide a synchronous shutdown.
#[derive(Debug)]
pub struct BorrowedSharedRuntime {
    runtime: Arc<tokio::runtime::Runtime>,
    workers: Arc<Mutex<Vec<WorkerEntry>>>,
    next_worker_id: AtomicU64,
}

impl BorrowedSharedRuntime {
    pub fn from_runtime(runtime: Arc<tokio::runtime::Runtime>) -> Self {
        Self {
            runtime,
            workers: Arc::new(Mutex::new(Vec::new())),
            next_worker_id: AtomicU64::new(1),
        }
    }
}

impl SharedRuntime for BorrowedSharedRuntime {
    fn spawn_worker<T: Worker + Sync + 'static>(
        &self,
        worker: T,
        restart_on_fork: bool,
    ) -> Result<WorkerHandle, SharedRuntimeError> {
        if restart_on_fork {
            warn!(
                "restart_on_fork is ignored on BorrowedSharedRuntime: borrowed mode is not \
                 fork-safe; the outer runtime owner is responsible for fork handling"
            );
        }

        let boxed_worker: BoxedWorker = Box::new(worker);
        debug!(?boxed_worker, "Spawning worker on BorrowedSharedRuntime");
        let mut pausable_worker = PausableWorker::new(boxed_worker);

        pausable_worker.start(tokio_spawn_fn(self.runtime.handle()))?;

        let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);

        self.workers.lock_or_panic().push(WorkerEntry {
            id: worker_id,
            restart_on_fork: false,
            worker: pausable_worker,
        });

        Ok(WorkerHandle {
            worker_id,
            workers: self.workers.clone(),
        })
    }

    async fn shutdown_async(&self) {
        debug!("Shutting down all workers on BorrowedSharedRuntime");
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
                .expect("failed to build outer runtime for borrowed test"),
        )
    }

    #[test]
    fn test_borrowed_spawn_worker_ignores_restart_on_fork() {
        let rt = new_outer_runtime();
        let borrowed = BorrowedSharedRuntime::from_runtime(rt);
        let (worker, receiver) = make_test_worker();

        let _handle = borrowed
            .spawn_worker(worker, true)
            .expect("restart_on_fork=true should be silently ignored in borrowed mode");

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run when restart_on_fork was passed"),
            0
        );
    }

    #[test]
    fn test_borrowed_spawn_worker_runs() {
        let rt = new_outer_runtime();
        let borrowed = BorrowedSharedRuntime::from_runtime(rt);
        let (worker, receiver) = make_test_worker();

        let _handle = borrowed.spawn_worker(worker, false).unwrap();

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run on borrowed runtime"),
            0
        );
    }

    #[test]
    fn test_borrowed_shutdown_async_stops_workers_only() {
        let rt = new_outer_runtime();
        let borrowed = BorrowedSharedRuntime::from_runtime(rt.clone());
        let (worker, receiver) = make_test_worker();

        let _handle = borrowed.spawn_worker(worker, false).unwrap();
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run before shutdown");

        rt.block_on(borrowed.shutdown_async());

        // The shutdown sentinel from Worker::shutdown.
        let mut last = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown did not send a value");
        while let Ok(v) = receiver.try_recv() {
            last = v;
        }
        assert_eq!(last, -1);
        assert_eq!(borrowed.workers.lock_or_panic().len(), 0);

        // The outer runtime must still be usable after shutdown_async.
        rt.block_on(async { sleep(Duration::from_millis(10)).await });
    }

    #[test]
    fn test_borrowed_keeps_runtime_alive_after_caller_drops() {
        let rt = new_outer_runtime();
        let borrowed = BorrowedSharedRuntime::from_runtime(rt.clone());
        // Caller drops their clone; the Arc inside BorrowedSharedRuntime
        // must keep the runtime alive so spawn_worker still works.
        drop(rt);
        let (worker, receiver) = make_test_worker();

        let _handle = borrowed.spawn_worker(worker, false).unwrap();

        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("worker did not run after caller dropped its runtime clone"),
            0
        );
    }
}
