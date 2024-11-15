// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use libc::pid_t;
use std::fs::File;
use std::io::{self, BufRead, BufReader};

#[cfg(target_os = "macos")]
pub(crate) fn alt_fork() -> i32 {
    // There is a lower-level `__fork()` function in macOS, and we can call it from Rust, but the
    // runtime is much stricter about which operations (e.g., no malloc) are allowed in the child.
    // This somewhat defeats the purpose, so macOS for now will just have to live with atfork
    // handlers.
    unsafe { libc::fork() }
}

#[cfg(target_os = "linux")]
fn is_being_traced() -> io::Result<bool> {
    // Check to see whether we are being traced.  This will fail on systems where procfs is
    // unavailable, but presumably in those systems `ptrace()` is also unavailable.
    // The caller is free to treat a failure as a false.
    let file = File::open("/proc/self/status")?;
    let reader = BufReader::new(file);

    for line in reader.lines() {
        let line = line?;
        if line.starts_with("TracerPid:") {
            let tracer_pid = line.split_whitespace().nth(1).unwrap_or("0");
            return Ok(tracer_pid != "0");
        }
    }

    Ok(false)
}

#[cfg(target_os = "linux")]
pub(crate) fn alt_fork() -> pid_t {
    use libc::{
        c_ulong, c_void, syscall, SYS_clone, CLONE_CHILD_CLEARTID, CLONE_CHILD_SETTID,
        CLONE_PTRACE, SIGCHLD,
    };

    let mut ptid: pid_t = 0;
    let mut ctid: pid_t = 0;

    // Check whether we're traced before we fork.
    let being_traced = is_being_traced().unwrap_or(false);
    let extra_flags = if being_traced { CLONE_PTRACE } else { 0 };

    // Use the direct syscall interface into `clone()`.  This should replicate the parameters used
    // for glibc `fork()`, except of course without calling the atfork handlers.
    // One question is whether we're using the right set of flags.  For instance, does suppressing
    // `SIGCHLD` here make it easier for us to handle some conditions in the parent process?
    let res = unsafe {
        syscall(
            SYS_clone,
            (CLONE_CHILD_CLEARTID | CLONE_CHILD_SETTID | SIGCHLD | extra_flags) as c_ulong,
            std::ptr::null_mut::<c_void>(),
            &mut ptid as *mut pid_t,
            &mut ctid as *mut pid_t,
            0 as c_ulong,
        )
    };

    if res == -1 {
        return -1;
    }

    ctid
}