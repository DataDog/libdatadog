// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! `ScopedFailingAllocator`: a global allocator proxy that can return null for a chosen
//! allocation of a scope.
//!
//! Outside a scope, every call is forwarded unchanged. Inside a scope (per thread), every
//! allocation attempt (`alloc`, `alloc_zeroed`, `realloc`) gets an ordinal, is recorded in a
//! preallocated trace, and fails when the scope's policy says so. `dealloc` is never counted and
//! never fails. While a scope is active, allocations of the other threads are only counted per
//! thread: they are forwarded unchanged and never fail.
//!
//! The allocator callbacks use constant-initialized thread-locals and atomics. When a failure is
//! selected, the worker records a diagnostic stack; allocations made by that stack walk bypass
//! tracing and injection, so diagnostics cannot change the observed outcome.

use std::alloc::{GlobalAlloc, Layout};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Maximum number of events kept in the trace (and in the expected trace).
pub const TRACE_CAPACITY: usize = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Count and record, never fail.
    Observe,
    /// Fail only the allocation with this ordinal.
    FailOnceAt(u64),
    /// Fail every allocation from this ordinal on.
    FailFrom(u64),
}

impl Policy {
    fn fails(self, ordinal: u64) -> bool {
        match self {
            Policy::Observe => false,
            Policy::FailOnceAt(n) => ordinal == n,
            Policy::FailFrom(n) => ordinal >= n,
        }
    }

    fn target(self) -> u64 {
        match self {
            Policy::Observe => u64::MAX,
            Policy::FailOnceAt(n) | Policy::FailFrom(n) => n,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Domain {
    Global = 0,
    ArenaRequest = 1,
    ArenaBacking = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Operation {
    Alloc = 0,
    AllocZeroed = 1,
    Realloc = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub ordinal: u64,
    pub domain: Domain,
    pub operation: Operation,
    /// Requested size (the new size for `realloc`).
    pub size: u64,
    pub align: u64,
    pub thread: u32,
}

impl Event {
    /// The fields that must match between runs of the same scenario (the thread ID may not).
    pub fn same_request(&self, other: &Event) -> bool {
        self.ordinal == other.ordinal
            && self.domain == other.domain
            && self.operation == other.operation
            && self.size == other.size
            && self.align == other.align
    }
}

struct Slot {
    ordinal_thread: AtomicU64,
    size: AtomicU64,
    align_domain_op: AtomicU64,
}

impl Slot {
    const fn new() -> Self {
        Slot {
            ordinal_thread: AtomicU64::new(0),
            size: AtomicU64::new(0),
            align_domain_op: AtomicU64::new(0),
        }
    }

    fn store(&self, ev: &Event) {
        self.ordinal_thread
            .store((ev.ordinal << 32) | u64::from(ev.thread), Ordering::Relaxed);
        self.size.store(ev.size, Ordering::Relaxed);
        self.align_domain_op.store(
            (ev.align << 16) | (u64::from(ev.domain as u8) << 8) | u64::from(ev.operation as u8),
            Ordering::Relaxed,
        );
    }

    fn load(&self) -> Event {
        let ordinal_thread = self.ordinal_thread.load(Ordering::Relaxed);
        let align_domain_op = self.align_domain_op.load(Ordering::Relaxed);
        Event {
            ordinal: ordinal_thread >> 32,
            thread: (ordinal_thread & 0xffff_ffff) as u32,
            size: self.size.load(Ordering::Relaxed),
            align: align_domain_op >> 16,
            domain: match (align_domain_op >> 8) & 0xff {
                1 => Domain::ArenaRequest,
                2 => Domain::ArenaBacking,
                _ => Domain::Global,
            },
            operation: match align_domain_op & 0xff {
                0 => Operation::Alloc,
                1 => Operation::AllocZeroed,
                _ => Operation::Realloc,
            },
        }
    }
}

static TRACE: [Slot; TRACE_CAPACITY] = [const { Slot::new() }; TRACE_CAPACITY];
static TRACE_LEN: AtomicUsize = AtomicUsize::new(0);
static TRACE_OVERFLOW: AtomicBool = AtomicBool::new(false);

/// The baseline trace a worker must reproduce before the failure is injected.
static EXPECTED: [Slot; TRACE_CAPACITY] = [const { Slot::new() }; TRACE_CAPACITY];
static EXPECTED_LEN: AtomicUsize = AtomicUsize::new(0);
static CHECK_EXPECTED: AtomicBool = AtomicBool::new(false);
/// First ordinal whose event differs from the expected trace (`u64::MAX`: none).
static DIVERGED_AT: AtomicU64 = AtomicU64::new(u64::MAX);

static INJECTED: AtomicU64 = AtomicU64::new(0);
static NEXT_THREAD_ID: AtomicU32 = AtomicU32::new(1);

/// Number of threads currently inside a scope.
static ACTIVE_SCOPES: AtomicU32 = AtomicU32::new(0);

/// Maximum number of other threads whose allocations are counted while a scope is active.
pub const OTHER_THREAD_CAPACITY: usize = 16;

/// Allocations made by threads outside any scope while another thread is in one. They are
/// counted per thread and never fail: they are reported, not enumerated.
struct OtherThread {
    thread: AtomicU32,
    count: AtomicU64,
    bytes: AtomicU64,
}

static OTHER_THREADS: [OtherThread; OTHER_THREAD_CAPACITY] = [const {
    OtherThread {
        thread: AtomicU32::new(0),
        count: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
    }
}; OTHER_THREAD_CAPACITY];
static OTHER_THREADS_OVERFLOW: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    static POLICY: Cell<Policy> = const { Cell::new(Policy::Observe) };
    static ORDINAL: Cell<u64> = const { Cell::new(0) };
    static THREAD_ID: Cell<u32> = const { Cell::new(0) };
    static RECORDING_STACK: Cell<bool> = const { Cell::new(false) };
}

/// Debugger breakpoint target: called just before the allocator returns null.
#[unsafe(no_mangle)]
#[inline(never)]
pub fn alloc_chaos_injection_hook(ordinal: u64) {
    std::hint::black_box(ordinal);
}

static WRITE_STACK: AtomicBool = AtomicBool::new(false);

/// Workers write the stack of their first injected failure to stderr.
pub fn write_stack_on_first_failure() {
    WRITE_STACK.store(true, Ordering::Relaxed);
}

/// Maximum number of return addresses written for the first injected failure.
const STACK_FRAMES: usize = 128;
pub const STACK_BASE_PREFIX: &str = "alloc-chaos-base ";
pub const STACK_FRAME_PREFIX: &str = "alloc-chaos-frame ";

/// Writes `prefix`, `value` in hexadecimal and a newline to stderr, without allocating.
fn write_hex_line(prefix: &str, value: usize) {
    use std::io::Write;
    let mut line = [0u8; 64];
    let prefix = prefix.as_bytes();
    line[..prefix.len()].copy_from_slice(prefix);
    let mut len = prefix.len();
    let digits = (usize::BITS as usize / 4).max(1);
    let mut started = false;
    for shift in (0..digits).rev() {
        let nibble = (value >> (shift * 4)) & 0xf;
        if nibble != 0 || started || shift == 0 {
            started = true;
            line[len] = b"0123456789abcdef"[nibble];
            len += 1;
        }
    }
    line[len] = b'\n';
    let _ = std::io::stderr().write_all(&line[..=len]);
}

/// Writes the return addresses of the current stack to stderr, with the address of
/// [`alloc_chaos_injection_hook`] so the controller can map them into its own address space.
/// Written right away: the process may abort just after.
fn write_stack() {
    write_hex_line(
        STACK_BASE_PREFIX,
        alloc_chaos_injection_hook as *const () as usize,
    );
    let mut frames = 0;
    // SAFETY: the closure only formats into a stack buffer and writes to stderr. Concurrent
    // unsynchronized traces are allowed on the supported platforms.
    unsafe {
        backtrace::trace_unsynchronized(|frame| {
            write_hex_line(STACK_FRAME_PREFIX, frame.ip() as usize);
            frames += 1;
            frames < STACK_FRAMES
        });
    }
}

fn thread_id() -> u32 {
    THREAD_ID
        .try_with(|id| {
            if id.get() == 0 {
                id.set(NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed));
            }
            id.get()
        })
        .unwrap_or(0)
}

/// Observer for the `libdd-profiling` arena proxy. `domain` and `op` use that proxy's codes.
#[cfg_attr(not(allocation_chaos_test), allow(dead_code))]
pub fn observe_allocation(domain: u8, op: u8, size: usize, align: usize) -> bool {
    let domain = match domain {
        1 => Domain::ArenaRequest,
        2 => Domain::ArenaBacking,
        _ => Domain::Global,
    };
    let operation = match op {
        0 => Operation::Alloc,
        1 => Operation::AllocZeroed,
        _ => Operation::Realloc,
    };
    on_allocation(domain, operation, size, align)
}

/// Same as [`observe_allocation`], for the harness's own arena wrappers.
pub fn observe(domain: Domain, operation: Operation, size: usize, align: usize) -> bool {
    on_allocation(domain, operation, size, align)
}

fn count_other_thread(size: usize) {
    let thread = thread_id();
    if thread == 0 {
        // The thread-local is gone: the thread is exiting.
        OTHER_THREADS_OVERFLOW.fetch_add(1, Ordering::Relaxed);
        return;
    }
    for slot in &OTHER_THREADS {
        let owner =
            match slot
                .thread
                .compare_exchange(0, thread, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => thread,
                Err(owner) => owner,
            };
        if owner == thread {
            slot.count.fetch_add(1, Ordering::Relaxed);
            slot.bytes.fetch_add(size as u64, Ordering::Relaxed);
            return;
        }
    }
    OTHER_THREADS_OVERFLOW.fetch_add(1, Ordering::Relaxed);
}

/// Counts and records one allocation attempt of the current thread. Returns `true` if it must
/// fail. Returns `false` outside a scope.
fn on_allocation(domain: Domain, operation: Operation, size: usize, align: usize) -> bool {
    if RECORDING_STACK.try_with(Cell::get).unwrap_or(false) {
        return false;
    }
    let Ok(Some((policy, ordinal))) = DEPTH.try_with(|depth| {
        if depth.get() == 0 {
            return None;
        }
        let policy = POLICY.with(Cell::get);
        let ordinal = ORDINAL.with(|o| {
            let current = o.get();
            o.set(current + 1);
            current
        });
        Some((policy, ordinal))
    }) else {
        if ACTIVE_SCOPES.load(Ordering::Relaxed) > 0 {
            count_other_thread(size);
        }
        return false;
    };

    let event = Event {
        ordinal,
        domain,
        operation,
        size: size as u64,
        align: align as u64,
        thread: thread_id(),
    };
    let index = TRACE_LEN.fetch_add(1, Ordering::Relaxed);
    match TRACE.get(index) {
        Some(slot) => slot.store(&event),
        None => TRACE_OVERFLOW.store(true, Ordering::Relaxed),
    }

    if CHECK_EXPECTED.load(Ordering::Relaxed) && ordinal <= policy.target() {
        let matches = usize::try_from(ordinal)
            .ok()
            .filter(|&i| i < EXPECTED_LEN.load(Ordering::Relaxed))
            .is_some_and(|i| EXPECTED[i].load().same_request(&event));
        if !matches {
            let _ = DIVERGED_AT.compare_exchange(
                u64::MAX,
                ordinal,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    if policy.fails(ordinal) && DIVERGED_AT.load(Ordering::Relaxed) == u64::MAX {
        if INJECTED.fetch_add(1, Ordering::Relaxed) == 0 && WRITE_STACK.load(Ordering::Relaxed) {
            RECORDING_STACK.with(|recording| {
                recording.set(true);
                write_stack();
                recording.set(false);
            });
        }
        alloc_chaos_injection_hook(ordinal);
        return true;
    }
    false
}

pub struct ScopedFailingAllocator<A>(pub A);

unsafe impl<A: GlobalAlloc> GlobalAlloc for ScopedFailingAllocator<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if on_allocation(
            Domain::Global,
            Operation::Alloc,
            layout.size(),
            layout.align(),
        ) {
            return std::ptr::null_mut();
        }
        unsafe { self.0.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if on_allocation(
            Domain::Global,
            Operation::AllocZeroed,
            layout.size(),
            layout.align(),
        ) {
            return std::ptr::null_mut();
        }
        unsafe { self.0.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Returning null leaves the old block allocated and unchanged.
        if on_allocation(Domain::Global, Operation::Realloc, new_size, layout.align()) {
            return std::ptr::null_mut();
        }
        unsafe { self.0.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { self.0.dealloc(ptr, layout) }
    }
}

/// An active scope on the current thread. Dropping the outermost guard ends the scope.
pub struct Scope {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Scope {
    /// Starts a scope on the current thread with `policy` and ordinal 0. A nested `enter` joins
    /// the active scope: it keeps the outer policy and ordinal counter, and `policy` is ignored.
    pub fn enter(policy: Policy) -> Scope {
        DEPTH.with(|depth| {
            if depth.get() == 0 {
                POLICY.with(|p| p.set(policy));
                ORDINAL.with(|o| o.set(0));
                ACTIVE_SCOPES.fetch_add(1, Ordering::Relaxed);
            }
            depth.set(depth.get() + 1);
        });
        Scope {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        DEPTH.with(|depth| {
            depth.set(depth.get() - 1);
            if depth.get() == 0 {
                ACTIVE_SCOPES.fetch_sub(1, Ordering::Relaxed);
            }
        });
    }
}

/// Allocations of other threads while a scope was active: `(thread, count, bytes)`, plus the
/// number of allocations from threads past [`OTHER_THREAD_CAPACITY`].
pub fn other_threads() -> (Vec<(u32, u64, u64)>, u64) {
    let threads = OTHER_THREADS
        .iter()
        .filter_map(|slot| {
            let thread = slot.thread.load(Ordering::Relaxed);
            (thread != 0).then(|| {
                (
                    thread,
                    slot.count.load(Ordering::Relaxed),
                    slot.bytes.load(Ordering::Relaxed),
                )
            })
        })
        .collect();
    (threads, OTHER_THREADS_OVERFLOW.load(Ordering::Relaxed))
}

/// Clears the trace, the expected trace and the counters. Call it outside any scope.
pub fn reset() {
    TRACE_LEN.store(0, Ordering::Relaxed);
    TRACE_OVERFLOW.store(false, Ordering::Relaxed);
    EXPECTED_LEN.store(0, Ordering::Relaxed);
    CHECK_EXPECTED.store(false, Ordering::Relaxed);
    DIVERGED_AT.store(u64::MAX, Ordering::Relaxed);
    INJECTED.store(0, Ordering::Relaxed);
}

/// Loads the baseline trace: the allocator then checks every event up to the policy's target
/// ordinal against it, and does not inject a failure once an event differs.
pub fn set_expected(events: &[Event]) -> Result<(), String> {
    if events.len() > TRACE_CAPACITY {
        return Err(format!("expected trace too long: {}", events.len()));
    }
    for (slot, event) in EXPECTED.iter().zip(events) {
        slot.store(event);
    }
    EXPECTED_LEN.store(events.len(), Ordering::Relaxed);
    CHECK_EXPECTED.store(true, Ordering::Relaxed);
    Ok(())
}

/// The recorded events. Call it outside any scope (it allocates).
pub fn recorded() -> Result<Vec<Event>, String> {
    if TRACE_OVERFLOW.load(Ordering::Relaxed) {
        return Err(format!("trace overflow: more than {TRACE_CAPACITY} events"));
    }
    let len = TRACE_LEN.load(Ordering::Relaxed);
    Ok(TRACE[..len].iter().map(Slot::load).collect())
}

pub fn injected() -> u64 {
    INJECTED.load(Ordering::Relaxed)
}

pub fn diverged_at() -> Option<u64> {
    match DIVERGED_AT.load(Ordering::Relaxed) {
        u64::MAX => None,
        ordinal => Some(ordinal),
    }
}

pub fn current_thread_id() -> u32 {
    thread_id()
}
