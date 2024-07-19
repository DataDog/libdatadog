// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module implements a crashtracker based on catching UNIX signals and
//! uploading the result to the backend.
//!
//! Architecturally, it consists of two parts:
//! 1. A signal handler, which catches a UNIX signal (SIGSEGV, SIGBUS, SIGABRT)
//!    associated with a crash, and and collects information about the state of
//!    the program at crash time.  The signal handler runs under a constrained
//!    environment where many standard operations are illegal.
//!    https://man7.org/linux/man-pages/man7/signal-safety.7.html
//!    In particular, memory allocation, and synchronization such as mutexes are
//!    potentially UB.  The signal handler therefore does as little as possible
//!    in process, and instead writes data across a pipe to a separate receiver
//!    process.
//!    The signal handler then restores the previous signal handler, and waits
//!    for the receiver process to exit.  Keeping the crashing process alive
//!    until the receiver has completed increases the chances that the container
//!    will survive long enough to upload the report; otherwise, there is a
//!    chance that the container will be killed when the crashing process dies
//!    and no telemetry will get out.
//!    Once the receiver has completed, the crash-handler returns, allowing the
//!    previous crash handler (if any) to execute, maintaining the customer
//!    experience as much as possible.
//! 2. The receiver process runs in the background, listening on `stdin`, which is connected by a
//!    pipe to the parent process.  When a crash occurs, the receiver gathers the information from
//!    the pipe, adds additional data about the system state (e.g. /proc/cpuinfo and /proc/meminfo),
//!    formats it into a crash report, uploads it to the backend, and then exits. The receiver also
//!    exits if the pipe is closed without a crash report, to avoid leaving a zombie process if the
//!    parent exits normally.
//!
//! Data collected:
//! 1. The data collected by the crash-handler includes:
//!    1. The signal type leading to the crash
//!    2. The stacktrace at time of crash (for the crashing thread). Depending on a flag, this can
//!       either be resolved, or raw addresses. Resolving addresses provide more data, but sometimes
//!       crashes the crash handler (ironic).
//!    3. System level info (e.g. /proc/self/maps).
//!    4. The result of counters describing the current state of the profiler.
//! 2. Data augmented by the receiver includes:
//!    1. Metadata provided by the caller (e.g. library & profiler versions).
//!    2. System info: OS version, /proc/cpuinfo /proc/meminfo, etc.
//!    3. A timestamp and GUID for tracking the crash report.
//!
//! Handling of forks
//! Safety issues

mod api;
mod collector;
mod configuration;
mod constants;
mod crash_info;
mod receiver;
mod telemetry;

#[cfg(unix)]
pub use api::*;
pub use configuration::{
    CrashtrackerConfiguration, CrashtrackerReceiverConfig, StacktraceCollection,
};
pub use constants::*;
#[cfg(unix)]
pub use collector::crash_handler::{update_config, update_metadata};
pub use crash_info::*;
#[cfg(unix)]
pub use receiver::{receiver_entry_point_stdin, reciever_entry_point_unix_socket};
pub use collector::{begin_profiling_op, end_profiling_op, reset_counters, ProfilingOpTypes, clear_spans, clear_traces, insert_span, insert_trace, remove_span, remove_trace};
