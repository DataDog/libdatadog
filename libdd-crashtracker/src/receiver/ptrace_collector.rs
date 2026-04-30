// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Ptrace-based thread context collection with libunwind remote unwinding.
//! This module is compiled for Linux only.
//!
//! This module provides ptrace-based thread context collection that runs in the
//! receiver process. It uses libunwind's remote unwinding APIs to generate full
//! stack traces for all threads in the crashed process.
//!
//! The flow is:
//! 1. Enumerate threads from /proc/<parent_pid>/task/
//! 2. Attach to each thread using PTRACE_SEIZE + PTRACE_INTERRUPT (stops the thread)
//! 3. While the thread is stopped, use libunwind remote APIs to unwind the stack:
//!    - _UPT_create(tid)                create ptrace unwinding state
//!    - unw_create_addr_space()         create address space with ptrace accessors
//!    - unw_init_remote()               initialize remote cursor
//!    - unw_step_remote() loop          walk frames
//!    - unw_get_proc_name_remote()      resolve symbol names
//!    - _UPT_destroy() + cleanup        clean up
//! 4. Detach from the thread via PTRACE_DETACH
//!
//! The crashed parent process stays alive (blocked in the signal handler) until
//! receiver.finish() completes. This guarantees the target process remains a valid
//! ptrace target for the entire duration of thread collection.
//!
//! The parent calls prctl(PR_SET_PTRACER, receiver_pid) before forking the collector,
//! which grants the receiver ptrace permission

use std::ptr;
use std::time::{Duration, Instant};

use libdd_libunwind_sys::{
    UnwAddrSpaceT, UnwCursor, UnwWord, _UPT_accessors, _UPT_create, _UPT_destroy,
    unw_create_addr_space, unw_destroy_addr_space, unw_get_proc_name_remote, unw_get_reg_remote,
    unw_init_remote, unw_step_remote, UNW_REG_IP, UNW_REG_SP,
};

use crate::crash_info::{StackFrame, StackTrace};

/// Maximum number of stack frames to capture per thread
const MAX_FRAMES: usize = 512;

/// A captured thread context containing a full remote stack trace
pub struct CapturedThreadContext {
    pub stack_trace: StackTrace,
}

#[derive(Debug)]
pub enum PtraceError {
    /// Failed to enumerate threads from /proc filesystem
    Enumeration(std::io::Error),
    /// Failed to attach to a thread
    Attach(libc::pid_t, i32),
    /// Failed to detach from a thread
    Detach(libc::pid_t, i32),
}

impl std::fmt::Display for PtraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PtraceError::Enumeration(e) => write!(f, "Failed to enumerate threads: {}", e),
            PtraceError::Attach(tid, errno) => {
                write!(f, "Failed to attach to thread {}: errno {}", tid, errno)
            }
            PtraceError::Detach(tid, errno) => {
                write!(f, "Failed to detach from thread {}: errno {}", tid, errno)
            }
        }
    }
}

impl std::error::Error for PtraceError {}

/// Enumerate all thread IDs for a given process from /proc/<pid>/task/
pub fn enumerate_threads(pid: libc::pid_t) -> Result<Vec<libc::pid_t>, PtraceError> {
    let task_dir = format!("/proc/{}/task", pid);
    let entries = std::fs::read_dir(&task_dir).map_err(PtraceError::Enumeration)?;

    let mut tids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(PtraceError::Enumeration)?;
        if let Ok(name) = entry.file_name().into_string() {
            if let Ok(tid) = name.parse::<libc::pid_t>() {
                tids.push(tid);
            }
        }
    }
    Ok(tids)
}

/// Wait for a thread to enter ptrace-stop after `PTRACE_INTERRUPT`.
///
/// Uses `waitpid` with `__WALL`, which is the standard mechanism for
/// observing ptrace-stop events even on threads created with `CLONE_THREAD`.
fn wait_for_stop(tid: libc::pid_t) -> Result<(), PtraceError> {
    let mut status = 0i32;
    // SAFETY: waitpid with a valid tid; __WALL observes stops on CLONE_THREAD
    // threads regardless of whether the tracer is the thread's parent.
    let ret = unsafe { libc::waitpid(tid, &mut status, libc::__WALL) };
    if ret == -1 || !libc::WIFSTOPPED(status) {
        return Err(PtraceError::Attach(tid, unsafe {
            *libc::__errno_location()
        }));
    }

    Ok(())
}

/// Attach to a thread using PTRACE_SEIZE + PTRACE_INTERRUPT, then wait for it
/// to enter ptrace-stop state before returning.
fn attach_thread(tid: libc::pid_t) -> Result<(), PtraceError> {
    // SAFETY: PTRACE_SEIZE attaches without stopping the thread
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
        return Err(PtraceError::Attach(tid, errno));
    }

    // SAFETY: PTRACE_INTERRUPT delivers a stop to the seized thread
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
        let _ = detach_thread(tid);
        return Err(PtraceError::Attach(tid, errno));
    }

    if let Err(e) = wait_for_stop(tid) {
        let _ = detach_thread(tid);
        return Err(e);
    }
    Ok(())
}

fn detach_thread(tid: libc::pid_t) -> Result<(), PtraceError> {
    // SAFETY: PTRACE_DETACH is valid for a currently-traced thread
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
        return Err(PtraceError::Detach(tid, errno));
    }
    Ok(())
}

/// Capture the full stack trace for a stopped thread using libunwind remote unwinding.
///
/// The thread must already be stopped (via `attach_thread`) before calling this.
/// The caller is responsible for detaching after this returns.
///
/// libunwind's ptrace backend (`_UPT_*`) implements the accessor callbacks that
/// libunwind uses to read memory and registers from the target process via ptrace.
fn unwind_remote_thread(
    tid: libc::pid_t,
    resolve_frames: crate::StacktraceCollection,
) -> StackTrace {
    // SAFETY: _UPT_create allocates a ptrace unwinding context for the given tid.
    // The thread must already be stopped via ptrace for this to succeed.
    let upt_info = unsafe { _UPT_create(tid) };
    if upt_info.is_null() {
        return StackTrace::new_incomplete();
    }

    // SAFETY: _UPT_accessors is a static accessor table provided by libunwind-ptrace.
    // byteorder=0 means native byte order.
    let addr_space: UnwAddrSpaceT =
        unsafe { unw_create_addr_space(&raw const _UPT_accessors as *mut _, 0) };
    if addr_space.is_null() {
        unsafe { _UPT_destroy(upt_info) };
        return StackTrace::new_incomplete();
    }

    // SAFETY: cursor is zeroed; unw_init_remote seeds it from the thread's registers
    // using ptrace with upt_info as the accessor argument.
    let mut cursor: UnwCursor = unsafe { std::mem::zeroed() };
    let ret = unsafe { unw_init_remote(&mut cursor, addr_space, upt_info) };
    if ret != 0 {
        unsafe {
            _UPT_destroy(upt_info);
            unw_destroy_addr_space(addr_space);
        }
        return StackTrace::new_incomplete();
    }

    let mut frames = Vec::new();

    for _ in 0..MAX_FRAMES {
        let mut ip: UnwWord = 0;
        let mut sp: UnwWord = 0;

        // SAFETY: cursor is initialized; unw_get_reg_remote reads from target via ptrace
        if unsafe { unw_get_reg_remote(&mut cursor, UNW_REG_IP, &mut ip) } != 0 || ip == 0 {
            break;
        }
        let _ = unsafe { unw_get_reg_remote(&mut cursor, UNW_REG_SP, &mut sp) };

        let mut frame = StackFrame {
            ip: Some(format!("0x{:x}", ip)),
            sp: Some(format!("0x{:x}", sp)),
            module_base_address: None,
            symbol_address: None,
            build_id: None,
            build_id_type: None,
            file_type: None,
            path: None,
            relative_address: None,
            column: None,
            file: None,
            function: None,
            line: None,
            type_name: None,
            mangled_name: None,
            comments: vec![],
        };

        // Resolve the function name if symbol resolution is enabled
        // We don't care whether it is EnabledWithInprocessSymbols or
        // EnabledWithSymbolsInReceiver since this is happening in the receiver
        if resolve_frames != crate::StacktraceCollection::Disabled
            && resolve_frames != crate::StacktraceCollection::WithoutSymbols
        {
            let mut name_buf = [0 as libc::c_char; 256];
            let mut offset: UnwWord = 0;
            // SAFETY: cursor is valid; unw_get_proc_name_remote reads symbol info via ptrace
            if unsafe {
                unw_get_proc_name_remote(
                    &mut cursor,
                    name_buf.as_mut_ptr().cast(),
                    name_buf.len(),
                    &mut offset,
                )
            } == 0
            {
                // SAFETY: libunwind wrote a null-terminated string into name_buf
                let name = unsafe { std::ffi::CStr::from_ptr(name_buf.as_ptr()) };
                if let Ok(s) = name.to_str() {
                    frame.function = Some(s.to_string());
                }
            }
        }

        frames.push(frame);

        // SAFETY: cursor is valid
        if unsafe { unw_step_remote(&mut cursor) } <= 0 {
            break;
        }
    }

    // SAFETY: cleaning up; these were created above
    unsafe {
        _UPT_destroy(upt_info);
        unw_destroy_addr_space(addr_space);
    }

    StackTrace::from_frames(frames, false)
}

/// Attach to a thread, capture its full stack trace using remote libunwind, then detach.
pub fn capture_thread_context(
    tid: libc::pid_t,
    resolve_frames: crate::StacktraceCollection,
) -> Result<CapturedThreadContext, PtraceError> {
    attach_thread(tid)?;

    let stack_trace = unwind_remote_thread(tid, resolve_frames);

    // Best-effort detach: if this fails the thread stays in ptrace-stop, but the
    // receiver exiting will clean it up. Don't discard a good stack trace over it.
    let _ = detach_thread(tid);

    Ok(CapturedThreadContext { stack_trace })
}

/// Stream thread contexts to a callback, one at a time, without intermediate storage.
///
/// For each non-crashing thread the callback receives the TID and an optional
/// `CapturedThreadContext` (None if attachment or unwinding failed).
pub fn stream_thread_contexts<F>(
    parent_pid: libc::pid_t,
    crashing_tid: libc::pid_t,
    max_threads: usize,
    timeout: Duration,
    resolve_frames: crate::StacktraceCollection,
    mut callback: F,
) -> Result<(), PtraceError>
where
    F: FnMut(libc::pid_t, Option<&CapturedThreadContext>),
{
    let start_time = Instant::now();
    let tids = enumerate_threads(parent_pid)?;
    let mut processed = 0;

    for tid in tids {
        if start_time.elapsed() >= timeout {
            break;
        }
        if tid == crashing_tid {
            continue;
        }
        if processed >= max_threads {
            break;
        }

        let context = capture_thread_context(tid, resolve_frames).ok();
        callback(tid, context.as_ref());
        processed += 1;
    }

    Ok(())
}
