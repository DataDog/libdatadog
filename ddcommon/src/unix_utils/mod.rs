// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

mod execve;
mod file_ops;
mod fork;
mod process;
mod timeout;

pub use execve::{PreparedExecve, PreparedExecveError};
pub use file_ops::open_file_or_quiet;
pub use fork::alt_fork;
pub use process::wait_for_pollhup;
pub use process::{reap_child_non_blocking, terminate, PollError, ReapError};
pub use timeout::TimeoutManager;
