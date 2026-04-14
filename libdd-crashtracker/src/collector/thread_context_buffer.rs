// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Pre-allocated per-thread ucontext capture buffer for multi-thread stack collection.
//! This module is compiled for Linux only
//!
//! # Overview
//!
//! When the crashtracker handles a fatal signal, it needs to unwind the stacks of all
//! threads in the process not just the crashing thread. The crashing thread context
//! is available directly from the signal handler ucontext parameter. For all other
//! threads, we use the following mechanism:
//!
//! 1. At init() time, a buffer of ThreadContextSlots is heap-allocated and stored in a global
//!    atomic pointer. A secondary signal (SIGUSR2) handler is registered that writes the calling
//!    thread ucontext into its reserved slot.
//!
//! 2. When a crash occurs (inside the primary signal handler), collect_thread_contexts is called.
//!    This function enumerates all live thread IDs from /proc/self/task/, claims a slot for each
//!    non-crashing thread, sends SIGUSR2 to it via tgkill, and spin-waits for each slot to be
//!    marked ready.
//!
//! 3. After the collector child is forked, iter_collected_contexts provides an iterator over all
//!    slots that were successfully filled. The collector uses these contexts to unwind and emit a
//!    stack trace for each thread.
//!
//! # Async-signal-safety
//!
//! All code executed inside the primary crash signal handler (step 2) uses only:
//! - Raw Linux syscalls (open, getdents64, tgkill, close, clock_gettime).
//! - Atomic loads/stores/CAS on the pre-allocated buffer.
//!
//! The SIGUSR2 handler uses:
//! - SYS_gettid syscall
//! - A linear scan of the pre-allocated buffer (no allocation)
//! - ptr::copy_nonoverlapping to write the ucontext
//! - An AtomicBool release store

use libc::{siginfo_t, ucontext_t};
use nix::sys::signal::{SigAction, SigHandler};
use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};

/// Maximum number of threads whose contexts can be captured simultaneously.
pub const MAX_TRACKED_THREADS: usize = 128;

/// A single slot in the thread context buffer.
///
/// Memory layout invariants:
/// - `tid == 0` means the slot is free.
/// - `tid != 0` means the slot has been claimed for that TID.
/// - `ready == true` (with Acquire load) implies `ctx` is fully written.
pub struct ThreadContextSlot {
    /// Kernel thread ID this slot is reserved for.  0 = free.
    pub tid: AtomicI32,
    /// Set to `true` (Release) after the context has been written.
    pub ready: AtomicBool,
    /// The saved ucontext_t for the thread.  Valid only when `ready == true`.
    pub ctx: UnsafeCell<MaybeUninit<ucontext_t>>,
}

// SAFETY: Concurrent access is coordinated via the atomic tid/ready fields.
// Only one writer (the SIGUSR2 handler) touches ctx per slot, and the reader
// (collector child after fork) observes ready with Acquire ordering before
// accessing ctx.
unsafe impl Sync for ThreadContextSlot {}
unsafe impl Send for ThreadContextSlot {}

impl ThreadContextSlot {
    fn new() -> Self {
        Self {
            tid: AtomicI32::new(0),
            ready: AtomicBool::new(false),
            ctx: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Reset the slot to the free state.  Call before re-use (e.g. after fork).
    pub fn reset(&self) {
        self.ready.store(false, Ordering::SeqCst);
        self.tid.store(0, Ordering::SeqCst);
    }
}

/// Heap-allocated array of slots, stored behind a global atomic pointer so that
/// it can be accessed from signal handlers without locks.
pub struct ThreadContextBuffer {
    pub slots: Vec<ThreadContextSlot>,
}

// SAFETY: Each slot is independently synchronized; the Vec itself is never resized
// after initialization.
unsafe impl Sync for ThreadContextBuffer {}

/// Global pointer to the heap-allocated buffer.  Null until
/// `init_thread_context_buffer` has been called.
static BUFFER: AtomicPtr<ThreadContextBuffer> = AtomicPtr::new(ptr::null_mut());

/// The `SigAction` that was in place for SIGUSR2 before we installed our handler.
/// Written once at init time by `store_prev_sigusr2_action`; read from the signal
/// handler when chaining.  Null until `register_thread_context_signal_handler` runs.
static PREV_SIGUSR2_ACTION: AtomicPtr<SigAction> = AtomicPtr::new(ptr::null_mut());

/// Store the previous SIGUSR2 `SigAction` so `handle_collect_context_signal` can
/// chain to it.  Called once from `register_thread_context_signal_handler`.
pub(crate) fn store_prev_sigusr2_action(old: SigAction) {
    let ptr = Box::into_raw(Box::new(old));
    let prev = PREV_SIGUSR2_ACTION.swap(ptr, Ordering::SeqCst);
    if !prev.is_null() {
        // SAFETY: prev was created by Box::into_raw in a previous call to this
        // function and has not been freed since.
        unsafe { drop(Box::from_raw(prev)) };
    }
}

/// Allocate the thread context buffer.
///
/// Must be called once from a non-signal context (inside `init()`).
/// Subsequent calls are a no-op.
pub fn init_thread_context_buffer(capacity: usize) {
    let capacity = capacity.clamp(1, MAX_TRACKED_THREADS);

    let buf = Box::new(ThreadContextBuffer {
        slots: (0..capacity).map(|_| ThreadContextSlot::new()).collect(),
    });
    let ptr = Box::into_raw(buf);

    // If already initialized, keep the original and free the new allocation.
    if BUFFER
        .compare_exchange(ptr::null_mut(), ptr, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // SAFETY: We just created this Box and the CAS rejected it, so no one else
        // has a reference to it.
        unsafe { drop(Box::from_raw(ptr)) };
    }
}

/// Return a reference to the buffer, or `None` if not yet initialised.
pub(crate) fn get_buffer() -> Option<&'static ThreadContextBuffer> {
    let ptr = BUFFER.load(Ordering::SeqCst);
    if ptr.is_null() {
        None
    } else {
        // SAFETY: The pointer was stored by `init_thread_context_buffer` via
        // `Box::into_raw` and is never freed until the process exits.
        Some(unsafe { &*ptr })
    }
}

/// Reset all slots to the free state.
///
/// Should be called in `on_fork()` so that child processes start with a clean buffer.
pub fn reset_thread_context_buffer() {
    if let Some(buf) = get_buffer() {
        for slot in &buf.slots {
            slot.reset();
        }
    }
}

/// SA_SIGINFO handler for SIGUSR2.
///
/// Locates the pre-claimed slot for the calling thread, copies the kernel-saved
/// `ucontext_t` into it, and marks the slot ready. After doing so it chains to
/// whatever `sigaction` was installed for SIGUSR2 before we registered this handler,
/// subject to the rules below.
///
/// # Chaining rules
///
/// - `SIG_DFL` (0): Do not chain. For SIGUSR2 the default action is process termination.  We send
///   this signal intentionally via `tgkill`, so invoking the default would be wrong.
/// - `SIG_IGN` (1): Do not chain. The previous code explicitly ignored SIGUSR2; honouring that
///   means doing nothing.
/// - Any function pointer: Chain. The previous handler was user-installed and must receive the
///   signal even when we intercept it.
///
/// We mark the slot ready before chaining so the crashtracker's spin-wait can
/// proceed promptly even if the previous handler blocks.
///
/// SAFETY: All operations are async-signal-safe (raw syscall + atomic loads/stores +
/// `copy_nonoverlapping` on a pre-allocated static buffer + conditional function call
/// through a previously-valid function pointer).
pub(crate) extern "C" fn handle_collect_context_signal(
    signum: i32,
    sig_info: *mut siginfo_t,
    ucontext: *mut libc::c_void,
) {
    // SAFETY: SYS_gettid is always async-signal-safe.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) as i32 };

    if let Some(buf) = get_buffer() {
        for slot in &buf.slots {
            if slot.tid.load(Ordering::SeqCst) == tid {
                if !ucontext.is_null() {
                    // SAFETY: ucontext is the kernel saved register state, valid for the
                    // duration of the signal handler. The slot ctx field is not accessed
                    // by anyone else until ready is observed as true.
                    unsafe {
                        ptr::copy_nonoverlapping(
                            ucontext as *const ucontext_t,
                            (*slot.ctx.get()).as_mut_ptr(),
                            1,
                        );
                    }
                }
                // Release store: guarantees the ctx write above is visible to any
                // thread that subsequently loads ready with Acquire ordering.
                slot.ready.store(true, Ordering::Release);
                break;
            }
        }
    }

    // Chain to the previous SIGUSR2 handler if one was installed.
    //
    // SAFETY: PREV_SIGUSR2_ACTION is written once at init time and never freed or
    // modified afterwards, so the pointer is always valid for the lifetime of the
    // process once it is non-null.
    let prev_ptr = PREV_SIGUSR2_ACTION.load(Ordering::SeqCst);
    if prev_ptr.is_null() {
        return;
    }
    let prev = unsafe { &*prev_ptr };

    // SigAction::handler() decodes sa_sigaction + sa_flags into a SigHandler enum,
    // mirroring the same dispatch pattern used by chain_signal_handler() for the
    // primary crash handlers.
    match prev.handler() {
        SigHandler::SigDfl | SigHandler::SigIgn => {
            // SIG_DFL for SIGUSR2 = process termination; do not chain (we sent
            // this signal intentionally via tgkill).
            // SIG_IGN = no-op; nothing to call.
        }
        SigHandler::Handler(f) => f(signum),
        SigHandler::SigAction(f) => f(signum, sig_info, ucontext),
    }
}

/// Read a monotonic timestamp in nanoseconds.  Uses clock_gettime, which is
/// listed as async-signal-safe by POSIX.
fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime is async-signal-safe; ts is a valid stack allocation.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

/// Enumerate live thread IDs from /proc/self/task/ using only raw Linux syscalls.
///
/// Results are written into `out`; returns the number of TIDs written.  This
/// function is async-signal-safe on Linux: it uses open(2), SYS_getdents64,
/// and close(2), none of which touch any library-managed state.
fn enumerate_tids(out: &mut [libc::pid_t]) -> usize {
    // SAFETY: The byte string is a valid null-terminated C path.
    let fd = unsafe {
        libc::open(
            c"/proc/self/task".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return 0;
    }

    let mut buf = [0u8; 4096];
    let mut count = 0;

    loop {
        // SAFETY: SYS_getdents64 is a raw kernel entry; buf is a valid stack buffer.
        let n = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                fd as libc::c_long,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len() as libc::c_long,
            )
        };
        if n <= 0 {
            break;
        }
        let n = n as usize;
        let mut offset = 0usize;
        // linux_dirent64 layout: d_ino(8) d_off(8) d_reclen(2) d_type(1) d_name(var)
        while offset + 19 <= n {
            let reclen = u16::from_ne_bytes([buf[offset + 16], buf[offset + 17]]) as usize;
            if reclen == 0 || offset + reclen > n {
                break;
            }
            let name_bytes = &buf[offset + 19..offset + reclen];
            if let Some(tid) = parse_tid(name_bytes) {
                if count < out.len() {
                    out[count] = tid;
                    count += 1;
                }
            }
            offset += reclen;
        }
    }

    // SAFETY: fd is a valid file descriptor opened above.
    unsafe { libc::close(fd) };
    count
}

/// Parse a decimal thread ID from a null-terminated directory entry name.
/// Returns None for "." and ".." and any non-numeric name.
fn parse_tid(bytes: &[u8]) -> Option<libc::pid_t> {
    let mut result: libc::pid_t = 0;
    let mut has_digits = false;
    for &b in bytes {
        if b == 0 {
            break;
        }
        if !b.is_ascii_digit() {
            return None;
        }
        has_digits = true;
        result = result
            .wrapping_mul(10)
            .wrapping_add((b - b'0') as libc::pid_t);
    }
    if has_digits {
        Some(result)
    } else {
        None
    }
}

/// Send SIGUSR2 to every thread in the process except the crashing thread,
/// then wait for each contacted thread to write its ucontext into its slot.
///
/// Called from the primary crash signal handler before fork()-ing the collector
/// child. Must be async-signal-safe.
///
/// - `crashing_tid`  TID of the thread that received the fatal signal (skipped).
/// - `max_threads`   cap on how many threads to sample.
/// - `timeout_ms`    maximum total wait time in milliseconds.
pub fn collect_thread_contexts(crashing_tid: libc::pid_t, max_threads: usize, timeout_ms: u64) {
    let Some(buf) = get_buffer() else { return };

    let capacity = buf.slots.len().min(max_threads);
    // SAFETY: getpid() is async-signal-safe.
    let pid = unsafe { libc::getpid() };

    let mut tid_list = [0i32; MAX_TRACKED_THREADS];
    let count = enumerate_tids(&mut tid_list[..capacity]);

    let mut claimed: usize = 0;
    for &tid in tid_list.iter().take(count) {
        if tid == 0 || tid == crashing_tid {
            continue;
        }
        // Claim a free slot for this TID via CAS.
        let mut claimed_slot = false;
        for slot in &buf.slots {
            if slot
                .tid
                .compare_exchange(0, tid, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                claimed_slot = true;
                claimed += 1;
                break;
            }
        }
        if claimed_slot {
            // SAFETY: tgkill is async-signal-safe.
            unsafe {
                libc::syscall(
                    libc::SYS_tgkill,
                    pid as libc::c_long,
                    tid as libc::c_long,
                    libc::SIGUSR2 as libc::c_long,
                )
            };
        }
    }

    if claimed == 0 {
        return;
    }

    // Spin-wait for all claimed slots to become ready.
    let timeout_ns = timeout_ms as i64 * 1_000_000;
    let deadline = monotonic_ns() + timeout_ns;

    loop {
        let ready = buf
            .slots
            .iter()
            .filter(|s| s.tid.load(Ordering::Relaxed) != 0)
            .filter(|s| s.ready.load(Ordering::Acquire))
            .count();
        if ready >= claimed {
            break;
        }
        if monotonic_ns() >= deadline {
            break;
        }
        core::hint::spin_loop();
    }
}

/// An iterator item produced by iter_collected_contexts.
pub struct CollectedContext {
    pub tid: libc::pid_t,
    /// Pointer to the captured ucontext_t.  Valid for the lifetime of the
    /// buffer (which persists across fork via COW mapping).
    pub ucontext: *const ucontext_t,
}

/// Iterate over all slots that were successfully filled by the SIGUSR2 handler.
///
/// Safe to call in the collector child after fork.  The buffer is readable via
/// the copy-on-write memory mapping inherited from the parent.
pub fn iter_collected_contexts() -> impl Iterator<Item = CollectedContext> {
    get_buffer()
        .map(|buf| buf.slots.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter(|s| s.tid.load(Ordering::Acquire) != 0 && s.ready.load(Ordering::Acquire))
        .map(|s| {
            let tid = s.tid.load(Ordering::Acquire);
            // SAFETY: ready was observed as true with Acquire ordering, synchronising
            // with the Release store in the SIGUSR2 handler.
            let ucontext = unsafe { (*s.ctx.get()).as_ptr() };
            CollectedContext { tid, ucontext }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// Claim the first free slot for `tid`.  Returns the slot index on success.
    fn claim_slot(buf: &ThreadContextBuffer, tid: i32) -> Option<usize> {
        buf.slots.iter().enumerate().find_map(|(i, slot)| {
            slot.tid
                .compare_exchange(0, tid, Ordering::SeqCst, Ordering::Relaxed)
                .ok()
                .map(|_| i)
        })
    }

    /// Install `handle_collect_context_signal` as the SIGUSR2 handler and return
    /// the previous `sigaction` so the caller can restore it.
    fn install_context_handler() -> libc::sigaction {
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_sigaction = handle_collect_context_signal as *const () as usize;
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART | libc::SA_NODEFER;
        let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
        unsafe { libc::sigaction(libc::SIGUSR2, &sa, &mut old) };
        old
    }

    fn restore_handler(old: &libc::sigaction) {
        unsafe { libc::sigaction(libc::SIGUSR2, old, std::ptr::null_mut()) };
    }

    fn current_tid() -> i32 {
        unsafe { libc::syscall(libc::SYS_gettid) as i32 }
    }

    #[test]
    fn test_parse_tid_valid() {
        assert_eq!(parse_tid(b"12345\0"), Some(12345));
        assert_eq!(parse_tid(b"1\0"), Some(1));
        assert_eq!(parse_tid(b"0\0"), Some(0));
    }

    #[test]
    fn test_parse_tid_invalid() {
        assert_eq!(parse_tid(b".\0"), None);
        assert_eq!(parse_tid(b"..\0"), None);
        assert_eq!(parse_tid(b"abc\0"), None);
        assert_eq!(parse_tid(b"\0"), None);
    }

    #[test]
    fn test_init_buffer() {
        init_thread_context_buffer(4);
        let buf = get_buffer().expect("buffer should be initialized");
        assert_eq!(buf.slots.len(), 4);
        for slot in &buf.slots {
            assert_eq!(slot.tid.load(Ordering::SeqCst), 0);
            assert!(!slot.ready.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn test_reset_buffer() {
        init_thread_context_buffer(4);
        let buf = get_buffer().unwrap();
        buf.slots[0].tid.store(42, Ordering::SeqCst);
        buf.slots[0].ready.store(true, Ordering::SeqCst);

        reset_thread_context_buffer();

        assert_eq!(buf.slots[0].tid.load(Ordering::SeqCst), 0);
        assert!(!buf.slots[0].ready.load(Ordering::SeqCst));
    }

    #[test]
    fn test_signal_handler_writes_context_and_sets_ready() {
        init_thread_context_buffer(16);
        reset_thread_context_buffer();

        let tid = current_tid();
        let buf = get_buffer().unwrap();
        let idx = claim_slot(buf, tid).expect("should find a free slot");

        let mut fake_ctx: libc::ucontext_t = unsafe { std::mem::zeroed() };
        fake_ctx.uc_flags = 0x1234_5678;

        handle_collect_context_signal(
            libc::SIGUSR2,
            std::ptr::null_mut(),
            &mut fake_ctx as *mut libc::ucontext_t as *mut libc::c_void,
        );

        let slot = &buf.slots[idx];
        assert!(
            slot.ready.load(Ordering::Acquire),
            "slot should be marked ready after handler runs"
        );
        // Verify the context bytes were actually copied.
        let copied_flags = unsafe { (*(*slot.ctx.get()).as_ptr()).uc_flags };
        assert_eq!(
            copied_flags, 0x1234_5678,
            "uc_flags sentinel should survive the copy"
        );

        reset_thread_context_buffer();
    }

    /// Even with a null ucontext (kernel gave us nothing), the slot must be
    /// marked ready so the spin-wait in collect_thread_contexts can complete.
    #[test]
    fn test_signal_handler_null_ucontext_still_sets_ready() {
        init_thread_context_buffer(16);
        reset_thread_context_buffer();

        let tid = current_tid();
        let buf = get_buffer().unwrap();
        let idx = claim_slot(buf, tid).expect("should find a free slot");

        handle_collect_context_signal(
            libc::SIGUSR2,
            std::ptr::null_mut(),
            std::ptr::null_mut(), // null ucontext
        );

        assert!(
            buf.slots[idx].ready.load(Ordering::Acquire),
            "null ucontext should still set ready so the caller doesn't stall"
        );

        reset_thread_context_buffer();
    }

    /// The handler must silently do nothing when the current TID has no slot
    /// claimed for it
    #[test]
    fn test_signal_handler_ignores_unclaimed_tid() {
        init_thread_context_buffer(16);
        reset_thread_context_buffer(); // all slots tid=0, ready=false

        let mut fake_ctx: libc::ucontext_t = unsafe { std::mem::zeroed() };
        // Call with no slot pre-claimed for the current TID.
        handle_collect_context_signal(
            libc::SIGUSR2,
            std::ptr::null_mut(),
            &mut fake_ctx as *mut libc::ucontext_t as *mut libc::c_void,
        );

        let buf = get_buffer().unwrap();
        let tid = current_tid();
        for slot in &buf.slots {
            assert!(
                !slot.ready.load(Ordering::Acquire),
                "no slot should be ready when TID was not pre-claimed"
            );
            assert_ne!(
                slot.tid.load(Ordering::Acquire),
                tid,
                "handler must not claim a slot for an unclaimed TID"
            );
        }

        reset_thread_context_buffer();
    }

    /// The crashing thread shouldn't appear as one of the captured threads
    #[test]
    #[cfg_attr(miri, ignore)] // sigaction is not supported by Miri
    fn test_collect_never_captures_crashing_tid() {
        init_thread_context_buffer(16);
        reset_thread_context_buffer();

        let old_sa = install_context_handler();
        let crashing_tid = unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t };

        collect_thread_contexts(crashing_tid, 16, 300);

        let buf = get_buffer().unwrap();
        for slot in &buf.slots {
            assert_ne!(
                slot.tid.load(Ordering::Acquire),
                crashing_tid as i32,
                "crashing TID ({crashing_tid}) must not appear in any slot"
            );
        }

        restore_handler(&old_sa);
        reset_thread_context_buffer();
    }

    /// Two background threads sleeping in a syscall should both respond to
    /// SIGUSR2 and have their contexts captured before the timeout fires.
    ///
    /// We record each worker's TID before they block and verify that exactly
    /// those TIDs appear in the buffer afterwards.
    #[test]
    #[cfg_attr(miri, ignore)] // sigaction is not supported by Miri
    fn test_collect_captures_background_threads() {
        init_thread_context_buffer(16);
        reset_thread_context_buffer();

        let old_sa = install_context_handler();

        let (tx, rx) = std::sync::mpsc::channel::<i32>();
        let keep = Arc::new(std::sync::atomic::AtomicBool::new(true));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let tx = tx.clone();
                let keep = Arc::clone(&keep);
                std::thread::spawn(move || {
                    tx.send(current_tid()).unwrap();
                    while keep.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                })
            })
            .collect();

        // Receive exactly one TID per worker
        drop(tx);
        let worker_tids: Vec<i32> = (0..2).map(|_| rx.recv().unwrap()).collect();
        std::thread::sleep(Duration::from_millis(50));

        let crashing_tid = unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t };
        collect_thread_contexts(crashing_tid, 16, 500);

        let buf = get_buffer().unwrap();

        // Both workers must have responded and their contexts must be in the buffer.
        for &wtid in &worker_tids {
            let found = buf
                .slots
                .iter()
                .any(|s| s.tid.load(Ordering::Acquire) == wtid && s.ready.load(Ordering::Acquire));
            assert!(
                found,
                "worker TID {wtid} should have its context captured in the buffer"
            );
        }

        // The crashing TID must still be absent.
        assert!(
            buf.slots
                .iter()
                .all(|s| s.tid.load(Ordering::Acquire) != crashing_tid as i32),
            "crashing TID ({crashing_tid}) must not be in the buffer"
        );

        keep.store(false, Ordering::Relaxed);
        for h in handles {
            let _ = h.join();
        }
        restore_handler(&old_sa);
        reset_thread_context_buffer();
    }

    /// Build a nix `SigAction` from the given `SigHandler` and store it as the
    /// previous SIGUSR2 action so the handler will chain to it.
    fn set_prev_action(handler: SigHandler) {
        use nix::sys::signal::{SaFlags, SigSet};
        let sa = SigAction::new(handler, SaFlags::empty(), SigSet::empty());
        store_prev_sigusr2_action(sa);
    }

    /// After calling `handle_collect_context_signal`, a previously-installed
    /// `SA_SIGINFO` function handler must be forwarded the signal
    #[test]
    #[cfg_attr(miri, ignore)] // SigSet::empty() calls sigemptyset, unsupported by Miri
    fn test_chain_to_previous_sa_siginfo_handler() {
        init_thread_context_buffer(16);
        reset_thread_context_buffer();

        // Flag set by the previous handler to prove it was called.
        static PREV_CALLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        PREV_CALLED.store(false, Ordering::SeqCst);

        extern "C" fn fake_sa_siginfo_handler(
            _signum: i32,
            _sig_info: *mut libc::siginfo_t,
            _ucontext: *mut libc::c_void,
        ) {
            PREV_CALLED.store(true, Ordering::SeqCst);
        }

        set_prev_action(SigHandler::SigAction(fake_sa_siginfo_handler));

        let tid = current_tid();
        let buf = get_buffer().unwrap();
        let idx = claim_slot(buf, tid).expect("should find a free slot");

        let mut fake_ctx: libc::ucontext_t = unsafe { std::mem::zeroed() };
        fake_ctx.uc_flags = 0xABCD;
        handle_collect_context_signal(
            libc::SIGUSR2,
            std::ptr::null_mut(),
            &mut fake_ctx as *mut libc::ucontext_t as *mut libc::c_void,
        );

        assert!(
            buf.slots[idx].ready.load(Ordering::Acquire),
            "our slot should be ready"
        );
        assert!(
            PREV_CALLED.load(Ordering::SeqCst),
            "previous SA_SIGINFO handler must have been called"
        );

        set_prev_action(SigHandler::SigDfl); // clear; won't be chained
        reset_thread_context_buffer();
    }

    #[test]
    #[cfg_attr(miri, ignore)] // SigSet::empty() calls sigemptyset, unsupported by Miri
    fn test_chain_to_previous_plain_handler() {
        init_thread_context_buffer(16);
        reset_thread_context_buffer();

        static PREV_CALLED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        PREV_CALLED.store(false, Ordering::SeqCst);

        extern "C" fn fake_plain_handler(_signum: i32) {
            PREV_CALLED.store(true, Ordering::SeqCst);
        }

        set_prev_action(SigHandler::Handler(fake_plain_handler));

        let tid = current_tid();
        let buf = get_buffer().unwrap();
        let idx = claim_slot(buf, tid).expect("should find a free slot");

        handle_collect_context_signal(libc::SIGUSR2, std::ptr::null_mut(), std::ptr::null_mut());

        assert!(
            buf.slots[idx].ready.load(Ordering::Acquire),
            "our slot should be ready"
        );
        assert!(
            PREV_CALLED.load(Ordering::SeqCst),
            "previous plain handler must have been called"
        );

        set_prev_action(SigHandler::SigDfl);
        reset_thread_context_buffer();
    }

    /// `SigHandler::SigDfl` must not be chained.  For SIGUSR2 the default action is
    /// process termination.  If this test completes without a crash, the guard works.
    #[test]
    #[cfg_attr(miri, ignore)] // SigSet::empty() calls sigemptyset, unsupported by Miri
    fn test_no_chain_to_sig_dfl() {
        init_thread_context_buffer(16);
        reset_thread_context_buffer();

        set_prev_action(SigHandler::SigDfl);

        let tid = current_tid();
        let buf = get_buffer().unwrap();
        let idx = claim_slot(buf, tid).expect("should find a free slot");

        // Must not crash.
        handle_collect_context_signal(libc::SIGUSR2, std::ptr::null_mut(), std::ptr::null_mut());

        assert!(
            buf.slots[idx].ready.load(Ordering::Acquire),
            "slot should be ready even when previous action is SigDfl"
        );

        reset_thread_context_buffer();
    }

    /// `SigHandler::SigIgn` must not be chained.  If this test completes, the guard
    /// is working correctly.
    #[test]
    #[cfg_attr(miri, ignore)] // SigSet::empty() calls sigemptyset, unsupported by Miri
    fn test_no_chain_to_sig_ign() {
        init_thread_context_buffer(16);
        reset_thread_context_buffer();

        set_prev_action(SigHandler::SigIgn);

        let tid = current_tid();
        let buf = get_buffer().unwrap();
        let idx = claim_slot(buf, tid).expect("should find a free slot");

        // Must not crash.
        handle_collect_context_signal(libc::SIGUSR2, std::ptr::null_mut(), std::ptr::null_mut());

        assert!(
            buf.slots[idx].ready.load(Ordering::Acquire),
            "slot should be ready even when previous action is SigIgn"
        );

        set_prev_action(SigHandler::SigDfl);
        reset_thread_context_buffer();
    }
}
