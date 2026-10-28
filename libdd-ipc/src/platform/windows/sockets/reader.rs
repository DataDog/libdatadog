// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::make_overlapped;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};
use windows_sys::Win32::Foundation::{
    ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_OPERATION_ABORTED, HANDLE,
};
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::CreateEventA;

/// Reads one message directly into the caller's buffer.
///
/// For [`PipeReader::read`], the buffer must hold the entire wire message; [`PipeReader::recv`]
/// continues a message that doesn't fit into a buffer of its own. As an optimization, sequential
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

        let event = self.event()?;
        let result = Self::read_chunk(pipe, event.as_raw_handle() as HANDLE, buf, blocking)
            .and_then(|chunk| match chunk {
                Chunk::Complete(n) => parse_message(buf, n),
                // The caller-provided buffer must hold the whole message.
                Chunk::Partial(_) => Err(io::Error::from_raw_os_error(ERROR_MORE_DATA as i32)),
            });
        self.cache_event(event);
        result
    }

    /// Blocking receive of one message of up to `max_wire` bytes, decoded by `decode`.
    ///
    /// `buf` is a reusable buffer for the common case. The rest of a message that doesn't fit
    /// is read into a buffer sized for that message, freed after decoding. A message beyond
    /// `max_wire` fails with `ERROR_MORE_DATA`, as it would with a `max_wire`-byte buffer.
    pub(super) fn recv<T>(
        &self,
        pipe: HANDLE,
        buf: &mut [u8],
        max_wire: usize,
        decode: impl FnOnce(&[u8]) -> T,
    ) -> io::Result<(T, Vec<OwnedHandle>)> {
        let event = self.event()?;
        let result = Self::recv_with_event(pipe, event.as_raw_handle() as HANDLE, buf, max_wire)
            .and_then(|(message, n)| {
                let message = message.as_deref().unwrap_or(buf);
                let (len, handles) = parse_message(message, n)?;
                Ok((decode(&message[..len]), handles))
            });
        self.cache_event(event);
        result
    }

    /// Reads one whole message. Returns the buffer holding it (`None` for `buf`) and its length.
    fn recv_with_event(
        pipe: HANDLE,
        event: HANDLE,
        buf: &mut [u8],
        max_wire: usize,
    ) -> io::Result<(Option<Vec<u8>>, usize)> {
        let first = match Self::read_chunk(pipe, event, buf, true)? {
            Chunk::Complete(n) => return Ok((None, n)),
            Chunk::Partial(n) => n,
        };
        let too_large = || io::Error::from_raw_os_error(ERROR_MORE_DATA as i32);
        let total = first.saturating_add(bytes_left_in_message(pipe)?);
        if total > max_wire {
            return Err(too_large());
        }

        let mut message = vec![0u8; total];
        message[..first].copy_from_slice(&buf[..first]);
        let mut filled = first;
        loop {
            if filled == message.len() {
                // The pipe reported less than the message holds; read on defensively.
                if filled >= max_wire {
                    return Err(too_large());
                }
                message.resize(filled.saturating_mul(2).min(max_wire), 0);
            }
            match Self::read_chunk(pipe, event, &mut message[filled..], true)? {
                Chunk::Complete(n) => return Ok((Some(message), filled + n)),
                Chunk::Partial(n) => filled += n,
            }
        }
    }

    /// Reads (part of) one message into `buf`.
    fn read_chunk(
        pipe: HANDLE,
        event: HANDLE,
        buf: &mut [u8],
        blocking: bool,
    ) -> io::Result<Chunk> {
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
            match error.raw_os_error().map(|e| e as u32) {
                Some(ERROR_IO_PENDING) => {}
                // Completed synchronously with part of the message; the overlapped result
                // reports the bytes read.
                Some(ERROR_MORE_DATA) => return Self::overlapped_chunk(pipe, &overlapped, false),
                _ => return Err(error),
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

        Self::overlapped_chunk(pipe, &overlapped, !blocking)
    }

    /// Waits for the read described by `overlapped`. `cancelled` maps an aborted read (see
    /// [`Self::read_chunk`]) to `WouldBlock`.
    fn overlapped_chunk(
        pipe: HANDLE,
        overlapped: &OVERLAPPED,
        cancelled: bool,
    ) -> io::Result<Chunk> {
        let mut read = 0;
        if unsafe { GetOverlappedResult(pipe, overlapped, &mut read, 1) } == 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error().map(|e| e as u32) {
                Some(ERROR_MORE_DATA) => return Ok(Chunk::Partial(read as usize)),
                Some(ERROR_OPERATION_ABORTED) if cancelled => {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                _ => return Err(error),
            }
        }
        Ok(Chunk::Complete(read as usize))
    }

    /// The cached completion event, or a new one.
    fn event(&self) -> io::Result<OwnedHandle> {
        if let Some(event) = self.take_cached_event() {
            return Ok(event);
        }
        let event = unsafe { CreateEventA(null(), 1, 0, null()) };
        if event == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { OwnedHandle::from_raw_handle(event as RawHandle) })
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

/// Outcome of one `ReadFile` on a message-mode pipe.
enum Chunk {
    /// The read ended the message.
    Complete(usize),
    /// The buffer filled up before the end of the message (`ERROR_MORE_DATA`).
    Partial(usize),
}

/// Bytes of the current message not read yet.
fn bytes_left_in_message(pipe: HANDLE) -> io::Result<usize> {
    let mut left: u32 = 0;
    if unsafe { PeekNamedPipe(pipe, null_mut(), 0, null_mut(), null_mut(), &mut left) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(left as usize)
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
