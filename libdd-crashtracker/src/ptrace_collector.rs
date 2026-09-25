// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Collect thread stacks with ptrace and libunwind on Linux.
//!
//! The collector is a child of the crashing process, using its credentials and owning its
//! ptrace stop notifications. Trusted receivers can also collect threads as a fallback.
//! Restricted receivers only consume supplied stacks.
//!
//! Threads share one libunwind address space to reuse its DWARF cache. Each capture records
//! instruction and stack pointers; symbolization happens after the thread is detached.
//!
//! The crashing process grants access with PR_SET_PTRACER and waits for reporting to finish.

#![allow(clippy::std_instead_of_alloc, clippy::std_instead_of_core)]

use std::ptr;
use std::time::{Duration, Instant};

use libdd_libunwind_sys::{
    unw_get_reg_remote, unw_init_remote, unw_step_remote, UnwAddrSpace, UnwCursor, UnwWord,
    UptInfo, UNW_REG_IP, UNW_REG_SP,
};

use crate::crash_info::{StackFrame, StackTrace, ThreadData};

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
    //
    // If the deadline expires before we see a non-zero IP, proceed anyway:
    // libunwind uses PTRACE_GETREGSET which may succeed even when PEEKUSER
    // returns zero. If registers are truly uncommitted, unwind_remote_thread
    // will return 0 frames and capture_with_retry will retry without needing
    // a costly detach/re-attach cycle (which can fail with EPERM under CPU
    // pressure because the kernel hasn't fully released the prior ptrace
    // state).
    let _ = wait_for_registers(tid, stop_deadline);

    Ok(())
}

/// Poll the thread's instruction pointer using PTRACE_PEEKUSER until it is
/// non-zero or the deadline expires. On modern kernels this should return on the
/// first iteration; on older ones, it may take a few microseconds.
///
/// Returns `true` if a non-zero IP was observed or the check is not applicable
/// (PTRACE_PEEKUSER unsupported on the architecture), `false` if the
/// deadline expired without reading a valid IP on a platform that supports it.
fn wait_for_registers(tid: libc::pid_t, deadline: Instant) -> bool {
    #[cfg(target_arch = "x86_64")]
    const IP_OFFSET: libc::c_long = 16 * std::mem::size_of::<libc::c_long>() as libc::c_long; // RIP

    #[cfg(target_arch = "aarch64")]
    const IP_OFFSET: libc::c_long = 32 * std::mem::size_of::<libc::c_long>() as libc::c_long; // PC

    const SPIN_SLEEP: Duration = Duration::from_micros(100);

    // First probe: if PTRACE_PEEKUSER returns EIO, the kernel doesn't support it
    // In that case, skip the check. libunwind uses PTRACE_GETREGSET which works
    // regardless, and modern kernels commit register state synchronously on ptrace-stop.
    unsafe { *libc::__errno_location() = 0 };
    let ip = unsafe { libc::ptrace(libc::PTRACE_PEEKUSER, tid as libc::c_long, IP_OFFSET, 0) };
    let errno = unsafe { *libc::__errno_location() };
    if errno == libc::EIO {
        return true;
    }
    if ip != 0 && errno == 0 {
        return true;
    }

    loop {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(SPIN_SLEEP);
        unsafe { *libc::__errno_location() = 0 };
        let ip = unsafe { libc::ptrace(libc::PTRACE_PEEKUSER, tid as libc::c_long, IP_OFFSET, 0) };
        let errno = unsafe { *libc::__errno_location() };
        if errno == libc::EIO {
            return true;
        }
        if ip != 0 && errno == 0 {
            return true;
        }
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

    // Drain any pending waitpid event so the kernel fully releases the thread.
    // Without this, a rapid re-attach (PTRACE_SEIZE) can fail with EPERM under
    // CPU pressure because the kernel hasn't finished processing the detach.
    unsafe {
        libc::waitpid(tid, ptr::null_mut(), libc::__WALL | libc::WNOHANG);
    }

    Ok(())
}

/// Capture the full stack trace for a stopped thread using libunwind remote unwinding.
///
/// The thread must already be stopped (`attach_thread`) before calling this.
/// The caller is responsible for detaching after this returns.
///
/// `addr_space` is owned by the caller and shared across threads; this function
/// only borrows it.
fn unwind_remote_thread(tid: libc::pid_t, addr_space: &UnwAddrSpace) -> StackTrace {
    // The ptrace unwinding context requires the thread to already be stopped by ptrace.
    // It is released when `upt_info` goes out of scope.
    let Some(upt_info) = UptInfo::new(tid) else {
        return StackTrace::new_incomplete();
    };

    // SAFETY: cursor is zeroed; unw_init_remote seeds it from the thread's registers
    // using ptrace with upt_info as the accessor argument.
    let mut cursor: UnwCursor = unsafe { std::mem::zeroed() };
    let ret = unsafe { unw_init_remote(&mut cursor, addr_space.as_ptr(), upt_info.as_ptr()) };
    if ret != 0 {
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

        // Function names are resolved later by CrashInfo::enrich_callstacks using blazesym.
        // Previously, libunwind supplied an earlier name that blazesym replaced on success
        // and retained only as a fallback on failure. Producing that fallback searched the
        // target's ELF symbols once per frame and could crash the receiver before upload.
        // Prefer unresolved addresses in a complete report to losing the entire report.
        frames.push(StackFrame {
            ip: Some(format!("0x{:x}", ip)),
            sp: Some(format!("0x{:x}", sp)),
            ..StackFrame::new()
        });

        // SAFETY: cursor is valid
        if unsafe { unw_step_remote(&mut cursor) } <= 0 {
            break;
        }
    }

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
    addr_space: &UnwAddrSpace,
    stop_deadline: Instant,
) -> Result<CapturedThreadContext, PtraceError> {
    attach_thread(tid, stop_deadline)?;

    let stack_trace = unwind_remote_thread(tid, addr_space);

    // Best-effort detach: if this fails the thread stays in ptrace-stop, but the
    // receiver exiting will clean it up. Don't discard a good stack trace over it.
    let _ = detach_thread(tid);

    Ok(CapturedThreadContext { stack_trace })
}

/// Maximum time to wait for a single thread to enter ptrace-stop.
const STOP_TIMEOUT_PER_THREAD: Duration = Duration::from_millis(200);

/// Delay between retry attempts that is used as a base for an exponential back-off starting from
/// this value (10ms, 20ms, 40ms).
const RETRY_BASE_DELAY: Duration = Duration::from_millis(10);

/// Maximum number of retry attempts per thread.
const MAX_RETRIES: u32 = 3;

/// Returns true if err is worth retrying.
///
/// EPERM (Yama denial / missing PR_SET_PTRACER) and ESRCH (thread exited)
/// are permanent for the receiver's lifetime and are not retried.
fn is_transient_ptrace_error(err: &PtraceError) -> bool {
    matches!(err, PtraceError::Attach(_, libc::ETIMEDOUT))
}

/// Attempt to capture a thread context, retrying on transient failures.
///
/// Each attempt gets its own `STOP_TIMEOUT_PER_THREAD` budget (capped at the
/// overall deadline) so that a retry after a timeout-induced failure actually
/// has enough time to succeed. On older kernels (CentOS 7 / kernel 3.10)
/// the first attempt can consume its entire budget waiting for registers to
/// become readable; reusing that exhausted deadline would make the retry a
/// no-op.
///
/// A capture that succeeds but produces zero frames is also retried: on a
/// running thread with a confirmed non-zero IP, empty frames indicates a
/// transient issue.
fn capture_with_retry(
    tid: libc::pid_t,
    addr_space: &UnwAddrSpace,
    overall_deadline: Instant,
) -> Option<CapturedThreadContext> {
    for attempt in 0..=MAX_RETRIES {
        let thread_deadline = (Instant::now() + STOP_TIMEOUT_PER_THREAD).min(overall_deadline);

        match capture_thread_context(tid, addr_space, thread_deadline) {
            Ok(ctx) if !ctx.stack_trace.frames.is_empty() => return Some(ctx),
            Ok(_) => {}                                      // 0 frames -- retry
            Err(ref e) if is_transient_ptrace_error(e) => {} // ETIMEDOUT -- retry
            Err(_) => return None,                           // permanent error
        }

        if attempt == MAX_RETRIES {
            break;
        }

        let delay = RETRY_BASE_DELAY * 2u32.saturating_pow(attempt);
        if Instant::now() + delay >= overall_deadline {
            break;
        }
        std::thread::sleep(delay);
    }

    // All attempts produced 0 frames or timed out; return None so the caller
    // records the thread with an incomplete stack rather than frames: [].
    None
}

/// Visit the crashing thread first, then other threads up to the cap or timeout.
///
/// The callback owns each capture, or receives None if attachment or unwinding failed.
/// Each stop also has a STOP_TIMEOUT_PER_THREAD limit. Returns true if threads were left
/// unvisited.
pub(crate) fn stream_thread_contexts<F>(
    parent_pid: libc::pid_t,
    crashing_tid: libc::pid_t,
    max_threads: usize,
    timeout: Duration,
    mut callback: F,
) -> Result<bool, PtraceError>
where
    F: FnMut(libc::pid_t, Option<CapturedThreadContext>),
{
    let overall_deadline = Instant::now() + timeout;
    let tids = enumerate_threads(parent_pid)?;
    let total_eligible = tids.len();
    let mut processed = 0;

    // Create a single address space shared across all threads.  All threads in the
    // same process share the same binary mappings, so the DWARF unwind info that
    // libunwind caches inside the address space is valid for every thread and is
    // reused rather than re-parsed on each iteration.
    let Some(addr_space) = UnwAddrSpace::new() else {
        return Ok(true); // treat as incomplete; nothing was collected
    };

    // Process the crashing thread first so it is never dropped by the cap.
    if crashing_tid != 0 && tids.contains(&crashing_tid) {
        let context = capture_with_retry(crashing_tid, &addr_space, overall_deadline);
        callback(crashing_tid, context);
        processed += 1;
    }

    for tid in tids {
        if tid == crashing_tid {
            continue;
        }
        if Instant::now() >= overall_deadline || processed >= max_threads {
            break;
        }

        let context = capture_with_retry(tid, &addr_space, overall_deadline);
        callback(tid, context);
        processed += 1;
    }

    let incomplete = processed < total_eligible;
    Ok(incomplete)
}

/// Add thread metadata and trim handler frames from the crashing thread's stack.
pub(crate) fn thread_data_from_capture(
    parent_pid: libc::pid_t,
    crashing_tid: libc::pid_t,
    crash_site: Option<(u64, u64)>,
    tid: libc::pid_t,
    captured: Option<CapturedThreadContext>,
) -> ThreadData {
    let (name, state) = read_thread_stat(parent_pid, tid);
    let name = name.unwrap_or_else(|| tid.to_string());

    let mut stack = match captured {
        Some(ctx) => ctx.stack_trace,
        None => StackTrace::new_incomplete(),
    };

    let crashed = tid == crashing_tid;
    if crashed {
        if let Some((ip, sp)) = crash_site {
            drop_frames_above_crash_site(&mut stack, ip, sp);
        }
    }

    ThreadData {
        crashed,
        name,
        stack,
        state,
    }
}

pub(crate) fn parse_hex_address(value: &str) -> Option<u64> {
    u64::from_str_radix(value.trim_start_matches("0x"), 16).ok()
}
/// Remove handler frames above the faulting frame, if the unwind reached it.
///
/// Match both saved registers: stack-pointer ordering cannot identify the faulting frame
/// when the handler uses an alternate signal stack. Leave an unmatched stack intact.
pub(crate) fn drop_frames_above_crash_site(stack: &mut StackTrace, ip: u64, sp: u64) {
    let is_crash_site = |frame: &StackFrame| {
        frame.ip.as_deref().and_then(parse_hex_address) == Some(ip)
            && frame.sp.as_deref().and_then(parse_hex_address) == Some(sp)
    };

    if let Some(crash_site) = stack.frames.iter().position(is_crash_site) {
        stack.frames.drain(..crash_site);
    }
}
/// Read thread name and state from a single `/proc/{pid}/task/{tid}/stat` file.
///
/// The stat file format is: `pid (comm) state ...`
/// `comm` (the thread name) is enclosed between the first `(` and the last `)`
/// The state character immediately follows the closing `)`.
fn read_thread_stat(pid: i32, tid: i32) -> (Option<String>, Option<String>) {
    let content = match std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")) {
        Ok(c) => c,
        Err(_) => return (None, None),
    };

    let Some(name_start) = content.find('(') else {
        return (None, None);
    };
    let Some(name_end) = content.rfind(')') else {
        return (None, None);
    };

    let name = Some(content[name_start + 1..name_end].to_string());
    let state = content[name_end + 1..]
        .split_whitespace()
        .next()
        .map(|s| s.to_string());

    (name, state)
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
    #[cfg_attr(miri, ignore)]
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

        let Some(addr_space) = UnwAddrSpace::new() else {
            eprintln!("skipping ptrace test (UnwAddrSpace::new failed)");
            barrier.wait();
            handle.join().unwrap();
            return;
        };
        match capture_thread_context(tid, &addr_space, Instant::now() + Duration::from_secs(5)) {
            Err(e) => eprintln!("skipping ptrace test (ptrace unavailable): {e}"),
            Ok(ctx) => assert!(
                !ctx.stack_trace.frames.is_empty(),
                "expected at least one frame from a running thread"
            ),
        }

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
            |_tid, _ctx| collected += 1,
        );

        // max_threads=2 but crashing_tid is always included, so up to 3
        assert!(collected <= 3, "collected {collected}, expected <= 3");
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn stream_includes_all_threads() {
        let barrier = Arc::new(Barrier::new(2));
        let b: Arc<Barrier> = Arc::clone(&barrier);
        let (tx, rx) = std::sync::mpsc::channel();

        let handle = std::thread::spawn(move || {
            tx.send(current_tid()).unwrap();
            b.wait();
        });

        let worker_tid = rx.recv().unwrap();

        let mut seen_worker = false;
        let mut seen_self = false;
        let self_tid = current_tid();
        let _ = stream_thread_contexts(
            std::process::id() as libc::pid_t,
            self_tid,
            64,
            Duration::from_secs(5),
            |tid, _ctx| {
                if tid == worker_tid {
                    seen_worker = true;
                }
                if tid == self_tid {
                    seen_self = true;
                }
            },
        );

        assert!(seen_worker, "worker thread should appear in callbacks");
        assert!(seen_self, "current thread should appear in callbacks");

        barrier.wait();
        handle.join().unwrap();
    }
}
