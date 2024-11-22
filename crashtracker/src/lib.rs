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
//!    <https://man7.org/linux/man-pages/man7/signal-safety.7.html>
//!    In particular, memory allocation, and synchronization such as mutexes, are
//!    potentially UB.  The signal handler therefore does as little as possible
//!    in process, and instead writes data across a socket to a separate receiver
//!    process.
//!    The signal handler then waits for the receiver process to exit in order to reap its exit
//!    status (otherwise, upon the termination of the crashing process the child will be
//!    re-parented to PID 1 in the current PID namespace, which can be problematic for some user
//!    applications) and restores the previous signal handler.
//!    Once the receiver has completed, the crash-handler returns, allowing the
//!    previous crash handler (if any) to execute, maintaining the customer
//!    experience as much as possible.
//! 2. The receiver process, which is spawned by the signal handler.  It is connected by an
//!    anynomous AF_UNIX `socketpair()` to the parent process. When a crash occurs, the receiver
//!    gathers the information from the pipe, adds additional data about the system state (e.g.
//!    /proc/cpuinfo and /proc/meminfo), formats it into a crash report, uploads it to the backend,
//!    and then exits. The signal handler must wait for the receiver in order to reap its exit
//!    status.
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

#[cfg(all(unix, feature = "collector"))]
mod collector;
mod crash_info;
#[cfg(all(unix, feature = "receiver"))]
mod receiver;

// TODO: For now, we have name conflicts with the `crash_info` module.
// Once that module is removed, those conflicts will go away
// Till then, keep things in two name spaces
pub mod rfc5_crash_info;
#[cfg(all(unix, any(feature = "collector", feature = "receiver")))]
mod shared;

#[cfg(all(unix, feature = "collector"))]
pub use collector::{
    begin_op, clear_spans, clear_traces, end_op, init, insert_span, insert_trace, on_fork,
    remove_span, remove_trace, reset_counters, shutdown_crash_handler, update_config,
    update_metadata, OpTypes,
};

pub use crash_info::*;

#[cfg(all(unix, feature = "receiver"))]
pub use receiver::{
    async_receiver_entry_point_unix_socket, receiver_entry_point_stdin,
    receiver_entry_point_unix_socket,
};

#[cfg(all(unix, any(feature = "collector", feature = "receiver")))]
pub use shared::configuration::{
    CrashtrackerConfiguration, CrashtrackerReceiverConfig, StacktraceCollection,
};
