// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

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
//! 2. The receiver process runs in the background, listening on `stdin`, which
//!    is connected by a pipe to the parent process.  When a crash occurs, the
//!    receiver gathers the information from the pipe, adds additional data
//!    about the system state (e.g. /proc/cpuinfo and /proc/meminfo), formats it
//!    into a crash report, uploads it to the backend, and then exits.
//!    The receiver also exits if the pipe is closed without a crash report,
//!    to avoid leaving a zombie process if the parent exits normally.
//!
//! Data collected:
//! 1. The data collected by the crash-handler includes:
//!    i. The signal type leading to the crash
//!    ii. The stacktrace at time of crash (for the crashing thread).  Depending
//!        on a flag, this can either be resolved, or raw addresses.
//!        Resolving addresses provide more data, but sometimes crashes the
//!        crash handler (ironic).
//!    iii. System level info (e.g. /proc/self/maps).
//!    iv. The result of counters describing the current state of the profiler.
//! 2. Data augmented by the receiver includes:
//!    i. Metadata provided by the caller (e.g. library & profiler versions).
//!    ii. System info: OS version, /proc/cpuinfo /proc/meminfo, etc.
//!    iii. A timestamp and GUID for tracking the crash report.
//!    
//! Handling of forks
//! Safety issues
#![cfg(unix)]

mod api;
mod collectors;
mod constants;
mod counters;
mod crash_handler;
mod crash_info;
mod receiver;
mod stacktrace;

pub use api::*;
pub use constants::*;
pub use counters::{begin_profiling_op, end_profiling_op, ProfilingOpTypes};
pub use crash_handler::{update_config, update_metadata};
pub use crash_info::*;
pub use receiver::receiver_entry_point;

#[test]
fn test_blaze() {
    use blazesym::symbolize::CodeInfo;
    use blazesym::symbolize::Input;
    use blazesym::symbolize::Process;
    use blazesym::symbolize::Source;
    use blazesym::symbolize::Sym;
    use blazesym::symbolize::Symbolized;
    use blazesym::symbolize::Symbolizer;
    use blazesym::Addr;
    use blazesym::Pid;

    const ADDR_WIDTH: usize = 16;
    // Retrieve up to 64 stack frames of the calling thread.
    const MAX_CNT: usize = 64;

    let mut addrs_buf = [std::ptr::null_mut::<libc::c_void>(); MAX_CNT];
    let addr_cnt = unsafe { libc::backtrace(addrs_buf.as_mut_ptr(), MAX_CNT as _) } as usize;
    let addrs = &addrs_buf[0..std::cmp::min(addr_cnt, MAX_CNT)];
    // Symbolize the addresses for the current process, as that's where
    // they were captured.
    let mut process = Process::new(std::process::id().into());
    process.map_files = false;
    let src = Source::Process(process);
    let symbolizer = Symbolizer::new();
    let _syms = symbolizer
        .symbolize(&src, Input::AbsAddr(&[addrs[0] as u64]))
        .unwrap();
}
