// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Readiness-based blocking I/O for a connected named pipe end (client or server) kept
//! permanently in `PIPE_NOWAIT` mode.
//!
//! ## Why
//!
//! A named-pipe handle's wait mode (`PIPE_WAIT` / `PIPE_NOWAIT`) must not be switched while the
//! handle is in use: toggling it around individual `WriteFile` calls races with other I/O on the
//! handle and turns a full pipe buffer into a silent zero-byte "successful" write.  Every
//! connected pipe end is therefore switched to `PIPE_NOWAIT | PIPE_READMODE_MESSAGE` exactly
//! once, in [`NowaitPipe::new`], and never changed again.  No method of [`NowaitPipe`] can
//! change it.
//!
//! ## Synchronous and overlapped handles
//!
//! Client ends are opened synchronously.  Server ends are pipe instances created with
//! `FILE_FLAG_OVERLAPPED` (needed for the cancellable overlapped `ConnectNamedPipe` of the
//! listener), and that flag cannot be removed from a handle.  On such a handle any request may
//! return `STATUS_PENDING` / `ERROR_IO_PENDING` and complete later, writing its status block and
//! output buffer at that point.  Measured on Windows Server 2019, on an overlapped server end in
//! `PIPE_NOWAIT` mode:
//!
//! * `FSCTL_PIPE_EVENT_SELECT(_OLD)` and `FSCTL_PIPE_EVENT_ENUM` return `STATUS_PENDING` *every
//!   time* and complete shortly afterwards (on a synchronous handle they complete inline);
//! * `ReadFile` / `WriteFile` were never seen pending (empty pipe: `ERROR_NO_DATA`; not enough
//!   write quota: success with zero bytes written) over millions of calls, whereas in `PIPE_WAIT`
//!   mode an empty read or a full-buffer write pends.
//!
//! Therefore every request here gets its own completion event (and `OVERLAPPED` /
//! `IO_STATUS_BLOCK`), and a pending request is waited for on that event before the call
//! returns (see [`NowaitPipe::transfer`] and [`fsctl`]).  It must be the request's own event:
//! waiting on the file handle instead (what `ReadFile` / `WriteFile` / `DeviceIoControl` do when
//! called without an `OVERLAPPED` on an overlapped handle) is not reliable, because any other
//! request completing on the same handle signals it too, so the call could return before its own
//! request completed and the kernel would later write into a dead stack frame.  That is not
//! hypothetical: the old server-end code did exactly this with `PIPE_WAIT` reads and writes,
//! which do pend.  Only the readiness *waits* use the shared readiness event.
//!
//! [`NowaitPipe::new`] must be called with no overlapped operation outstanding on the handle (the
//! listener's `ConnectNamedPipe` has completed, or has been cancelled and waited for) and before
//! any other I/O is issued on it.
//!
//! ## How blocking works on a `PIPE_NOWAIT` handle
//!
//! The Named Pipe File System (NPFS) has an undocumented readiness notification mechanism,
//! also used by Microsoft's OpenVMM (`support/pal/src/windows/pipe.rs` and
//! `support/pal/pal_async/src/windows/pipe.rs`):
//!
//! * `FSCTL_PIPE_EVENT_SELECT` registers a Win32 event which NPFS signals whenever the pipe end
//!   becomes readable (`FILE_PIPE_READ_READY`), writable (`FILE_PIPE_WRITE_READY`) or the peer
//!   disconnects (`FILE_PIPE_DISCONNECTED`).  Older Windows versions only support
//!   `FSCTL_PIPE_EVENT_SELECT_OLD` (duplex pipes only), which we fall back to on
//!   `STATUS_NOT_SUPPORTED`.
//! * `FSCTL_PIPE_EVENT_ENUM` returns the current readiness bits.
//!
//! A blocking operation is a loop of: attempt the non-blocking `ReadFile` / `WriteFile`; if it
//! would block, wait on the (auto-reset) event and try again.  Waits are coordinated by
//! [`ReadinessWaiter`] so that concurrent readers and writers sharing one event never lose a
//! wake-up.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    BOOL, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, HANDLE,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::{
    GetNamedPipeInfo, SetNamedPipeHandleState, PIPE_NOWAIT, PIPE_READMODE_MESSAGE, PIPE_SERVER_END,
};
use windows_sys::Win32::System::Threading::{CreateEventA, WaitForSingleObject, INFINITE};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED, OVERLAPPED_0};

/// The pipe end has data available to read.
pub(crate) const FILE_PIPE_READ_READY: u32 = 1;
/// The pipe end has write quota available.
pub(crate) const FILE_PIPE_WRITE_READY: u32 = 2;
/// The peer end has been closed or disconnected.
pub(crate) const FILE_PIPE_DISCONNECTED: u32 = 4;

const FILE_DEVICE_NAMED_PIPE: u32 = 0x11;
const METHOD_BUFFERED: u32 = 0;
const FILE_ANY_ACCESS: u32 = 0;
const FILE_READ_DATA: u32 = 1;
const FILE_WRITE_DATA: u32 = 2;

const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

pub(crate) const FSCTL_PIPE_EVENT_SELECT: u32 = ctl_code(
    FILE_DEVICE_NAMED_PIPE,
    3071,
    METHOD_BUFFERED,
    FILE_ANY_ACCESS,
);
pub(crate) const FSCTL_PIPE_EVENT_SELECT_OLD: u32 = ctl_code(
    FILE_DEVICE_NAMED_PIPE,
    3071,
    METHOD_BUFFERED,
    FILE_WRITE_DATA,
);
pub(crate) const FSCTL_PIPE_EVENT_ENUM: u32 = ctl_code(
    FILE_DEVICE_NAMED_PIPE,
    3072,
    METHOD_BUFFERED,
    FILE_READ_DATA,
);

type NtStatus = i32;
// NTSTATUS values are conventionally written as u32 hex; the `as` reinterprets the bits.
const STATUS_NOT_SUPPORTED: NtStatus = 0xC000_00BB_u32 as i32;
const STATUS_INVALID_DEVICE_REQUEST: NtStatus = 0xC000_0010_u32 as i32;
const STATUS_PENDING: NtStatus = 0x0000_0103;

/// Input buffer of `FSCTL_PIPE_EVENT_SELECT`.
#[repr(C)]
#[allow(dead_code)] // only read by the kernel
pub(crate) struct FilePipeEventSelectBuffer {
    event_types: u32,
    event_handle: u64,
}

/// `IO_STATUS_BLOCK`: a `NTSTATUS`/`PVOID` union followed by `ULONG_PTR Information`.
#[repr(C)]
#[allow(dead_code)] // only written by the kernel; the returned NTSTATUS is used instead
struct IoStatusBlock {
    status: usize,
    information: usize,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtFsControlFile(
        file_handle: HANDLE,
        event: HANDLE,
        apc_routine: *mut c_void,
        apc_context: *mut c_void,
        io_status_block: *mut IoStatusBlock,
        fs_control_code: u32,
        input_buffer: *const c_void,
        input_buffer_length: u32,
        output_buffer: *mut c_void,
        output_buffer_length: u32,
    ) -> NtStatus;

    fn RtlNtStatusToDosError(status: NtStatus) -> u32;
}

/// Manual-reset events used to wait for the completion of individual I/O requests (not for
/// readiness), one per request in flight.
///
/// A request that returns `STATUS_PENDING` / `ERROR_IO_PENDING` must be waited for before its
/// `IO_STATUS_BLOCK` / `OVERLAPPED` and buffers go out of scope, and each concurrently issued
/// request needs its own event for that (waiting on the file handle itself is not reliable: any
/// request completing on the handle signals it).  Events are recycled, so after warm-up a request
/// costs no event creation.
struct CompletionEvents {
    free: Mutex<Vec<OwnedHandle>>,
}

/// A [`CompletionEvents`] event, returned to the pool on drop.
struct CompletionEvent<'a> {
    pool: &'a CompletionEvents,
    event: Option<OwnedHandle>,
}

impl CompletionEvents {
    fn new() -> Self {
        Self {
            free: Mutex::new(Vec::new()),
        }
    }

    fn get(&self) -> io::Result<CompletionEvent<'_>> {
        let pooled = self.free.lock().map_err(|_| poisoned())?.pop();
        let event = match pooled {
            Some(event) => event,
            None => {
                // Manual-reset, initially non-signalled.  The I/O functions reset it when a
                // request is issued.
                // SAFETY: plain event creation with default security and no name.
                let event = unsafe { CreateEventA(null(), 1, 0, null()) };
                if event == 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: `event` is a freshly created handle that nothing else owns.
                unsafe { OwnedHandle::from_raw_handle(event as RawHandle) }
            }
        };
        Ok(CompletionEvent {
            pool: self,
            event: Some(event),
        })
    }
}

impl CompletionEvent<'_> {
    fn raw(&self) -> HANDLE {
        self.event
            .as_ref()
            .map_or(0, |event| event.as_raw_handle() as HANDLE)
    }
}

impl Drop for CompletionEvent<'_> {
    fn drop(&mut self) {
        // Only reached once the request using the event has completed.
        if let (Some(event), Ok(mut free)) = (self.event.take(), self.pool.free.lock()) {
            free.push(event);
        }
    }
}

/// Issue an FSCTL and wait for it to complete; returns its final `NTSTATUS`.
///
/// On an overlapped handle NPFS returns `STATUS_PENDING` for the `FSCTL_PIPE_EVENT_*` requests
/// even in `PIPE_NOWAIT` mode (observed on Windows Server 2019), completing them shortly after.
/// `iosb` and the caller's buffers must stay valid until then, so a pending request is waited for
/// on its own completion `event`.
fn fsctl(
    pipe: HANDLE,
    event: &CompletionEvent<'_>,
    code: u32,
    input: *const c_void,
    input_len: u32,
    output: *mut c_void,
    output_len: u32,
) -> NtStatus {
    // Pre-set to STATUS_PENDING: the kernel overwrites it when a pending request completes.
    let mut iosb = IoStatusBlock {
        status: STATUS_PENDING as usize, // non-negative, lossless
        information: 0,
    };
    // SAFETY: the caller passes buffers valid for the given lengths, and they, `iosb` and the
    // event outlive the request: if it is pending we wait below for its completion, which is
    // signalled on `event` (reset by NtFsControlFile when the request is issued).
    let status = unsafe {
        NtFsControlFile(
            pipe,
            event.raw(),
            null_mut(),
            null_mut(),
            &mut iosb,
            code,
            input,
            input_len,
            output,
            output_len,
        )
    };
    if status != STATUS_PENDING {
        return status;
    }
    let status_ptr = std::ptr::addr_of!(iosb.status);
    // SAFETY: `status_ptr` is the status of the request just issued with `event`.
    unsafe { wait_until_complete(event.raw(), status_ptr) };
    // SAFETY: the request has completed, nothing writes to `iosb` any more.
    ntstatus_of(unsafe { std::ptr::read_volatile(status_ptr) })
}

/// The `NTSTATUS` (32 bits) stored in the pointer-sized first word of an `IO_STATUS_BLOCK` /
/// `OVERLAPPED::Internal`; the truncating casts extract it.
fn ntstatus_of(word: usize) -> NtStatus {
    word as u32 as NtStatus
}

/// Block until the request whose `IO_STATUS_BLOCK` status word is `status` has completed.
///
/// Never returns while the kernel may still write to the request's status block or buffers: if
/// the wait itself failed (impossible for a valid event), it is simply retried.
///
/// # Safety
///
/// `status` must point to the status word of a request that was issued with completion event
/// `event` and returned `STATUS_PENDING` (or `ERROR_IO_PENDING`), and whose status word was
/// `STATUS_PENDING` when it was issued.
unsafe fn wait_until_complete(event: HANDLE, status: *const usize) {
    // The kernel writes the final status before it signals the event.
    while ntstatus_of(std::ptr::read_volatile(status)) == STATUS_PENDING {
        WaitForSingleObject(event, INFINITE);
    }
}

fn status_to_error(status: NtStatus) -> io::Error {
    // RtlNtStatusToDosError returns a Win32 error code, which always fits in an i32.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(i32::MAX))
}

/// Register `event` to be signalled when the readiness state of `pipe` changes.
///
/// Returns `Ok(false)` if the running Windows version supports neither select FSCTL.
fn select_events(
    pipe: HANDLE,
    completion: &CompletionEvent<'_>,
    event: HANDLE,
    event_types: u32,
) -> io::Result<bool> {
    let input = FilePipeEventSelectBuffer {
        event_types,
        // Kernel handle values are small non-negative integers; the FSCTL takes them as u64.
        event_handle: event as usize as u64,
    };
    let input_len = std::mem::size_of::<FilePipeEventSelectBuffer>() as u32; // 16, fits in u32
    let mut status = STATUS_NOT_SUPPORTED;
    // Newer Windows versions support FSCTL_PIPE_EVENT_SELECT (works on unidirectional pipes);
    // older versions only FSCTL_PIPE_EVENT_SELECT_OLD (bidirectional pipes only).
    for code in [FSCTL_PIPE_EVENT_SELECT, FSCTL_PIPE_EVENT_SELECT_OLD] {
        status = fsctl(
            pipe,
            completion,
            code,
            (&input as *const FilePipeEventSelectBuffer).cast(),
            input_len,
            null_mut(),
            0,
        );
        if status != STATUS_NOT_SUPPORTED {
            break;
        }
    }
    match status {
        s if s >= 0 => Ok(true),
        STATUS_NOT_SUPPORTED | STATUS_INVALID_DEVICE_REQUEST => Ok(false),
        s => Err(status_to_error(s)),
    }
}

/// Query the current readiness bits (`FILE_PIPE_*`) of `pipe`.
fn enum_events(pipe: HANDLE, completion: &CompletionEvent<'_>) -> io::Result<u32> {
    // Optional handle of an event to reset; we pass 0 (none), like OpenVMM.
    let handle_to_reset: u64 = 0;
    let mut events: u32 = 0;
    let status = fsctl(
        pipe,
        completion,
        FSCTL_PIPE_EVENT_ENUM,
        (&handle_to_reset as *const u64).cast(),
        8,
        (&mut events as *mut u32).cast(),
        4,
    );
    if status >= 0 {
        Ok(events)
    } else {
        Err(status_to_error(status))
    }
}

/// Poll period used only when the OS supports neither `FSCTL_PIPE_EVENT_SELECT` variant.
const FALLBACK_POLL_INTERVAL_MS: u32 = 5;

/// Coordinates any number of threads waiting on one auto-reset readiness event.
///
/// A single auto-reset event can only wake one waiter per signal, so a reader and a writer
/// waiting concurrently could otherwise steal each other's wake-up.  Instead, at most one
/// thread (the "pump") waits on the kernel event at a time; every signal bumps `generation`
/// and wakes all other waiters via the condvar, who then retry their I/O.
///
/// Callers sample [`Self::generation`] *before* their non-blocking attempt and pass it to
/// [`Self::wait_for_change`], which returns as soon as any signal was observed after the sample.
/// A signal arriving between the attempt and the wait is therefore never lost: either the event
/// is still set when the pump waits on it, or another pump already consumed it and bumped the
/// generation.
struct ReadinessWaiter {
    generation: AtomicU64,
    /// `true` while some thread is blocked on the kernel event.
    pumping: Mutex<bool>,
    changed: Condvar,
}

fn poisoned() -> io::Error {
    io::Error::other("pipe readiness lock poisoned")
}

/// Milliseconds to wait until `deadline` (rounded up), `INFINITE` without a deadline, or
/// `None` if the deadline has passed.
fn remaining_ms(deadline: Option<Instant>) -> Option<u32> {
    let Some(deadline) = deadline else {
        return Some(INFINITE);
    };
    let remaining = deadline.checked_duration_since(Instant::now())?;
    if remaining.is_zero() {
        return None;
    }
    let ms = remaining.as_micros().div_ceil(1000);
    // Cap below INFINITE so that a very long timeout never turns into an infinite wait.
    Some(u32::try_from(ms).unwrap_or(INFINITE - 1).min(INFINITE - 1))
}

impl ReadinessWaiter {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            pumping: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Block until the readiness event was signalled after `seen` was sampled.
    ///
    /// Returns `Err(TimedOut)` once `deadline` passes without a signal.
    fn wait_for_change(
        &self,
        event: HANDLE,
        seen: u64,
        deadline: Option<Instant>,
        fallback_poll: bool,
    ) -> io::Result<()> {
        let mut pumping = self.pumping.lock().map_err(|_| poisoned())?;
        loop {
            if self.generation.load(Ordering::Acquire) != seen {
                return Ok(());
            }
            let Some(timeout_ms) = remaining_ms(deadline) else {
                return Err(io::ErrorKind::TimedOut.into());
            };
            if *pumping {
                // Another thread is waiting on the kernel event; it will wake us.
                pumping = match deadline {
                    None => self.changed.wait(pumping).map_err(|_| poisoned())?,
                    Some(_) => {
                        self.changed
                            .wait_timeout(pumping, Duration::from_millis(u64::from(timeout_ms)))
                            .map_err(|_| poisoned())?
                            .0
                    }
                };
                continue;
            }

            *pumping = true;
            drop(pumping);
            let wait_ms = if fallback_poll {
                timeout_ms.min(FALLBACK_POLL_INTERVAL_MS)
            } else {
                timeout_ms
            };
            // SAFETY: `event` is a valid event handle owned by the enclosing `NowaitPipe`.
            let result = unsafe { WaitForSingleObject(event, wait_ms) };
            let wait_err = io::Error::last_os_error();
            pumping = self.pumping.lock().map_err(|_| poisoned())?;
            *pumping = false;
            let signalled = match result {
                WAIT_OBJECT_0 => true,
                // Without event support, every poll tick is a potential change.
                WAIT_TIMEOUT => fallback_poll && wait_ms < timeout_ms,
                _ => {
                    self.changed.notify_all();
                    return Err(wait_err);
                }
            };
            if signalled {
                self.generation.fetch_add(1, Ordering::AcqRel);
            }
            self.changed.notify_all();
        }
    }
}

fn is_os_error(err: &io::Error, code: u32) -> bool {
    err.raw_os_error()
        .and_then(|e| u32::try_from(e).ok())
        .is_some_and(|e| e == code)
}

fn broken_pipe(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, msg)
}

/// Connected end (client or server) of a message-mode named pipe, permanently in `PIPE_NOWAIT`
/// mode.
///
/// Invariant: the handle's wait mode is set once in [`NowaitPipe::new`] and never changed for
/// the lifetime of the handle.  There is intentionally no API to change the mode or to take the
/// handle back out; dropping a `NowaitPipe` simply closes the handle.
pub(crate) struct NowaitPipe {
    /// Declared first so that the pipe is closed before its readiness event.
    handle: OwnedHandle,
    /// Auto-reset event registered with `FSCTL_PIPE_EVENT_SELECT`.
    ready_event: OwnedHandle,
    /// Write quota of this end: the largest message that can ever be written.
    out_quota: usize,
    /// `false` if the OS supports no select FSCTL and waits degrade to short polling.
    event_registered: bool,
    waiter: ReadinessWaiter,
    /// Completion events of the individual read / write / FSCTL requests.
    completions: CompletionEvents,
}

impl NowaitPipe {
    /// Take ownership of a freshly connected pipe handle and switch it to
    /// `PIPE_NOWAIT | PIPE_READMODE_MESSAGE` for the rest of its life.
    ///
    /// `handle` is either a synchronous client end, or a server instance (opened with
    /// `FILE_FLAG_OVERLAPPED`) whose `ConnectNamedPipe` has completed.  No I/O may have been
    /// issued on it yet other than that `ConnectNamedPipe`, and no overlapped operation may still
    /// be outstanding on it (see the module documentation).
    ///
    /// This is the only place where the wait mode of a connected pipe handle is ever set.
    pub(crate) fn new(handle: OwnedHandle) -> io::Result<Self> {
        let raw = handle.as_raw_handle() as HANDLE;

        let mode = PIPE_NOWAIT | PIPE_READMODE_MESSAGE;
        // SAFETY: `raw` is a valid pipe handle owned by `handle`.
        if unsafe { SetNamedPipeHandleState(raw, &mode, null(), null()) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let mut flags: u32 = 0;
        let mut out_size: u32 = 0;
        let mut in_size: u32 = 0;
        // SAFETY: all out-pointers are valid for writes.
        if unsafe { GetNamedPipeInfo(raw, &mut flags, &mut out_size, &mut in_size, null_mut()) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        // GetNamedPipeInfo reports sizes from the server's point of view: a client's writes
        // land in the server's inbound buffer.
        let out_quota = if flags & PIPE_SERVER_END != 0 {
            out_size
        } else {
            in_size
        };
        if out_quota == 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unbuffered named pipes cannot be used in PIPE_NOWAIT mode",
            ));
        }
        let out_quota = usize::try_from(out_quota)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Auto-reset, initially non-signalled.
        // SAFETY: plain event creation with default security and no name.
        let event = unsafe { CreateEventA(null(), 0, 0, null()) };
        if event == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `event` is a freshly created handle that nothing else owns.
        let ready_event = unsafe { OwnedHandle::from_raw_handle(event as RawHandle) };

        let completions = CompletionEvents::new();
        let event_registered = match select_events(
            raw,
            &completions.get()?,
            event,
            FILE_PIPE_READ_READY | FILE_PIPE_WRITE_READY | FILE_PIPE_DISCONNECTED,
        ) {
            Ok(registered) => registered,
            // The peer is already gone.  Reads and writes will fail immediately, so no wait
            // ever happens and the missing registration does not matter.
            Err(e) if is_os_error(&e, ERROR_PIPE_NOT_CONNECTED) => true,
            Err(e) => return Err(e),
        };

        Ok(Self {
            handle,
            ready_event,
            out_quota,
            event_registered,
            waiter: ReadinessWaiter::new(),
            completions,
        })
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    pub(crate) fn as_raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }

    /// Whether readiness waits use the NPFS event (`true`) or the polling fallback.
    #[cfg(test)]
    pub(crate) fn event_registered(&self) -> bool {
        self.event_registered
    }

    /// Fail with `InvalidInput` if a `len`-byte message can never fit into the pipe.
    ///
    /// A non-blocking write of such a message would report zero bytes written forever.
    pub(crate) fn check_write_len(&self, len: usize) -> io::Result<()> {
        if len > self.out_quota {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "IPC message of {len} bytes exceeds the named pipe buffer quota of {} bytes",
                    self.out_quota
                ),
            ));
        }
        Ok(())
    }

    /// `true` if the peer end is known to be closed (or the pipe can no longer be queried).
    fn peer_disconnected(&self) -> bool {
        match self
            .completions
            .get()
            .and_then(|completion| enum_events(self.raw(), &completion))
        {
            Ok(events) => events & FILE_PIPE_DISCONNECTED != 0,
            Err(_) => true,
        }
    }

    /// Run one `ReadFile` / `WriteFile` call, `issue`, with its own `OVERLAPPED` and completion
    /// event, and return the number of bytes transferred once the request has completed.
    ///
    /// In `PIPE_NOWAIT` mode NPFS completes reads and writes before returning (never observed
    /// pending, unlike the FSCTLs), so this normally costs no more than a plain call.  The
    /// `OVERLAPPED` is what makes it correct regardless, on the overlapped server handles: a
    /// request that does pend is waited for on its own event before `buf` / `OVERLAPPED` go out
    /// of scope.  It is equally valid on synchronous client handles, where the call only returns
    /// once the request has completed.
    fn transfer(&self, issue: impl FnOnce(*mut OVERLAPPED) -> BOOL) -> io::Result<u32> {
        let completion = self.completions.get()?;
        let mut overlapped = OVERLAPPED {
            // Also set by ReadFile / WriteFile; the kernel overwrites it on completion.
            Internal: STATUS_PENDING as usize, // non-negative, lossless
            InternalHigh: 0,
            Anonymous: OVERLAPPED_0 {
                Pointer: null_mut(),
            },
            hEvent: completion.raw(),
        };
        if issue(&mut overlapped) == 0 {
            let err = io::Error::last_os_error();
            if !is_os_error(&err, ERROR_IO_PENDING) {
                // Completed synchronously with an error.
                return Err(err);
            }
            let status_ptr = std::ptr::addr_of!(overlapped.Internal);
            // SAFETY: `status_ptr` is the status of the request just issued with `completion`.
            unsafe { wait_until_complete(completion.raw(), status_ptr) };
        }
        let mut transferred: u32 = 0;
        // SAFETY: the request has completed, so this only reads its result out of `overlapped`
        // (bWait = FALSE).
        if unsafe { GetOverlappedResult(self.raw(), &overlapped, &mut transferred, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(transferred)
    }

    fn wait(&self, seen: u64, deadline: Option<Instant>) -> io::Result<()> {
        self.waiter.wait_for_change(
            self.ready_event.as_raw_handle() as HANDLE,
            seen,
            deadline,
            !self.event_registered,
        )
    }

    /// Single non-blocking read of one whole message into `buf`.
    ///
    /// Returns the message length, `Err(WouldBlock)` if no message is queued, or
    /// `Err(BrokenPipe)` once the peer is gone and no data is left.
    pub(crate) fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let buf_ptr = buf.as_mut_ptr();
        // SAFETY: `buf` is valid for `len` bytes of writes until `transfer` returns, which is
        // after the request completed.
        let err = match self.transfer(|overlapped| unsafe {
            ReadFile(self.raw(), buf_ptr.cast(), len, null_mut(), overlapped)
        }) {
            Ok(read) => {
                return usize::try_from(read)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
            Err(err) => err,
        };
        if is_os_error(&err, ERROR_NO_DATA) {
            // Empty pipe.  If the peer is gone nothing will ever arrive.
            if self.peer_disconnected() {
                return Err(broken_pipe("named pipe peer disconnected"));
            }
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if is_os_error(&err, ERROR_PIPE_NOT_CONNECTED) {
            return Err(broken_pipe("named pipe is not connected"));
        }
        Err(err)
    }

    /// Single non-blocking write of `data` as one message.
    ///
    /// Returns `Err(WouldBlock)` if the pipe currently has no room for the whole message;
    /// nothing is written in that case.
    pub(crate) fn try_write(&self, data: &[u8]) -> io::Result<()> {
        self.check_write_len(data.len())?;
        let len = u32::try_from(data.len())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let data_ptr = data.as_ptr();
        // SAFETY: `data` is valid for `len` bytes of reads until `transfer` returns, which is
        // after the request completed.
        let err = match self.transfer(|overlapped| unsafe {
            WriteFile(self.raw(), data_ptr.cast(), len, null_mut(), overlapped)
        }) {
            Ok(written) if written == len => return Ok(()),
            // PIPE_NOWAIT + message mode: not enough quota for the whole message, nothing was
            // written.
            Ok(0) => return Err(io::ErrorKind::WouldBlock.into()),
            // Must not happen in message mode; the peer would see a truncated message.
            Ok(written) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    format!("partial named pipe message write: {written} of {len} bytes"),
                ))
            }
            Err(err) => err,
        };
        if is_os_error(&err, ERROR_NO_DATA) {
            // ERROR_NO_DATA (STATUS_PIPE_CLOSING) normally means the peer closed its end; only
            // report WouldBlock if NPFS says the peer is still there.
            if self.peer_disconnected() {
                return Err(broken_pipe("named pipe peer disconnected"));
            }
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if is_os_error(&err, ERROR_PIPE_NOT_CONNECTED) || is_os_error(&err, ERROR_BROKEN_PIPE) {
            return Err(broken_pipe("named pipe peer disconnected"));
        }
        Err(err)
    }

    /// Blocking read of one whole message, waiting on the readiness event while the pipe is
    /// empty.  `timeout` bounds the total time spent waiting (`Err(TimedOut)`).
    pub(crate) fn read_blocking(
        &self,
        buf: &mut [u8],
        timeout: Option<Duration>,
    ) -> io::Result<usize> {
        let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        loop {
            let seen = self.waiter.generation();
            match self.try_read(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.wait(seen, deadline)?,
                result => return result,
            }
        }
    }

    /// Blocking write of one whole message, waiting on the readiness event while the pipe has
    /// no room.  `timeout` bounds the total time spent waiting (`Err(TimedOut)`).
    pub(crate) fn write_blocking(&self, data: &[u8], timeout: Option<Duration>) -> io::Result<()> {
        let deadline = timeout.and_then(|t| Instant::now().checked_add(t));
        loop {
            let seen = self.waiter.generation();
            match self.try_write(data) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => self.wait(seen, deadline)?,
                result => return result,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsctl_codes_match_openvmm() {
        assert_eq!(FSCTL_PIPE_EVENT_SELECT, 0x0011_2FFC);
        assert_eq!(FSCTL_PIPE_EVENT_SELECT_OLD, 0x0011_AFFC);
        assert_eq!(FSCTL_PIPE_EVENT_ENUM, 0x0011_7000);
    }

    #[test]
    fn select_buffer_layout() {
        assert_eq!(std::mem::size_of::<FilePipeEventSelectBuffer>(), 16);
        assert_eq!(std::mem::align_of::<FilePipeEventSelectBuffer>(), 8);
        assert_eq!(
            std::mem::size_of::<IoStatusBlock>(),
            2 * std::mem::size_of::<usize>()
        );
    }

    #[test]
    fn remaining_ms_rounds_up_and_expires() {
        assert_eq!(remaining_ms(None), Some(INFINITE));
        let past = Instant::now().checked_sub(Duration::from_millis(1));
        if let Some(past) = past {
            assert_eq!(remaining_ms(Some(past)), None);
        }
        let soon = Instant::now() + Duration::from_micros(1500);
        let ms = remaining_ms(Some(soon)).unwrap_or(0);
        assert!((1..=2).contains(&ms), "{ms}");
    }
}
