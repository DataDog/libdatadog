// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::worker::Worker;
use futures::stream::{FuturesUnordered, StreamExt};
use libdd_common::MutexExt;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tracing::{debug, error};

use super::{
    pausable_worker::PausableWorker, BoxedWorker, SharedRuntime, SharedRuntimeError, WorkerEntry,
    WorkerHandle, WorkerRegistry,
};

/// Single-threaded local executor runtime for wasm32.
///
/// Spawns workers via [`wasm_bindgen_futures::spawn_local`]. No fork protocol,
/// no `block_on`, no synchronous shutdown — all entry points are async.
///
/// On native targets use [`crate::ForkSafeRuntime`] (with fork hooks) or
/// [`crate::BasicRuntime`] (caller-provided tokio runtime) instead.
#[derive(Debug)]
pub struct LocalRuntime {
    workers: WorkerRegistry,
    next_worker_id: AtomicU64,
}

impl LocalRuntime {
    fn push_worker(
        &self,
        workers_guard: &mut std::sync::MutexGuard<Vec<WorkerEntry>>,
        pausable_worker: PausableWorker<BoxedWorker>,
    ) -> WorkerHandle {
        let worker_id = self.next_worker_id.fetch_add(1, Ordering::Relaxed);
        workers_guard.push(WorkerEntry {
            id: worker_id,
            worker: pausable_worker,
        });
        WorkerHandle {
            worker_id,
            workers: self.workers.clone(),
        }
    }
}

impl SharedRuntime for LocalRuntime {
    fn new() -> Result<Self, SharedRuntimeError> {
        Ok(Self {
            workers: Rc::new(Mutex::new(Vec::new())),
            next_worker_id: AtomicU64::new(1),
        })
    }

    fn spawn_worker<T: Worker + Sync + 'static>(
        &self,
        worker: T,
        // LocalRuntime has no fork protocol.
        _restart_on_fork: bool,
    ) -> Result<WorkerHandle, SharedRuntimeError> {
        let boxed_worker: BoxedWorker = Box::new(worker);
        debug!(?boxed_worker, "Spawning worker on LocalRuntime");
        let mut pausable_worker = PausableWorker::new(boxed_worker);
        let mut workers_guard = self.workers.lock_or_panic();

        pausable_worker.start(|future| {
            use futures_util::FutureExt;
            let (remote, handle) = future.remote_handle();
            wasm_bindgen_futures::spawn_local(remote);
            Box::pin(async { Ok(handle.await) })
        })?;

        Ok(self.push_worker(&mut workers_guard, pausable_worker))
    }

    async fn shutdown_async(&self) {
        debug!("Shutting down all workers on LocalRuntime");
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
