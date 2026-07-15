// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::worker::Worker;
use futures::stream::{FuturesUnordered, StreamExt};
use libdd_common::MutexExt;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use tracing::{debug, error};

use super::{
    pausable_worker::{tokio_spawn_fn, PausableWorker},
    BlockingRuntime, BoxedWorker, SharedRuntime, SharedRuntimeError, WorkerEntry, WorkerHandle,
};

fn build_runtime(worker_threads: usize) -> Result<Runtime, io::Error> {
    Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
}

/// Owns a tokio runtime and manages [`PausableWorker`]s on it.
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
        self.restart_runtime()?;

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

    /// Shuts down all workers synchronously. Returns `ShutdownTimedOut` if `timeout` is
    /// exceeded.
    ///
    /// This always drives each worker's shutdown to completion so in-flight work (e.g. a
    /// final trace flush) is not lost. Normally that happens by calling `block_on` on the
    /// current thread. But `block_on` needs a live Tokio context, which requires the
    /// calling thread's CONTEXT thread-local — and during (embedded) interpreter
    /// finalization that thread-local may already be destroyed (see
    /// [`Self::can_block_on`]). The runtime's own worker threads are unaffected by that;
    /// only the *calling* thread's TLS is gone. So in that case we hand the same graceful
    /// shutdown off to a freshly spawned thread, whose TLS was never touched, and just wait
    /// for it to finish (bounded by `timeout`, if given) using plain OS-level
    /// synchronization that doesn't touch Tokio's TLS.
    pub fn shutdown(&self, timeout: Option<Duration>) -> Result<(), SharedRuntimeError> {
        debug!(?timeout, "Shutting down ForkSafeRuntime");
        let Some(runtime) = self.runtime.lock_or_recover().take() else {
            return Ok(());
        };

        if Self::can_block_on() {
            return Self::block_on_shutdown(&runtime, &self.workers, timeout);
        }

        debug!("No live Tokio context (finalization); flushing from a helper thread");
        Self::shutdown_from_helper_thread(runtime, self.workers.clone(), timeout)
    }

    /// Drives `shutdown_workers` to completion via `runtime.block_on` on the calling
    /// thread. Only safe to call when [`Self::can_block_on`] is true.
    fn block_on_shutdown(
        runtime: &Runtime,
        workers: &Arc<Mutex<Vec<WorkerEntry>>>,
        timeout: Option<Duration>,
    ) -> Result<(), SharedRuntimeError> {
        match timeout {
            Some(timeout) => {
                match runtime.block_on(async {
                    tokio::time::timeout(timeout, shutdown_workers(workers)).await
                }) {
                    Ok(()) => Ok(()),
                    Err(_) => Err(SharedRuntimeError::ShutdownTimedOut(timeout)),
                }
            }
            None => {
                runtime.block_on(shutdown_workers(workers));
                Ok(())
            }
        }
    }

    /// Runs [`Self::block_on_shutdown`] on a fresh, detached thread (whose Tokio TLS was
    /// never touched) and blocks the calling thread on its completion via a channel —
    /// plain OS-level synchronization, safe even with the calling thread's Tokio TLS gone.
    ///
    /// A missing `timeout` waits indefinitely for completion, matching the behavior of
    /// [`Self::block_on_shutdown`] itself when called with `None`.
    fn shutdown_from_helper_thread(
        runtime: Arc<Runtime>,
        workers: Arc<Mutex<Vec<WorkerEntry>>>,
        timeout: Option<Duration>,
    ) -> Result<(), SharedRuntimeError> {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = Self::block_on_shutdown(&runtime, &workers, timeout);
            let _ = done_tx.send(result);
        });

        match timeout {
            Some(timeout) => done_rx
                .recv_timeout(timeout)
                .unwrap_or(Err(SharedRuntimeError::ShutdownTimedOut(timeout))),
            None => done_rx.recv().unwrap_or(Ok(())),
        }
    }

    /// Whether the current thread can safely enter a Tokio context (i.e. call `block_on`).
    ///
    /// Returns `false` only when the Tokio CONTEXT thread-local has been *destroyed*,
    /// which happens during interpreter/thread finalization. A merely-missing context
    /// (the normal case for a thread that never entered a runtime) still returns `true`,
    /// because `block_on` establishes its own context in that case.
    fn can_block_on() -> bool {
        !matches!(
            tokio::runtime::Handle::try_current(),
            Err(ref e) if e.is_thread_local_destroyed()
        )
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

impl Drop for ForkSafeRuntime {
    /// Terminal teardown for the owned Tokio runtime.
    ///
    /// A normal `Runtime` drop blocks the current thread to join its worker threads,
    /// and that join path touches the Tokio CONTEXT thread-local. During (embedded)
    /// interpreter finalization — e.g. a uWSGI worker exiting — that thread-local may
    /// already be destroyed, so a normal drop panics with "The Tokio context
    /// thread-local variable has been destroyed" (and, via a poisoned mutex, cascades).
    ///
    /// `shutdown_background` instead signals shutdown and returns immediately, detaching
    /// the worker threads; the OS reclaims them on process exit. This is done
    /// *unconditionally* — we do not try to detect whether we are in finalization —
    /// so there is no ordering-dependent branch that a future teardown scenario can
    /// slip past.
    ///
    /// [`Self::shutdown`] always takes the runtime out (even during finalization, via a
    /// helper thread — see its docs), so this only fires when `shutdown` was never called
    /// at all before drop; that caller-error case is the one scenario this crate can't
    /// flush on behalf of the caller, so it just detaches cleanly instead of risking a
    /// panic.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.lock_or_recover().take() {
            match Arc::try_unwrap(runtime) {
                Ok(runtime) => runtime.shutdown_background(),
                // Another Arc clone is still alive (e.g. an in-flight block_on holds
                // one). We cannot take ownership to detach; drop our reference and let
                // the last owner's drop run. This is not the finalization path.
                Err(_runtime) => {}
            }
        }
    }
}

impl SharedRuntime for ForkSafeRuntime {
    fn new() -> Result<Self, SharedRuntimeError> {
        Self::with_worker_threads(1)
    }

    fn spawn_worker<T: Worker + Sync + 'static>(
        &self,
        worker: T,
        restart_on_fork: bool,
    ) -> Result<WorkerHandle, SharedRuntimeError> {
        let boxed_worker: BoxedWorker = Box::new(worker);
        debug!(?boxed_worker, "Spawning worker on ForkSafeRuntime");
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

    async fn shutdown_async(&self) {
        shutdown_workers(&self.workers).await;
    }
}

/// Drains and gracefully shuts down every registered worker. Standalone (rather than a
/// `&self` method) so it can run identically whether driven from the calling thread
/// ([`ForkSafeRuntime::block_on_shutdown`]) or from the helper thread spawned by
/// [`ForkSafeRuntime::shutdown_from_helper_thread`].
async fn shutdown_workers(workers: &Mutex<Vec<WorkerEntry>>) {
    debug!("Shutting down all workers asynchronously");
    let workers = {
        let mut workers_lock = workers.lock_or_recover();
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

impl BlockingRuntime for ForkSafeRuntime {
    /// Falls back to a temporary current-thread runtime in the fork window.
    fn block_on<F: std::future::Future>(&self, f: F) -> Result<F::Output, io::Error> {
        let runtime = match self.runtime.lock_or_panic().as_ref() {
            None => Arc::new(Builder::new_current_thread().enable_all().build()?),
            Some(runtime) => runtime.clone(),
        };
        Ok(runtime.block_on(f))
    }

    /// Entering a Tokio context, or building a new runtime, touches thread-locals that
    /// panic if the calling thread's TLS is already destroyed (same hazard as
    /// [`Self::shutdown`]). When that's the case, run `f` on a helper thread instead.
    fn block_on_send<F: std::future::Future + Send + 'static>(
        &self,
        f: F,
    ) -> Result<F::Output, io::Error>
    where
        F::Output: Send + 'static,
    {
        if Self::can_block_on() {
            return self.block_on(f);
        }

        debug!("No live Tokio context (finalization); running block_on from a helper thread");
        let runtime = self.runtime.lock_or_recover().clone();
        Self::block_on_from_helper_thread(runtime, f)
    }
}

impl ForkSafeRuntime {
    /// Runs `f` on a fresh thread (whose Tokio TLS was never touched) and blocks the
    /// calling thread on its result via a plain OS channel, safe even with the calling
    /// thread's TLS gone. Mirrors [`Self::shutdown_from_helper_thread`].
    fn block_on_from_helper_thread<F: std::future::Future + Send + 'static>(
        runtime: Option<Arc<Runtime>>,
        f: F,
    ) -> Result<F::Output, io::Error>
    where
        F::Output: Send + 'static,
    {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new().spawn(move || {
            let result = (|| -> Result<F::Output, io::Error> {
                let runtime = match runtime {
                    Some(runtime) => runtime,
                    None => Arc::new(Builder::new_current_thread().enable_all().build()?),
                };
                Ok(runtime.block_on(f))
            })();
            let _ = done_tx.send(result);
        })?;
        done_rx
            .recv()
            .map_err(|_| io::Error::other("block_on helper thread died without a result"))?
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
    fn test_shutdown_is_idempotent() {
        // Calling shutdown() twice must not panic or error. The second call hits the
        // None-guard (runtime already taken). This covers the same early-return path as
        // the TLS-destroyed guard added for CPython atexit finalization ordering.
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let _ = shared_runtime.spawn_worker(worker, true).unwrap();
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");

        assert!(shared_runtime.shutdown(None).is_ok());
        assert!(
            shared_runtime.shutdown(None).is_ok(),
            "second shutdown must not panic"
        );
    }

    #[test]
    fn test_shutdown_from_helper_thread_flushes_workers() {
        // Exercises the destroyed-TLS fallback mechanism directly (this test's thread has
        // a perfectly fine Tokio context, but shutdown_from_helper_thread is the exact
        // code path shutdown() delegates to when the calling thread's context is gone).
        // Confirms the worker's shutdown() -- the "final flush" -- actually runs to
        // completion on the helper thread, not just that teardown avoids a panic.
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let _ = shared_runtime.spawn_worker(worker, true).unwrap();
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");

        let runtime = shared_runtime.runtime.lock_or_recover().take().unwrap();
        let result = ForkSafeRuntime::shutdown_from_helper_thread(
            runtime,
            shared_runtime.workers.clone(),
            Some(Duration::from_secs(1)),
        );
        assert!(result.is_ok());

        let mut last = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("helper thread did not flush worker shutdown");
        while let Ok(v) = receiver.try_recv() {
            last = v;
        }
        assert_eq!(
            last, -1,
            "worker shutdown (the flush) must actually run on the helper thread"
        );
    }

    #[test]
    fn test_drop_without_shutdown_detaches_cleanly() {
        // Dropping a ForkSafeRuntime that still owns a live runtime + worker (i.e.
        // shutdown() was never called) must not panic or hang. Drop routes through
        // shutdown_background(), which detaches worker threads without blocking or
        // entering the Tokio context — the structural terminal-teardown path that
        // makes finalization (destroyed-TLS) drops safe.
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, receiver) = make_test_worker();

        let _ = shared_runtime.spawn_worker(worker, true).unwrap();
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("worker did not run");

        // No explicit shutdown() — just drop. Must return promptly without panicking.
        drop(shared_runtime);
    }

    #[test]
    fn test_lock_recovers_from_poison() {
        // A panic while holding one of the runtime's locks must not cascade into
        // subsequent lifecycle calls (the original PoisonError second-panic). After a
        // poisoning panic, shutdown()/Drop must still succeed.
        use std::panic::{catch_unwind, AssertUnwindSafe};

        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let (worker, _receiver) = make_test_worker();
        let _ = shared_runtime.spawn_worker(worker, true).unwrap();

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = shared_runtime.workers.lock_or_recover();
            panic!("poison the workers mutex while holding it");
        }));

        // The mutex is now poisoned; lock_or_recover must still work rather than panic.
        assert!(
            shared_runtime.shutdown(None).is_ok(),
            "shutdown must recover from a poisoned lock"
        );
    }

    #[test]
    fn test_block_on_send_runs_future_on_calling_thread() {
        // Sanity check for the live-TLS path: block_on_send must still just drive `f` to
        // completion on the calling thread and return its output, matching block_on.
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let result = shared_runtime.block_on_send(async { 21 * 2 }).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_block_on_from_helper_thread_runs_future_and_returns_value() {
        // Exercises the fallback directly; real TLS destruction can't be triggered
        // deterministically from a unit test. Mirrors
        // test_shutdown_from_helper_thread_flushes_workers.
        let shared_runtime = ForkSafeRuntime::new().unwrap();
        let runtime = shared_runtime.runtime.lock_or_recover().clone();

        let result = ForkSafeRuntime::block_on_from_helper_thread(runtime, async { 1 + 1 });
        assert_eq!(result.unwrap(), 2);
    }

    #[test]
    fn test_block_on_from_helper_thread_builds_runtime_when_none() {
        // Same as above but for the (runtime already taken) case block_on's own None
        // branch handles unsafely — the helper thread must build its own fresh runtime
        // instead, since building on the *calling* thread is what panics once TLS is gone.
        let result = ForkSafeRuntime::block_on_from_helper_thread(None, async { 40 + 2 });
        assert_eq!(result.unwrap(), 42);
    }
}
