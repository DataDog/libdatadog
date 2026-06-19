// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! A shared tokio runtime for running background workers across multiple components.
//!
//! This crate provides three implementations of [`SharedRuntime`], distinguished by their
//! threading model and fork-safety guarantees:
//!
//! - [`ForkSafeRuntime`] *(native only)* — owns a multi-thread tokio runtime and exposes the full
//!   fork protocol ([`ForkSafeRuntime::before_fork`] / [`ForkSafeRuntime::after_fork_parent`] /
//!   [`ForkSafeRuntime::after_fork_child`]) that pauses and restarts workers around `fork()` calls,
//!   preventing deadlocks in child processes. Also provides synchronous
//!   [`ForkSafeRuntime::block_on`] and [`ForkSafeRuntime::shutdown`].
//! - [`BasicRuntime`] *(native only)* — the regular (non-fork-safe) variant. Its internal tokio
//!   runtime can be library-built ([`BasicRuntime::new`] / [`BasicRuntime::with_worker_threads`])
//!   or supplied by the caller as an `Arc<tokio::runtime::Runtime>`
//!   ([`BasicRuntime::from_handle`]).
//! - [`LocalRuntime`] *(wasm32 only)* — single-threaded local executor; spawns workers via
//!   `wasm_bindgen_futures::spawn_local`. No fork protocol, no `block_on`, async-only.
//!
//! Components such as the trace exporter can share one runtime instead of each creating their
//! own, reducing thread and resource overhead.

pub mod shared_runtime;
mod weak_waker;
pub mod worker;

// Top-level re-exports for convenience
#[cfg(not(target_arch = "wasm32"))]
pub use shared_runtime::BasicRuntime;
#[cfg(not(target_arch = "wasm32"))]
pub use shared_runtime::BlockingRuntime;
#[cfg(not(target_arch = "wasm32"))]
pub use shared_runtime::ForkSafeRuntime;
#[cfg(target_arch = "wasm32")]
pub use shared_runtime::LocalRuntime;
pub use shared_runtime::{SharedRuntime, SharedRuntimeError, WorkerHandle, WorkerHandleError};
pub use worker::Worker;
