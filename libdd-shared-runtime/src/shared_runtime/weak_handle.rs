// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::pausable_worker::{PausableWorker, tokio_spawn_fn};
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

/// Non-fork-safe [`SharedRuntime`] spawning workers on a tokio runtime it does **not** own,
/// through a [`tokio::runtime::Handle`] (e.g. `Handle::current()` inside a tokio context).
///
/// # Lifetime
/// The caller is responsible for keeping the runtime behind the handle alive until this
/// runtime has been shut down ([`SharedRuntime::shutdown_async`]) or every worker has been
/// stopped. If that runtime is dropped first, spawned workers are cancelled without running
/// their shutdown logic, and later spawns fail.
///
/// # `block_on`
/// [`BlockingRuntime::block_on`] uses [`tokio::runtime::Handle::block_on`]: it panics when called
/// from within an async context, and on a current-thread runtime workers only make progress
/// while that runtime is driven by another thread.
///
/// Use it when a component already runs inside a runtime whose lifetime it does not control
/// (e.g. an application `main` runtime); otherwise prefer [`crate::BasicRuntime`].
#[derive(Debug)]
pub struct WeakHandleRuntime {
    handle: tokio::runtime::Handle,
    workers: Arc<Mutex<Vec<WorkerEntry>>>,
    next_worker_id: AtomicU64,
}

impl WeakHandleRuntime {
    /// Creates a runtime spawning workers through `handle`. See the type docs for the lifetime
    /// requirements on the underlying runtime.
    pub fn from_handle(handle: tokio::runtime::Handle) -> Self {
        Self {
            handle,
            workers: Arc::new(Mutex::new(Vec::new())),
            next_worker_id: AtomicU64::new(1),
        }
    }
}

impl SharedRuntime for WeakHandleRuntime {
    /// Wraps the ambient runtime (`Handle::try_current`); errors outside a tokio context.
    fn new() -> Result<Self, SharedRuntimeError> {
        tokio::runtime::Handle::try_current()
            .map(Self::from_handle)
            .map_err(|_| SharedRuntimeError::RuntimeUnavailable)
    }

    fn spawn_worker<T: Worker + 'static>(
        &self,
        worker: T,
        restart_on_fork: bool,
    ) -> Result<WorkerHandle, SharedRuntimeError> {
        if restart_on_fork {
            warn!("restart_on_fork is ignored on WeakHandleRuntime: it is not fork-safe");
        }

        let boxed_worker: BoxedWorker = Box::new(worker);
        debug!(?boxed_worker, "Spawning worker on WeakHandleRuntime");
        let mut pausable_worker = PausableWorker::new(boxed_worker);

        // Hold the lock across start+push so a concurrent shutdown_async cannot miss it.
        let mut workers_guard = self.workers.lock_or_panic();
        pausable_worker.start(tokio_spawn_fn(&self.handle))?;
        let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);
        workers_guard.push(WorkerEntry {
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
        debug!("Shutting down all workers on WeakHandleRuntime");
        let workers = std::mem::take(&mut *self.workers.lock_or_panic());

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

impl BlockingRuntime for WeakHandleRuntime {
    fn block_on<F: std::future::Future>(&self, f: F) -> Result<F::Output, io::Error> {
        Ok(self.handle.block_on(f))
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

    fn last_value(receiver: &Receiver<i32>) -> i32 {
        let mut last = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        while let Ok(v) = receiver.try_recv() {
            last = v;
        }
        last
    }

    #[test]
    fn multi_thread_spawn_and_shutdown() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let shared_runtime = WeakHandleRuntime::from_handle(rt.handle().clone());
        let (worker, receiver) = make_test_worker();

        let _handle = shared_runtime.spawn_worker(worker, false).unwrap();
        assert_eq!(receiver.recv_timeout(Duration::from_secs(1)).unwrap(), 0);

        shared_runtime
            .block_on(shared_runtime.shutdown_async())
            .unwrap();
        assert_eq!(last_value(&receiver), -1);
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);
    }

    #[tokio::test]
    async fn current_thread_spawn_and_shutdown() {
        let shared_runtime = WeakHandleRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let _handle = shared_runtime.spawn_worker(worker, false).unwrap();
        // Yield until the worker ran once (its trigger sleeps 100ms first).
        let first = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(v) = receiver.try_recv() {
                    return v;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(first, 0);

        shared_runtime.shutdown_async().await;
        assert_eq!(last_value(&receiver), -1);
        assert_eq!(shared_runtime.workers.lock_or_panic().len(), 0);
    }

    #[test]
    fn new_outside_runtime_errors() {
        assert!(matches!(
            WeakHandleRuntime::new(),
            Err(SharedRuntimeError::RuntimeUnavailable)
        ));
    }
}
