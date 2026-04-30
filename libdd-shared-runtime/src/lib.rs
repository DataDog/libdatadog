// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! A shared tokio runtime for running background workers across multiple components.
//!
//! This crate provides [`SharedRuntime`], which owns a single tokio runtime and manages
//! [`PausableWorker`]s on it. Components such as the trace exporter can share one runtime
//! instead of each creating their own, reducing thread and resource overhead.
//!
//! [`SharedRuntime`] also provides fork-safety hooks (`before_fork`, `after_fork_parent`,
//! `after_fork_child`) that pause and restart workers around `fork()` calls, preventing
//! deadlocks in child processes.

pub mod shared_runtime;
pub mod worker;

// Top-level re-exports for convenience
pub use shared_runtime::{SharedRuntime, SharedRuntimeError, WorkerHandle, WorkerHandleError};
pub use worker::Worker;
