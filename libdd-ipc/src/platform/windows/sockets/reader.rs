// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::make_overlapped;
use std::cell::UnsafeCell;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use windows_sys::Win32::Foundation::{
    ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::{CreateEventA, INFINITE, WaitForSingleObject};

pub(crate) struct DeadlineRead {
    pipe: OwnedHandle,
    event: OwnedHandle,
    op: Box<UnsafeCell<OVERLAPPED>>,
    buffer: Option<Vec<u8>>,
}

// The kernel owns the boxed operation and buffer until completion. Rust accesses them exclusively.
unsafe impl Send for DeadlineRead {}
unsafe impl Sync for DeadlineRead {}

impl DeadlineRead {
    pub(crate) fn new(conn: &super::SeqpacketConn) -> io::Result<Self> {
        let pipe = conn.handle.try_clone()?;
        let event = create_event()?;
        let op = Box::new(UnsafeCell::new(make_overlapped(
            event.as_raw_handle() as HANDLE | 1,
        )));
        Ok(Self {
            pipe,
            event,
            op,
            buffer: None,
        })
    }

    pub(crate) fn wait(&mut self, buf: &mut Vec<u8>, deadline: Instant) -> io::Result<()> {
        let pipe = self.pipe.as_raw_handle() as HANDLE;
        if self.buffer.is_none() {
            let len = u32::try_from(buf.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
            // Suppress IOCP notifications: the pipe writer owns the TP_IO callbacks.
            *self.op.get_mut() = make_overlapped(self.event.as_raw_handle() as HANDLE | 1);
            let bytes = self.buffer.insert(std::mem::take(buf));
            if unsafe { ReadFile(pipe, bytes.as_mut_ptr(), len, null_mut(), self.op.get()) } != 0 {
                return Ok(());
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                std::mem::swap(buf, bytes);
                self.buffer = None;
                return Err(err);
            }
        }
        let ms = deadline
            .saturating_duration_since(Instant::now())
            .as_nanos()
            .div_ceil(1_000_000);
        let timeout = u32::try_from(ms).unwrap_or(INFINITE - 1).min(INFINITE - 1);
        match unsafe { WaitForSingleObject(self.event.as_raw_handle() as HANDLE, timeout) } {
            WAIT_OBJECT_0 | WAIT_TIMEOUT => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }

    pub(crate) fn finish(
        &mut self,
        buf: &mut Vec<u8>,
        blocking: bool,
    ) -> Option<io::Result<(usize, Vec<OwnedHandle>)>> {
        self.buffer.as_ref()?;
        let mut n = 0;
        let ok = unsafe {
            GetOverlappedResult(
                self.pipe.as_raw_handle() as HANDLE,
                self.op.get(),
                &mut n,
                i32::from(blocking),
            )
        };
        let result = if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        };
        if result
            .as_ref()
            .is_err_and(|e| e.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32))
        {
            return Some(Err(io::ErrorKind::WouldBlock.into()));
        }
        *buf = self.buffer.take()?;
        Some(result.and_then(|len| parse_message(buf, len)))
    }
}

impl Drop for DeadlineRead {
    fn drop(&mut self) {
        if self.buffer.is_some() {
            // A timeout leaves the ordered read alive for late-reply accounting. Only teardown
            // cancels it.
            unsafe { CancelIoEx(self.pipe.as_raw_handle() as HANDLE, self.op.get()) };
            let _ = self.finish(&mut Vec::new(), true);
        }
    }
}

fn create_event() -> io::Result<OwnedHandle> {
    let event = unsafe { CreateEventA(null(), 1, 0, null()) };
    if event == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(event as RawHandle) })
}

/// Reads one message directly into the caller's buffer.
///
/// The buffer must hold the entire wire message. As an optimization, sequential
/// reads reuse one completion event. Concurrent reads create independent events
/// and retain whichever one returns to an empty cache first. (The penalty of
/// recreating the event is not that big on Windows, but it's significant on
/// Wine.)
#[derive(Default)]
pub(super) struct PipeReader {
    cached_event: AtomicUsize,
}

impl PipeReader {
    pub(super) fn read(
        &self,
        pipe: HANDLE,
        buf: &mut [u8],
        blocking: bool,
    ) -> io::Result<(usize, Vec<OwnedHandle>)> {
        if !blocking {
            let mut avail: u32 = 0;
            if unsafe { PeekNamedPipe(pipe, null_mut(), 0, null_mut(), &mut avail, null_mut()) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            if avail == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }

        let event = match self.take_cached_event() {
            Some(event) => event,
            None => create_event()?,
        };
        let result = Self::read_with_event(pipe, event.as_raw_handle() as HANDLE, buf, blocking);
        self.cache_event(event);
        result
    }

    fn read_with_event(
        pipe: HANDLE,
        event: HANDLE,
        buf: &mut [u8],
        blocking: bool,
    ) -> io::Result<(usize, Vec<OwnedHandle>)> {
        let len = u32::try_from(buf.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "IPC buffer is too large"))?;
        // ReadFile resets the reusable event before starting the operation.
        // The low event bit suppresses IOCP notifications. Reads do not reserve a
        // TP_IO callback, even after the writer registers this handle.
        let mut overlapped = make_overlapped(event | 1);
        if unsafe {
            ReadFile(
                pipe,
                buf.as_mut_ptr() as _,
                len,
                null_mut(),
                &mut overlapped,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_IO_PENDING as i32)
            {
                return Err(error);
            }
            if !blocking {
                // The Named Pipe File System (NPFS) stages incoming bytes, then atomically
                // claims the read's I/O Request Packet (IRP) by clearing its cancel routine.
                // Only the winning read decrements the writer's BytesNotWritten count,
                // committing consumption. If cancellation owns the IRP, NPFS frees the
                // staged copy and leaves the message for the next read; if the read owns it,
                // completion reports success. After waiting for terminal completion below,
                // ERROR_OPERATION_ABORTED is therefore a rollback-safe WouldBlock result.
                // Writes have the reverse ordering; see PipeWriter.
                unsafe { CancelIoEx(pipe, &overlapped) };
            }
        }

        let mut read = 0;
        if unsafe { GetOverlappedResult(pipe, &overlapped, &mut read, 1) } == 0 {
            let error = io::Error::last_os_error();
            if !blocking
                && error.raw_os_error()
                    == Some(windows_sys::Win32::Foundation::ERROR_OPERATION_ABORTED as i32)
            {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            return Err(error);
        }
        parse_message(buf, read as usize)
    }

    fn take_cached_event(&self) -> Option<OwnedHandle> {
        let raw = self.cached_event.swap(0, Ordering::Relaxed);
        if raw == 0 {
            None
        } else {
            Some(unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) })
        }
    }

    fn cache_event(&self, event: OwnedHandle) {
        let raw = event.as_raw_handle() as usize;
        if self
            .cached_event
            .compare_exchange(0, raw, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            let _ = event.into_raw_handle();
        }
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        let raw = *self.cached_event.get_mut();
        if raw != 0 {
            // The connection is exclusively owned during Drop, so no reader
            // can concurrently take this handle from the cache.
            drop(unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) });
        }
    }
}

/// Parse the handle-suffix wire format from a received message.
///
/// `buf[..n]` contains the raw bytes received from the pipe.
/// Returns `(payload_len, owned_handles)`.
fn parse_message(buf: &[u8], n: usize) -> io::Result<(usize, Vec<OwnedHandle>)> {
    if n < 4 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    let count_bytes: [u8; 4] = buf[n - 4..n]
        .try_into()
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let count = u32::from_le_bytes(count_bytes) as usize;

    let handles_start = n
        .checked_sub(4 + 8 * count)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;

    let mut handles = Vec::with_capacity(count);
    for i in 0..count {
        let off = handles_start + 8 * i;
        let val_bytes: [u8; 8] = buf[off..off + 8]
            .try_into()
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        let val = u64::from_le_bytes(val_bytes);
        handles.push(unsafe { OwnedHandle::from_raw_handle(val as RawHandle) });
    }

    Ok((handles_start, handles))
}
