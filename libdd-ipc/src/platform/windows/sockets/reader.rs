// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::make_overlapped;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::CreateEventA;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult};

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
            None => {
                let event = unsafe { CreateEventA(null(), 1, 0, null()) };
                if event == 0 {
                    return Err(io::Error::last_os_error());
                }
                unsafe { OwnedHandle::from_raw_handle(event as RawHandle) }
            }
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
        let mut raw = self.cached_event.load(Ordering::Relaxed);
        loop {
            if raw == 0 {
                return None;
            }
            match self.cached_event.compare_exchange_weak(
                raw,
                0,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(raw) => return Some(unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) }),
                Err(current) => raw = current,
            }
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
