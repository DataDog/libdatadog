// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(unix)]

mod execve;
mod file_ops;
#[cfg(not(target_os = "aix"))]
mod fork;
mod process;
mod restricted_file;

pub use execve::{PreparedExecve, PreparedExecveError};
pub use file_ops::open_file_or_quiet;
#[cfg(not(target_os = "aix"))]
pub use fork::alt_fork;
pub use process::wait_for_pollhup;
pub use process::{PollError, ReapError, reap_child_non_blocking, terminate};
pub use restricted_file::{
    RestrictedOpenError, constrained_unix_socket_fd, open_regular_for_append,
    open_regular_for_create, open_regular_for_read, set_restrict_worker_file_outputs,
    worker_file_outputs_restricted,
};
