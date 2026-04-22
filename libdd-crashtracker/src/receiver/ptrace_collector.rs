// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Ptrace-based thread context collection for crash reporting.
//! This module is compiled for Linux only.
//!
//! # Overview
//!
//! This module provides ptrace-based thread context collection that runs in the
//! collector child process (not in signal handler context). It replaces the previous
//! SIGUSR2-based in-process collection mechanism.
//!
//! The flow is:
//! 1. Collector child enumerates threads from /proc/<parent_pid>/task/
//! 2. Attaches to each thread using PTRACE_SEIZE
//! 3. Captures register context using PTRACE_GETREGSET
//! 4. Reads stack memory using process_vm_readv
//! 5. Detaches from threads
//!
//! # Benefits over SIGUSR2 approach
//!
//! - No pre-allocation of memory
//! - No conflict with application's use of SIGUSR2
//! - Better thread states (stopped at execution points, not in signal handlers)
//! - Richer context (can access additional register sets)
//! - No async-signal-safety constraints

use libc::{ucontext_t, user_regs_struct};
use std::ptr;
use std::time::{Duration, Instant};

/// Maximum number of threads to collect contexts for
const MAX_TRACKED_THREADS: usize = 128;

/// A captured thread context containing basic register state
///
/// TODO: Add remote libunwind stack walking using:
/// - unw_init_remote() with _UPT_accessors for ptrace-based remote unwinding
/// - _UPT_create(), _UPT_destroy() for managing ptrace state
/// - This will enable full stack traces for all threads
pub struct CapturedThreadContext {
    /// The captured register context as a ucontext_t
    pub ucontext: ucontext_t,
}

/// Error types for ptrace operations
#[derive(Debug)]
pub enum PtraceError {
    /// Failed to enumerate threads from /proc filesystem
    EnumerationFailed(std::io::Error),
    /// Failed to attach to a thread
    AttachFailed(libc::pid_t, i32),
    /// Failed to read registers from a thread
    RegisterReadFailed(libc::pid_t, i32),
    /// Failed to detach from a thread
    DetachFailed(libc::pid_t, i32),
}

impl std::fmt::Display for PtraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PtraceError::EnumerationFailed(e) => write!(f, "Failed to enumerate threads: {}", e),
            PtraceError::AttachFailed(tid, errno) => {
                write!(f, "Failed to attach to thread {}: errno {}", tid, errno)
            }
            PtraceError::RegisterReadFailed(tid, errno) => {
                write!(
                    f,
                    "Failed to read registers from thread {}: errno {}",
                    tid, errno
                )
            }
            PtraceError::DetachFailed(tid, errno) => {
                write!(f, "Failed to detach from thread {}: errno {}", tid, errno)
            }
        }
    }
}

impl std::error::Error for PtraceError {}

/// Enumerate all thread IDs for a given process
///
/// Reads the /proc/<pid>/task/ directory to discover all threads.
/// Returns a vector of thread IDs.
pub fn enumerate_threads(pid: libc::pid_t) -> Result<Vec<libc::pid_t>, PtraceError> {
    let task_dir = format!("/proc/{}/task", pid);
    let entries = std::fs::read_dir(&task_dir).map_err(PtraceError::EnumerationFailed)?;

    let mut tids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(PtraceError::EnumerationFailed)?;
        if let Ok(name) = entry.file_name().into_string() {
            if let Ok(tid) = name.parse::<libc::pid_t>() {
                tids.push(tid);
            }
        }
    }

    Ok(tids)
}

/// Attach to a thread using ptrace
///
/// Uses PTRACE_SEIZE to attach without stopping the thread initially.
/// The thread must be explicitly stopped with PTRACE_INTERRUPT.
pub fn attach_thread(tid: libc::pid_t) -> Result<(), PtraceError> {
    // SAFETY: PTRACE_SEIZE is a valid ptrace operation
    let result = unsafe {
        libc::ptrace(
            libc::PTRACE_SEIZE,
            tid as libc::c_long,
            ptr::null_mut::<libc::c_void>(),
            ptr::null_mut::<libc::c_void>(),
        )
    };

    if result == -1 {
        let errno = unsafe { *libc::__errno_location() };
        return Err(PtraceError::AttachFailed(tid, errno));
    }

    // Stop the thread so we can read its registers
    // SAFETY: PTRACE_INTERRUPT is a valid ptrace operation for a seized thread
    let result = unsafe {
        libc::ptrace(
            libc::PTRACE_INTERRUPT,
            tid as libc::c_long,
            ptr::null_mut::<libc::c_void>(),
            ptr::null_mut::<libc::c_void>(),
        )
    };

    if result == -1 {
        let errno = unsafe { *libc::__errno_location() };
        // Try to detach on failure
        let _ = detach_thread(tid);
        return Err(PtraceError::AttachFailed(tid, errno));
    }

    // Wait for the thread to stop
    let mut status = 0;
    // SAFETY: waitpid is safe with valid arguments
    unsafe {
        libc::waitpid(tid, &mut status, 0);
    }

    Ok(())
}

/// Detach from a thread
pub fn detach_thread(tid: libc::pid_t) -> Result<(), PtraceError> {
    // SAFETY: PTRACE_DETACH is a valid ptrace operation
    let result = unsafe {
        libc::ptrace(
            libc::PTRACE_DETACH,
            tid as libc::c_long,
            ptr::null_mut::<libc::c_void>(),
            ptr::null_mut::<libc::c_void>(),
        )
    };

    if result == -1 {
        let errno = unsafe { *libc::__errno_location() };
        return Err(PtraceError::DetachFailed(tid, errno));
    }

    Ok(())
}

/// Read general purpose registers from a thread using ptrace
///
/// Uses PTRACE_GETREGS to read the x86_64 general purpose registers.
/// Converts them to a ucontext_t format for compatibility with existing code.
pub fn read_thread_registers(tid: libc::pid_t) -> Result<ucontext_t, PtraceError> {
    let mut regs: user_regs_struct = unsafe { std::mem::zeroed() };

    // SAFETY: PTRACE_GETREGS is valid for stopped threads, regs is properly allocated
    let result = unsafe {
        libc::ptrace(
            libc::PTRACE_GETREGS,
            tid as libc::c_long,
            ptr::null_mut::<libc::c_void>(),
            &mut regs as *mut user_regs_struct as *mut libc::c_void,
        )
    };

    if result == -1 {
        let errno = unsafe { *libc::__errno_location() };
        return Err(PtraceError::RegisterReadFailed(tid, errno));
    }

    // Convert user_regs_struct to ucontext_t for compatibility
    let mut uctx: ucontext_t = unsafe { std::mem::zeroed() };

    // Fill in the mcontext with register values
    // These field mappings are specific to x86_64 Linux
    #[cfg(target_arch = "x86_64")]
    {
        uctx.uc_mcontext.gregs[libc::REG_RIP as usize] = regs.rip as i64;
        uctx.uc_mcontext.gregs[libc::REG_RSP as usize] = regs.rsp as i64;
        uctx.uc_mcontext.gregs[libc::REG_RBP as usize] = regs.rbp as i64;
        uctx.uc_mcontext.gregs[libc::REG_RAX as usize] = regs.rax as i64;
        uctx.uc_mcontext.gregs[libc::REG_RBX as usize] = regs.rbx as i64;
        uctx.uc_mcontext.gregs[libc::REG_RCX as usize] = regs.rcx as i64;
        uctx.uc_mcontext.gregs[libc::REG_RDX as usize] = regs.rdx as i64;
        uctx.uc_mcontext.gregs[libc::REG_RSI as usize] = regs.rsi as i64;
        uctx.uc_mcontext.gregs[libc::REG_RDI as usize] = regs.rdi as i64;
        uctx.uc_mcontext.gregs[libc::REG_R8 as usize] = regs.r8 as i64;
        uctx.uc_mcontext.gregs[libc::REG_R9 as usize] = regs.r9 as i64;
        uctx.uc_mcontext.gregs[libc::REG_R10 as usize] = regs.r10 as i64;
        uctx.uc_mcontext.gregs[libc::REG_R11 as usize] = regs.r11 as i64;
        uctx.uc_mcontext.gregs[libc::REG_R12 as usize] = regs.r12 as i64;
        uctx.uc_mcontext.gregs[libc::REG_R13 as usize] = regs.r13 as i64;
        uctx.uc_mcontext.gregs[libc::REG_R14 as usize] = regs.r14 as i64;
        uctx.uc_mcontext.gregs[libc::REG_R15 as usize] = regs.r15 as i64;
        uctx.uc_mcontext.gregs[libc::REG_EFL as usize] = regs.eflags as i64;
        uctx.uc_mcontext.gregs[libc::REG_CSGSFS as usize] =
            ((regs.cs as i64) << 16) | ((regs.gs as i64) << 8) | (regs.fs as i64);
    }

    Ok(uctx)
}

/// Capture register context for a single thread
pub fn capture_thread_context(
    _pid: libc::pid_t,
    tid: libc::pid_t,
) -> Result<CapturedThreadContext, PtraceError> {
    // Attach to the thread
    attach_thread(tid)?;

    // Read basic registers
    let ucontext = match read_thread_registers(tid) {
        Ok(uctx) => uctx,
        Err(e) => {
            let _ = detach_thread(tid);
            return Err(e);
        }
    };

    // Detach from the thread
    detach_thread(tid)?;

    Ok(CapturedThreadContext { ucontext })
}

/// Streaming callback-based thread context collection
///
/// This function enumerates threads and calls the provided callback for each thread.
/// The callback receives the thread info and an optional captured context.
/// No intermediate storage is used
pub fn stream_thread_contexts<F>(
    parent_pid: libc::pid_t,
    crashing_tid: libc::pid_t,
    max_threads: usize,
    timeout: Duration,
    mut callback: F,
) -> Result<(), PtraceError>
where
    F: FnMut(libc::pid_t, Option<&CapturedThreadContext>) -> bool, /* returns false to stop
                                                                    * iteration */
{
    let start_time = Instant::now();

    // Enumerate all threads
    let tids = enumerate_threads(parent_pid)?;

    // Limit the number of threads we process
    let max_count = max_threads.min(MAX_TRACKED_THREADS);
    let mut processed = 0;

    for tid in tids {
        // Check timeout
        if start_time.elapsed() >= timeout {
            break;
        }

        // Skip the crashing thread
        if tid == crashing_tid {
            continue;
        }

        // Skip if we've hit our limit
        if processed >= max_count {
            break;
        }

        // Try to capture this thread's context
        let context = match capture_thread_context(parent_pid, tid) {
            Ok(ctx) => Some(ctx),
            Err(_) => None, // Continue with other threads even if this one fails
        };

        // Call the callback with the thread info
        let should_continue = callback(tid, context.as_ref());
        processed += 1;

        if !should_continue {
            break;
        }
    }

    Ok(())
}
