// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! A shared tokio runtime for running background workers across multiple components.
//!
//! Components such as the trace exporter can share one runtime instead of each creating their
//! own, reducing thread and resource overhead.
//!
//! # Choosing a runtime
//!
//! | Runtime             | Target | Threads | Fork-safe           | `block_on` | When to use                                                                                                                                 |
//! |---------------------|--------|---------|---------------------|------------|---------------------------------------------------------------------------------------------------------------------------------------------|
//! | [`ForkSafeRuntime`] | native | multi   | yes — full protocol | yes        | Default for native code that may run in a forking process (e.g. Ruby, Python runtimes).                                                     |
//! | [`BasicRuntime`]    | native | multi*  | no                  | yes        | Native code where `fork()` is not a concern; optionally share an existing `Arc<tokio::runtime::Runtime>` via [`BasicRuntime::from_handle`]. |
//! | `LocalRuntime`      | wasm32 | single  | n/a                 | no         | WebAssembly; spawns via `wasm_bindgen_futures::spawn_local`.                                                                                |
//!
//! \* [`BasicRuntime::new`] and [`BasicRuntime::with_worker_threads`] build a multi-thread runtime;
//! [`BasicRuntime::from_handle`] accepts any `Arc<tokio::runtime::Runtime>`, including
//! single-thread ones.
//!
//! ## Fork protocol ([`ForkSafeRuntime`] only)
//!
//! Call these around every `fork()` to prevent deadlocks in child processes:
//!
//! 1. [`ForkSafeRuntime::before_fork`] — pauses workers
//! 2. `fork()`
//! 3. parent: [`ForkSafeRuntime::after_fork_parent`] — resumes workers
//! 4. child: [`ForkSafeRuntime::after_fork_child`] — restarts workers on a fresh runtime

//! ## Discarding stale workers
//!
//! Use [`WorkerHandle::discard`] when buffered data must be thrown away rather than flushed.
//! Unlike graceful [`WorkerHandle::stop`], discard cancels in-flight async work, aborts queued
//! flushes, and skips [`Worker::shutdown`]. The worker remains tracked until destruction completes;
//! abandoning the wait does not let it escape fork or shutdown coordination. Another handle can
//! retry discard to await the same retirement. A discarded worker is never restarted.
//!
//! Cancellation is cooperative. Synchronous work and destructors must return promptly; callers
//! on native targets can use [`BlockingRuntime::block_on_with_timeout`] to bound their wait.
//! Requests already sent to the network cannot be recalled.

pub mod shared_runtime;
mod weak_waker;
pub mod worker;

// Top-level re-exports for convenience
#[cfg(not(target_arch = "wasm32"))]
pub use shared_runtime::BasicRuntime;
#[cfg(not(target_arch = "wasm32"))]
pub use shared_runtime::BlockOnTimeoutError;
#[cfg(not(target_arch = "wasm32"))]
pub use shared_runtime::BlockingRuntime;
#[cfg(not(target_arch = "wasm32"))]
pub use shared_runtime::ForkSafeRuntime;
#[cfg(target_arch = "wasm32")]
pub use shared_runtime::LocalRuntime;
pub use shared_runtime::{
    FlushToken, SharedRuntime, SharedRuntimeError, WorkerHandle, WorkerHandleError,
};
pub use worker::Worker;
