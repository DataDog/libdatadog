// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod worker;

// Top-level re-exports for convenience
pub use worker::pausable_worker::{PausableWorker, PausableWorkerError};
pub use worker::shared_runtime::{SharedRuntime, SharedRuntimeError, WorkerHandle, WorkerHandleError};
pub use worker::Worker;
