// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Ptrace-based thread context collection with libunwind remote unwinding.
//! This is compiled for Linux only.
//!
//! This provides ptrace-based thread context collection that runs in the
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
    _UPT_accessors, _UPT_create, _UPT_destroy, unw_create_addr_space, unw_destroy_addr_space,
    unw_get_proc_name_remote, unw_get_reg_remote, unw_init_remote, unw_step_remote, UnwAddrSpaceT,
    UnwCursor, UnwWord, UNW_REG_IP, UNW_REG_SP,
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

/// Wait for a thread to enter ptrace-stop after `PTRACE_INTERRUPT`, with a deadline.
///
/// Polls with `WNOHANG` in a short sleep loop so that a single slow thread
/// cannot consume the entire remaining collection budget.
fn wait_for_stop(tid: libc::pid_t, deadline: Instant) -> Result<(), PtraceError> {
    const POLL_SLEEP: Duration = Duration::from_millis(2);
    loop {
        let mut status = 0i32;
        // SAFETY: waitpid with WNOHANG | __WALL returns immediately if the thread
        // has not yet stopped. __WALL observes stops on CLONE_THREAD threads
        // regardless of whether the tracer is the thread's parent.
        let ret = unsafe { libc::waitpid(tid, &mut status, libc::__WALL | libc::WNOHANG) };
        if ret == tid as libc::pid_t {
            // Got a status event for this thread.
            if libc::WIFSTOPPED(status) {
                return Ok(());
            }
            // Got an event but it wasn't a stop (the thread exited).
            return Err(PtraceError::Attach(tid, unsafe {
                *libc::__errno_location()
            }));
        } else if ret == 0 {
            // Thread not yet stopped; check deadline before sleeping.
            if Instant::now() >= deadline {
                return Err(PtraceError::Attach(tid, libc::ETIMEDOUT));
            }
            std::thread::sleep(POLL_SLEEP);
        } else {
            // ret == -1: a real error.
            return Err(PtraceError::Attach(tid, unsafe {
                *libc::__errno_location()
            }));
        }
    }
}

/// Attach to a thread using PTRACE_SEIZE + PTRACE_INTERRUPT, then wait for it
/// to enter ptrace-stop state before returning.
///
/// `stop_deadline` bounds how long we poll for the stop event.
///
/// After the thread enters ptrace-stop, this function also polls until the
/// instruction pointer is non-zero. On older kernels there may be a race where
/// `waitpid` returns WIFSTOPPED but the thread's register state hasn't been fully
/// flushed to the ptrace-accessible area yet. Reading registers in that window
/// yields zeros, which causes libunwind to produce an empty stack trace.
fn attach_thread(tid: libc::pid_t, stop_deadline: Instant) -> Result<(), PtraceError> {
    // PTRACE_SEIZE attaches without stopping the thread
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

    // PTRACE_INTERRUPT delivers a stop to the seized thread
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

    if let Err(e) = wait_for_stop(tid, stop_deadline) {
        let _ = detach_thread(tid);
        return Err(e);
    }

    // On older kernels, the register state may not be
    // immediately readable after waitpid reports the stop. Spin briefly
    // until PEEKUSER returns a non-zero IP, proving registers are committed.
    wait_for_registers(tid, stop_deadline);

    Ok(())
}

/// Poll the thread's instruction pointer using PTRACE_PEEKUSER until it is
/// non-zero or the deadline expires. On modern kernels this should return on the
/// first iteration; on older ones, it may take a few microseconds.
fn wait_for_registers(tid: libc::pid_t, deadline: Instant) {
    #[cfg(target_arch = "x86_64")]
    const IP_OFFSET: libc::c_long = 16 * std::mem::size_of::<libc::c_long>() as libc::c_long; // RIP

    #[cfg(target_arch = "aarch64")]
    const IP_OFFSET: libc::c_long = 32 * std::mem::size_of::<libc::c_long>() as libc::c_long; // PC

    const SPIN_SLEEP: Duration = Duration::from_micros(100);

    loop {
        unsafe { *libc::__errno_location() = 0 };
        let ip = unsafe { libc::ptrace(libc::PTRACE_PEEKUSER, tid as libc::c_long, IP_OFFSET, 0) };
        // PEEKUSER returns the register value; errno==0 means success.
        if ip != 0 && unsafe { *libc::__errno_location() } == 0 {
            return;
        }
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(SPIN_SLEEP);
    }
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
        // ESRCH means the thread already exited; treat as success since
        // there is nothing left to detach from.
        if errno != libc::ESRCH {
            return Err(PtraceError::Detach(tid, errno));
        }
    }
    Ok(())
}

/// Create a libunwind remote address space backed by the `_UPT_accessors` ptrace
/// callback table.
///
/// The returned address space is reusable across multiple threads: all threads in
/// the same process share the same binary mappings, so the DWARF unwind info that
/// libunwind caches inside the address space is valid for every thread.  The
/// caller owns the address space and must destroy it with `unw_destroy_addr_space`
/// when done.  Returns `None` if allocation fails.
fn create_addr_space() -> Option<UnwAddrSpaceT> {
    // SAFETY: _UPT_accessors is a static accessor table provided by libunwind-ptrace.
    // unw_create_addr_space only reads (copies) the accessor struct; it does not mutate
    // it.  The *mut _ cast is required because the C declaration is const-incorrect, but
    // no mutation occurs, so casting &raw const to *mut is safe here.
    // byteorder=0 requests native byte order.
    let addr_space = unsafe { unw_create_addr_space(&raw const _UPT_accessors as *mut _, 0) };
    if addr_space.is_null() {
        None
    } else {
        Some(addr_space)
    }
}

/// Capture the full stack trace for a stopped thread using libunwind remote unwinding.
///
/// The thread must already be stopped (`attach_thread`) before calling this.
/// The caller is responsible for detaching after this returns.
///
/// `addr_space` is a pre-created address space shared across threads; this
/// function does *not* destroy it.
fn unwind_remote_thread(
    tid: libc::pid_t,
    addr_space: UnwAddrSpaceT,
    resolve_frames: crate::StacktraceCollection,
) -> StackTrace {
    // SAFETY: _UPT_create allocates a ptrace unwinding context for the given tid.
    // The thread must already be stopped via ptrace for this to succeed.
    let upt_info = unsafe { _UPT_create(tid) };
    if upt_info.is_null() {
        return StackTrace::new_incomplete();
    }

    // SAFETY: cursor is zeroed; unw_init_remote seeds it from the thread's registers
    // using ptrace with upt_info as the accessor argument.
    let mut cursor: UnwCursor = unsafe { std::mem::zeroed() };
    let ret = unsafe { unw_init_remote(&mut cursor, addr_space, upt_info) };
    if ret != 0 {
        unsafe { _UPT_destroy(upt_info) };
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

        // Resolve the function name if in process symbol resolution is enabled.
        if resolve_frames == crate::StacktraceCollection::EnabledWithSymbolsInReceiver {
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

    // SAFETY: upt_info was created above; addr_space is owned by the caller
    unsafe { _UPT_destroy(upt_info) };

    StackTrace::from_frames(frames, false)
}

/// Attach to a thread, capture its full stack trace using remote libunwind, then detach.
///
/// `addr_space` is a pre-created address space that may be shared across multiple
/// calls (all threads in the same process share binary mappings, so the DWARF
/// cache inside the address space remains valid).
///
/// `stop_deadline` bounds how long we poll for the thread to enter ptrace-stop.
pub fn capture_thread_context(
    tid: libc::pid_t,
    resolve_frames: crate::StacktraceCollection,
    addr_space: UnwAddrSpaceT,
    stop_deadline: Instant,
) -> Result<CapturedThreadContext, PtraceError> {
    attach_thread(tid, stop_deadline)?;

    let stack_trace = unwind_remote_thread(tid, addr_space, resolve_frames);

    // Best-effort detach: if this fails the thread stays in ptrace-stop, but the
    // receiver exiting will clean it up. Don't discard a good stack trace over it.
    let _ = detach_thread(tid);

    Ok(CapturedThreadContext { stack_trace })
}

/// Maximum time to wait for a single thread to enter ptrace-stop.
const STOP_TIMEOUT_PER_THREAD: Duration = Duration::from_millis(50);

/// Stream thread contexts to a callback one at a time.
///
/// For each non-crashing thread the callback receives the TID and an optional
/// `CapturedThreadContext` (None if attachment or unwinding failed).
///
/// The crashing thread is excluded: it is executing a signal handler on the
/// alternate signal stack, so ptrace-based unwinding cannot traverse the signal
/// frame and consistently crashes the receiver. The crash-site registers are
/// already captured from the signal context before this function is called.
///
/// Two deadlines bound collection:
/// - An *overall* deadline derived from `timeout`, shared across all threads.
/// - A *per-thread stop* deadline of at most `STOP_TIMEOUT_PER_THREAD` (capped at the overall
///   deadline) so that a single slow-to-stop thread cannot starve the rest.
///
/// Returns `Ok(incomplete)` where `incomplete` is `true` when collection was cut
/// short by the timeout or the `max_threads` cap, meaning there may be additional
/// threads that were not visited.
pub fn stream_thread_contexts<F>(
    parent_pid: libc::pid_t,
    crashing_tid: libc::pid_t,
    max_threads: usize,
    timeout: Duration,
    resolve_frames: crate::StacktraceCollection,
    mut callback: F,
) -> Result<bool, PtraceError>
where
    F: FnMut(libc::pid_t, Option<&CapturedThreadContext>),
{
    let overall_deadline = Instant::now() + timeout;
    // Exclude the crashing thread: it is blocked in the signal handler waiting
    // for the receiver to finish. Ptracing it from the receiver causes PTRACE_INTERRUPT
    // to fire inside the signal handler, and libunwind cannot walk the alternate
    // signal stack through the signal frame, consistently crashing the receiver.
    let tids: Vec<_> = enumerate_threads(parent_pid)?
        .into_iter()
        .filter(|&tid| tid != crashing_tid)
        .collect();
    let total_eligible = tids.len();
    let mut processed = 0;

    // Create a single address space shared across all threads.  All threads in the
    // same process share the same binary mappings, so the DWARF unwind info that
    // libunwind caches inside the address space is valid for every thread and is
    // reused rather than re-parsed on each iteration.
    let Some(addr_space) = create_addr_space() else {
        return Ok(true); // treat as incomplete; nothing was collected
    };

    for tid in tids {
        let now = Instant::now();
        if now >= overall_deadline || processed >= max_threads {
            break;
        }

        // Cap the per-thread stop wait at STOP_TIMEOUT_PER_THREAD but never
        // past the overall deadline, so one thread can't consume the budget.
        let stop_deadline = (now + STOP_TIMEOUT_PER_THREAD).min(overall_deadline);

        let context = capture_thread_context(tid, resolve_frames, addr_space, stop_deadline).ok();
        callback(tid, context.as_ref());
        processed += 1;
    }

    // SAFETY: addr_space was created above and is no longer referenced
    unsafe { unw_destroy_addr_space(addr_space) };

    let incomplete = processed < total_eligible;
    Ok(incomplete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    fn current_tid() -> libc::pid_t {
        unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
    }

    #[test]
    fn enumerate_includes_current_thread() {
        let pid = std::process::id() as libc::pid_t;
        let tids = enumerate_threads(pid).expect("enumerate_threads should succeed for self");
        assert!(tids.contains(&pid), "main thread TID {pid} not in {tids:?}");
    }

    #[test]
    fn enumerate_rejects_nonexistent_pid() {
        // PID 0 is not a real process.
        assert!(enumerate_threads(0).is_err());
    }

    #[test]
    fn enumerate_discovers_spawned_thread() {
        let barrier = Arc::new(Barrier::new(2));
        let b = Arc::clone(&barrier);
        let (tx, rx) = std::sync::mpsc::channel();

        let handle = std::thread::spawn(move || {
            tx.send(current_tid()).unwrap();

            b.wait();
        });

        let spawned_tid = rx.recv().unwrap();
        let pid = std::process::id() as libc::pid_t;
        let tids = enumerate_threads(pid).expect("enumerate_threads should succeed");

        assert!(
            tids.contains(&spawned_tid),
            "spawned TID {spawned_tid} should appear in {tids:?}"
        );

        barrier.wait();
        handle.join().unwrap();
    }

    /// A stopped thread should produce at least one frame (the IP at ptrace-stop).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn capture_context_produces_frames() {
        let barrier = Arc::new(Barrier::new(2));
        let b = Arc::clone(&barrier);
        let (tx, rx) = std::sync::mpsc::channel();

        let handle = std::thread::spawn(move || {
            tx.send(current_tid()).unwrap();
            b.wait();
        });

        let tid = rx.recv().unwrap();

        let Some(addr_space) = create_addr_space() else {
            eprintln!("skipping ptrace test (unw_create_addr_space failed)");
            barrier.wait();
            handle.join().unwrap();
            return;
        };
        match capture_thread_context(
            tid,
            crate::StacktraceCollection::Disabled,
            addr_space,
            Instant::now() + Duration::from_secs(5),
        ) {
            Err(e) => eprintln!("skipping ptrace test (ptrace unavailable): {e}"),
            Ok(ctx) => assert!(
                !ctx.stack_trace.frames.is_empty(),
                "expected at least one frame from a running thread"
            ),
        }
        // SAFETY: addr_space was created above and is no longer referenced
        unsafe { unw_destroy_addr_space(addr_space) };

        barrier.wait();
        handle.join().unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn stream_respects_max_threads_limit() {
        // Spawn 3 extra threads so there are definitely more than 2 to iterate.
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();
        for _ in 0..3 {
            let b = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                b.wait();
            }));
        }
        barrier.wait();

        let mut collected = 0usize;
        let _ = stream_thread_contexts(
            std::process::id() as libc::pid_t,
            current_tid(),
            2,
            Duration::from_secs(5),
            crate::StacktraceCollection::Disabled,
            |_tid, _ctx| collected += 1,
        );

        assert!(collected <= 2, "collected {collected}, expected <= 2");
        for h in handles {
            h.join().unwrap();
        }
    }

    /// Regression test: a thread blocked in poll() passed as crashing_tid must not
    /// be ptraced. Previously, PTRACE_INTERRUPT would fire inside the signal handler's
    /// poll() call causing an EINTR loop that hung the receiver indefinitely.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn stream_excludes_crashing_tid_blocked_in_poll() {
        let mut pipe_fds = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
        let [read_fd, write_fd] = pipe_fds;

        let (tx, rx) = std::sync::mpsc::channel();

        let handle = std::thread::spawn(move || {
            tx.send(current_tid()).unwrap();
            let mut pfd = libc::pollfd { fd: read_fd, events: libc::POLLHUP, revents: 0 };
            unsafe { libc::poll(&mut pfd, 1, 10_000) };
            unsafe { libc::close(read_fd) };
        });

        let blocking_tid = rx.recv().unwrap();

        let mut seen_blocking = false;
        let _ = stream_thread_contexts(
            std::process::id() as libc::pid_t,
            blocking_tid,
            64,
            Duration::from_secs(5),
            crate::StacktraceCollection::Disabled,
            |tid, _ctx| {
                if tid == blocking_tid {
                    seen_blocking = true;
                }
            },
        );

        assert!(!seen_blocking, "thread blocked in poll() must not be ptraced as crashing_tid");

        unsafe { libc::close(write_fd) };
        handle.join().unwrap();
    }
}
