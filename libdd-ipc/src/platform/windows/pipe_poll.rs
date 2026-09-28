// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Readiness-based blocking I/O for the client end of a named pipe kept permanently in
//! `PIPE_NOWAIT` mode.
//!
//! ## Why
//!
//! A named-pipe handle's wait mode (`PIPE_WAIT` / `PIPE_NOWAIT`) must not be switched while the
//! handle is in use: toggling it around individual `WriteFile` calls races with other I/O on the
//! handle and turns a full pipe buffer into a silent zero-byte "successful" write.  The client
//! end is therefore switched to `PIPE_NOWAIT | PIPE_READMODE_MESSAGE` exactly once, in
//! [`ClientPipe::new`], and never changed again.  No method of [`ClientPipe`] can change it.
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
    ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::{
    GetNamedPipeInfo, SetNamedPipeHandleState, PIPE_NOWAIT, PIPE_READMODE_MESSAGE, PIPE_SERVER_END,
};
use windows_sys::Win32::System::Threading::{CreateEventA, WaitForSingleObject, INFINITE};

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

fn fsctl(
    pipe: HANDLE,
    code: u32,
    input: *const c_void,
    input_len: u32,
    output: *mut c_void,
    output_len: u32,
) -> NtStatus {
    let mut iosb = IoStatusBlock {
        status: 0,
        information: 0,
    };
    // SAFETY: the caller passes buffers valid for the given lengths.  The client handle is
    // synchronous (not FILE_FLAG_OVERLAPPED), so the I/O manager completes the request before
    // returning and `iosb` does not need to outlive this call.
    unsafe {
        NtFsControlFile(
            pipe,
            0,
            null_mut(),
            null_mut(),
            &mut iosb,
            code,
            input,
            input_len,
            output,
            output_len,
        )
    }
}

fn status_to_error(status: NtStatus) -> io::Error {
    if status == STATUS_PENDING {
        return io::Error::other("named pipe FSCTL unexpectedly pending on a synchronous handle");
    }
    // RtlNtStatusToDosError returns a Win32 error code, which always fits in an i32.
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(i32::MAX))
}

/// Register `event` to be signalled when the readiness state of `pipe` changes.
///
/// Returns `Ok(false)` if the running Windows version supports neither select FSCTL.
pub(crate) fn select_events(pipe: HANDLE, event: HANDLE, event_types: u32) -> io::Result<bool> {
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
        s if s >= 0 && s != STATUS_PENDING => Ok(true),
        STATUS_NOT_SUPPORTED | STATUS_INVALID_DEVICE_REQUEST => Ok(false),
        s => Err(status_to_error(s)),
    }
}

/// Query the current readiness bits (`FILE_PIPE_*`) of `pipe`.
pub(crate) fn enum_events(pipe: HANDLE) -> io::Result<u32> {
    // Optional handle of an event to reset; we pass 0 (none), like OpenVMM.
    let handle_to_reset: u64 = 0;
    let mut events: u32 = 0;
    let status = fsctl(
        pipe,
        FSCTL_PIPE_EVENT_ENUM,
        (&handle_to_reset as *const u64).cast(),
        8,
        (&mut events as *mut u32).cast(),
        4,
    );
    if status >= 0 && status != STATUS_PENDING {
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
            // SAFETY: `event` is a valid event handle owned by the enclosing `ClientPipe`.
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

/// Client end of a message-mode named pipe, permanently in `PIPE_NOWAIT` mode.
///
/// Invariant: the handle's wait mode is set once in [`ClientPipe::new`] and never changed for
/// the lifetime of the handle.  There is intentionally no API to change the mode or to take the
/// handle back out; dropping a `ClientPipe` simply closes the handle.
pub(crate) struct ClientPipe {
    /// Declared first so that the pipe is closed before its readiness event.
    handle: OwnedHandle,
    /// Auto-reset event registered with `FSCTL_PIPE_EVENT_SELECT`.
    ready_event: OwnedHandle,
    /// Write quota of this end: the largest message that can ever be written.
    out_quota: usize,
    /// `false` if the OS supports no select FSCTL and waits degrade to short polling.
    event_registered: bool,
    waiter: ReadinessWaiter,
}

impl ClientPipe {
    /// Take ownership of a freshly opened, synchronous client pipe handle and switch it to
    /// `PIPE_NOWAIT | PIPE_READMODE_MESSAGE` for the rest of its life.
    ///
    /// This is the only place where the wait mode of a client handle is ever set.
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

        let event_registered = match select_events(
            raw,
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
        match enum_events(self.raw()) {
            Ok(events) => events & FILE_PIPE_DISCONNECTED != 0,
            Err(_) => true,
        }
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
        let mut read: u32 = 0;
        // SAFETY: `buf` is valid for `len` bytes of writes; synchronous handle, no OVERLAPPED.
        let ok = unsafe {
            ReadFile(
                self.raw(),
                buf.as_mut_ptr().cast(),
                len,
                &mut read,
                null_mut(),
            )
        };
        if ok != 0 {
            return usize::try_from(read)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
        let err = io::Error::last_os_error();
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
        let mut written: u32 = 0;
        // SAFETY: `data` is valid for `len` bytes of reads; synchronous handle, no OVERLAPPED.
        let ok = unsafe {
            WriteFile(
                self.raw(),
                data.as_ptr().cast(),
                len,
                &mut written,
                null_mut(),
            )
        };
        if ok != 0 {
            return if written == len {
                Ok(())
            } else if written == 0 {
                // PIPE_NOWAIT + message mode: not enough quota for the whole message, nothing
                // was written.
                Err(io::ErrorKind::WouldBlock.into())
            } else {
                // Must not happen in message mode; the peer would see a truncated message.
                Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    format!("partial named pipe message write: {written} of {len} bytes"),
                ))
            };
        }
        let err = io::Error::last_os_error();
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
