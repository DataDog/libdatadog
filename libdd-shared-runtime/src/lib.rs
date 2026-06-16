// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! A shared tokio runtime for running background workers across multiple components.
//!
//! This crate provides two implementations of [`SharedRuntime`], distinguished by whether
//! they handle `fork()`:
//!
//! - [`ForkSafeRuntime`] owns a tokio runtime and exposes fork hooks (`before_fork`,
//!   `after_fork_parent`, `after_fork_child`) that pause and restart workers around `fork()` calls,
//!   preventing deadlocks in child processes.
//! - [`BasicRuntime`] is the regular (non-fork-safe) variant. Its internal tokio runtime can be
//!   library-built ([`BasicRuntime::new`] / [`BasicRuntime::with_worker_threads`]) or supplied by
//!   the caller as an `Arc<tokio::runtime::Runtime>` ([`BasicRuntime::from_handle`]).
//!
//! Components such as the trace exporter can share one runtime instead of each creating their
//! own, reducing thread and resource overhead.

pub mod shared_runtime;
pub mod worker;

// Top-level re-exports for convenience
#[cfg(not(target_arch = "wasm32"))]
pub use shared_runtime::BasicRuntime;
pub use shared_runtime::{
    ForkSafeRuntime, SharedRuntime, SharedRuntimeError, WorkerHandle, WorkerHandleError,
};
pub use worker::Worker;
