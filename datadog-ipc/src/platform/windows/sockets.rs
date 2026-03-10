// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows IPC implementation using named pipes in message mode.
//!
//! ## Connection protocol
//!
//! Named pipes with `PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE` preserve message boundaries,
//! giving semantics equivalent to `AF_UNIX SOCK_SEQPACKET` on Linux.
//!
//! ## Handle transfer
//!
//! Windows has no `SCM_RIGHTS`. Handles are duplicated into the peer process before sending,
//! and the duplicated values are embedded as a wire-format suffix after the payload:
//!
//! ```text
//! [payload bytes] [u64 LE × handle_count: handle values in receiver] [u32 LE: handle_count]
//! ```
//!
//! Because `PIPE_READMODE_MESSAGE` delivers the entire message in one `ReadFile` call, the
//! receiver can read directly into the caller-provided buffer, then strip the suffix in-place —
//! no intermediate copy needed.  The caller's buffer must have at least `HANDLE_SUFFIX_SIZE`
//! bytes beyond the maximum expected payload size.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};
use crate::platform::message::MAX_FDS;

// winapi – only used for things not cleanly available in windows-sys
use winapi::shared::minwindef::ULONG;
use winapi::shared::winerror::ERROR_PIPE_CONNECTED;
use winapi::um::handleapi::{CloseHandle, DuplicateHandle, INVALID_HANDLE_VALUE};
use winapi::um::processthreadsapi::{GetCurrentProcess, GetCurrentProcessId, OpenProcess};
use winapi::um::winbase::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use winapi::um::winnt::{DUPLICATE_SAME_ACCESS, HANDLE, PROCESS_DUP_HANDLE};

// windows-sys – used for all pipe/IO/threading syscalls
use windows_sys::Win32::Foundation::{HANDLE as SysHANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::IO::{CancelIo, GetOverlappedResult, OVERLAPPED, OVERLAPPED_0};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeA, PeekNamedPipe, SetNamedPipeHandleState,
    PIPE_NOWAIT, PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{CreateEventA, WaitForSingleObject, INFINITE};

// Named-pipe open-mode bits not available in windows-sys 0.48
const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003; // PIPE_ACCESS_INBOUND | PIPE_ACCESS_OUTBOUND
const FILE_FLAG_OVERLAPPED_: u32 = 0x4000_0000;
const FILE_FLAG_FIRST_PIPE_INSTANCE_: u32 = 0x0008_0000;

/// Maximum IPC message payload size (4 MiB).
pub const MAX_MESSAGE_SIZE: usize = 4 * 1024 * 1024;

/// Wire-format suffix overhead: 4-byte count + 8 bytes per handle slot.
///
/// Receive buffers must be at least `expected_payload_max + HANDLE_SUFFIX_SIZE` bytes.
pub const HANDLE_SUFFIX_SIZE: usize = 4 + 8 * MAX_FDS;

/// Credentials of the connected peer.
#[derive(Debug, Clone, Copy, Default)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
}

/// Append `handles` (duplicated into `peer_pid`) followed by the 4-byte count to `data`.
///
/// On error the function returns without having fully appended.  The caller is responsible
/// for truncating `data` back to the pre-call length if it wishes to restore the original.
fn append_handle_suffix(
    data: &mut Vec<u8>,
    handles: &[RawHandle],
    peer_pid: u32,
) -> io::Result<()> {
    let count = handles.len();

    if count > 0 {
        let peer_proc = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, peer_pid) };
        if peer_proc.is_null() {
            return Err(io::Error::last_os_error());
        }
        for &h in handles {
            let mut dup: HANDLE = null_mut();
            let ok = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    h as HANDLE,
                    peer_proc,
                    &mut dup,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if ok == 0 {
                let err = io::Error::last_os_error();
                unsafe { CloseHandle(peer_proc) };
                return Err(err);
            }
            data.extend_from_slice(&(dup as u64).to_le_bytes());
        }
        unsafe { CloseHandle(peer_proc) };
    }

    data.extend_from_slice(&(count as u32).to_le_bytes());
    Ok(())
}

/// Read one message from `h` directly into `buf`.
///
/// `buf` must be large enough to hold the entire wire message
/// (payload + `HANDLE_SUFFIX_SIZE`).  If the message is larger than `buf`, `ReadFile`
/// returns `ERROR_MORE_DATA` and this function propagates the error.
///
/// Returns `(payload_len, owned_handles)`.
fn pipe_read(
    h: SysHANDLE,
    buf: &mut [u8],
    blocking: bool,
) -> io::Result<(usize, Vec<OwnedHandle>)> {
    if !blocking {
        let mut avail: u32 = 0;
        if unsafe { PeekNamedPipe(h, null_mut(), 0, null_mut(), &mut avail, null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if avail == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
    }

    let mut read: u32 = 0;
    if unsafe { ReadFile(h, buf.as_mut_ptr() as _, buf.len() as u32, &mut read, null_mut()) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let n = read as usize;

    // Parse the suffix: last 4 bytes are handle_count (LE u32).
    if n < 4 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    let count_bytes: [u8; 4] = buf[n - 4..n]
        .try_into()
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let count = u32::from_le_bytes(count_bytes) as usize;

    // Before the count are 8 bytes × count handle values.
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

    // payload occupies buf[0..handles_start]
    Ok((handles_start, handles))
}

fn pipe_write(h: SysHANDLE, data: &[u8], blocking: bool) -> io::Result<()> {
    if !blocking {
        let mode = PIPE_NOWAIT;
        unsafe { SetNamedPipeHandleState(h, &mode, null(), null()) };
    }

    let mut written: u32 = 0;
    let ok =
        unsafe { WriteFile(h, data.as_ptr() as _, data.len() as u32, &mut written, null_mut()) };

    if !blocking {
        let mode = PIPE_WAIT;
        unsafe { SetNamedPipeHandleState(h, &mode, null(), null()) };
    }

    if ok == 0 {
        let err = io::Error::last_os_error();
        if !blocking
            && err.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_NO_DATA as i32)
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        return Err(err);
    }
    Ok(())
}

fn create_pipe_server(name: &[u8], first_instance: bool) -> io::Result<OwnedHandle> {
    let open_mode = PIPE_ACCESS_DUPLEX
        | FILE_FLAG_OVERLAPPED_
        | if first_instance {
            FILE_FLAG_FIRST_PIPE_INSTANCE_
        } else {
            0
        };

    let h = unsafe {
        CreateNamedPipeA(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            (MAX_MESSAGE_SIZE + HANDLE_SUFFIX_SIZE) as u32,
            (MAX_MESSAGE_SIZE + HANDLE_SUFFIX_SIZE) as u32,
            0,
            null_mut(),
        )
    };

    if h == INVALID_HANDLE_VALUE as SysHANDLE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) })
}

fn path_to_null_terminated(path: &Path) -> Vec<u8> {
    let s = path.to_string_lossy();
    let mut bytes = s.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn make_overlapped(event: SysHANDLE) -> OVERLAPPED {
    OVERLAPPED {
        Internal: 0,
        InternalHigh: 0,
        Anonymous: OVERLAPPED_0 { Pointer: null_mut() },
        hEvent: event,
    }
}

/// A named-pipe server that accepts message-mode IPC connections.
///
/// `try_accept` swaps the connected pipe instance for a fresh server instance so the listener
/// remains ready for the next client.  Interior mutability (`Mutex`) allows `&self` in
/// `try_accept`.
pub struct SeqpacketListener {
    inner: Mutex<OwnedHandle>,
    name: Vec<u8>, // NUL-terminated ANSI pipe name, e.g. `\\.\\pipe\\…`
}

unsafe impl Send for SeqpacketListener {}
unsafe impl Sync for SeqpacketListener {}

impl SeqpacketListener {
    /// Bind to a named pipe derived from `path` and prepare to accept connections.
    ///
    /// Uses `FILE_FLAG_FIRST_PIPE_INSTANCE` so that a second concurrent `bind` to the same path
    /// fails with `ERROR_ACCESS_DENIED` — the signal used by `attempt_listen` to detect that
    /// another process is already serving.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let name = path_to_null_terminated(path.as_ref());
        let handle = create_pipe_server(&name, true)?;
        Ok(Self {
            inner: Mutex::new(handle),
            name,
        })
    }

    /// Construct from a pre-bound handle received from a parent process.
    ///
    /// Reconstructs the pipe name via `NtQueryObject`.
    pub fn from_owned_fd(fd: OwnedHandle) -> Self {
        use crate::platform::named_pipe_name_from_raw_handle;
        let name = named_pipe_name_from_raw_handle(fd.as_raw_handle())
            .map(|s| { let mut b = s.into_bytes(); b.push(0); b })
            .unwrap_or_default();
        Self {
            inner: Mutex::new(fd),
            name,
        }
    }

    /// Try to accept a pending connection (non-blocking).
    ///
    /// Returns `Err(WouldBlock)` when no client is waiting.
    /// On success, the current pipe instance is handed to the `SeqpacketConn` and a fresh
    /// server instance replaces it in the listener.
    pub fn try_accept(&self) -> io::Result<SeqpacketConn> {
        // Create the replacement server handle *before* taking the lock so that on failure
        // we haven't mutated anything.
        let new_server = create_pipe_server(&self.name, false)?;

        let mut guard = self
            .inner
            .lock()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
        let raw: SysHANDLE = guard.as_raw_handle() as SysHANDLE;

        // Use overlapped ConnectNamedPipe with a 0-ms wait for non-blocking behaviour.
        let event = unsafe { CreateEventA(null_mut(), 1, 0, null_mut()) };
        if event == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut ov = make_overlapped(event);

        let result = unsafe { ConnectNamedPipe(raw, &mut ov) };
        let connect_err = io::Error::last_os_error();

        let connected = if result != 0 {
            true
        } else {
            match connect_err.raw_os_error().map(|e| e as u32) {
                Some(e) if e == ERROR_PIPE_CONNECTED => true,
                Some(e) if e == windows_sys::Win32::Foundation::ERROR_IO_PENDING => {
                    match unsafe { WaitForSingleObject(event, 0) } {
                        WAIT_OBJECT_0 => {
                            let mut transferred: u32 = 0;
                            unsafe { GetOverlappedResult(raw, &ov, &mut transferred, 0) != 0 }
                        }
                        WAIT_TIMEOUT => {
                            unsafe {
                                CancelIo(raw);
                                // Wait for the cancellation to complete so the handle is clean.
                                let mut transferred: u32 = 0;
                                GetOverlappedResult(raw, &ov, &mut transferred, 1);
                                CloseHandle(event as HANDLE);
                            }
                            return Err(io::ErrorKind::WouldBlock.into());
                        }
                        _ => {
                            unsafe { CloseHandle(event as HANDLE) };
                            return Err(io::Error::last_os_error());
                        }
                    }
                }
                _ => {
                    unsafe { CloseHandle(event as HANDLE) };
                    return Err(connect_err);
                }
            }
        };
        unsafe { CloseHandle(event as HANDLE) };

        if !connected {
            return Err(io::Error::last_os_error());
        }

        let mut client_pid: ULONG = 0;
        unsafe { GetNamedPipeClientProcessId(guard.as_raw_handle() as HANDLE, &mut client_pid) };

        // Swap: the connected handle goes to the SeqpacketConn; the fresh server replaces it.
        let conn_handle = std::mem::replace(&mut *guard, new_server);

        Ok(SeqpacketConn {
            inner: conn_handle,
            peer_pid: client_pid,
            read_timeout: None,
            write_timeout: None,
        })
    }

    pub fn as_raw_handle(&self) -> RawHandle {
        self.inner
            .lock()
            .map(|g| g.as_raw_handle())
            .unwrap_or(null_mut())
    }
}

impl std::os::windows::io::AsRawHandle for SeqpacketListener {
    fn as_raw_handle(&self) -> RawHandle {
        SeqpacketListener::as_raw_handle(self)
    }
}

impl std::os::windows::io::IntoRawHandle for SeqpacketListener {
    fn into_raw_handle(self) -> RawHandle {
        self.inner
            .into_inner()
            .map(|h| h.into_raw_handle())
            .unwrap_or(null_mut())
    }
}

/// A connected named pipe providing message-boundary-preserving IPC.
pub struct SeqpacketConn {
    pub(crate) inner: OwnedHandle,
    peer_pid: u32,
    read_timeout: Option<std::time::Duration>,
    write_timeout: Option<std::time::Duration>,
}

unsafe impl Send for SeqpacketConn {}

impl SeqpacketConn {
    /// Connect to a server at the given named pipe path (e.g. `\\\\.\\pipe\\…`).
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        use winapi::shared::winerror::ERROR_PIPE_BUSY;
        use winapi::um::fileapi::{CreateFileA, OPEN_EXISTING};
        use winapi::um::winnt::{GENERIC_READ, GENERIC_WRITE};

        let name = path_to_null_terminated(path.as_ref());
        let h = unsafe {
            CreateFileA(
                name.as_ptr() as *const i8,
                GENERIC_READ | GENERIC_WRITE,
                0,
                null_mut(),
                OPEN_EXISTING,
                0, // synchronous, non-overlapped
                null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
                return Err(io::ErrorKind::ConnectionRefused.into());
            }
            return Err(err);
        }

        // Upgrade to message read-mode.
        let mode = PIPE_READMODE_MESSAGE;
        unsafe { SetNamedPipeHandleState(h as SysHANDLE, &mode, null(), null()) };

        let inner = unsafe { OwnedHandle::from_raw_handle(h as RawHandle) };
        let mut server_pid: ULONG = 0;
        unsafe {
            GetNamedPipeServerProcessId(inner.as_raw_handle() as HANDLE, &mut server_pid);
        }
        Ok(Self {
            inner,
            peer_pid: server_pid,
            read_timeout: None,
            write_timeout: None,
        })
    }

    /// Create an in-process connected pair (for testing).
    pub fn socketpair() -> io::Result<(Self, Self)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = unsafe { GetCurrentProcessId() };
        let name_str = format!(r"\\.\pipe\datadog-ipc-pair-{}-{}", pid, n);
        let name = path_to_null_terminated(Path::new(&name_str));

        let server_handle = create_pipe_server(&name, true)?;

        // Start ConnectNamedPipe asynchronously so we can connect from the same thread.
        let event = unsafe { CreateEventA(null_mut(), 1, 0, null_mut()) };
        if event == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut ov = make_overlapped(event);
        let srv_raw = server_handle.as_raw_handle() as SysHANDLE;
        unsafe { ConnectNamedPipe(srv_raw, &mut ov) };

        let client = Self::connect(&name_str)?;

        // Wait for the server-side accept to complete.
        unsafe {
            WaitForSingleObject(event, INFINITE);
            CloseHandle(event as HANDLE);
        }

        let server = Self {
            inner: server_handle,
            peer_pid: pid,
            read_timeout: None,
            write_timeout: None,
        };
        Ok((server, client))
    }

    /// Build a `SeqpacketConn` from a server-side pipe handle (after `ConnectNamedPipe`).
    pub fn from_server_handle(handle: OwnedHandle, client_pid: u32) -> Self {
        Self {
            inner: handle,
            peer_pid: client_pid,
            read_timeout: None,
            write_timeout: None,
        }
    }

    /// Retrieve the peer process's credentials (pid, uid).
    pub fn peer_credentials(&self) -> io::Result<PeerCredentials> {
        Ok(PeerCredentials {
            pid: self.peer_pid,
            uid: 0,
        })
    }

    /// Non-blocking send.
    ///
    /// Appends the handle suffix to `data` in-place, writes the message, then truncates `data`
    /// back to its original length — whether the write succeeded or failed.  On `WouldBlock`
    /// the caller can retry without re-encoding `data`.
    pub fn try_send_raw(&self, data: &mut Vec<u8>, handles: &[RawHandle]) -> io::Result<()> {
        let orig_len = data.len();
        if let Err(e) = append_handle_suffix(data, handles, self.peer_pid) {
            data.truncate(orig_len);
            return Err(e);
        }
        let result = pipe_write(self.inner.as_raw_handle() as SysHANDLE, data, false);
        data.truncate(orig_len);
        result
    }

    /// Blocking send.
    pub fn send_raw_blocking(&self, data: &mut Vec<u8>, handles: &[RawHandle]) -> io::Result<()> {
        let orig_len = data.len();
        if let Err(e) = append_handle_suffix(data, handles, self.peer_pid) {
            data.truncate(orig_len);
            return Err(e);
        }
        let result = pipe_write(self.inner.as_raw_handle() as SysHANDLE, data, true);
        data.truncate(orig_len);
        result
    }

    /// Non-blocking receive. Returns `Err(WouldBlock)` when no message is available.
    ///
    /// `buf` must be at least `payload_max + HANDLE_SUFFIX_SIZE` bytes.
    pub fn try_recv_raw(&self, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedHandle>)> {
        pipe_read(self.inner.as_raw_handle() as SysHANDLE, buf, false)
    }

    /// Blocking receive.
    ///
    /// `buf` must be at least `payload_max + HANDLE_SUFFIX_SIZE` bytes.
    pub fn recv_raw_blocking(&self, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedHandle>)> {
        pipe_read(self.inner.as_raw_handle() as SysHANDLE, buf, true)
    }

    pub fn as_raw_handle(&self) -> RawHandle {
        self.inner.as_raw_handle()
    }

    pub fn set_read_timeout(&mut self, d: Option<std::time::Duration>) -> io::Result<()> {
        self.read_timeout = d;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, d: Option<std::time::Duration>) -> io::Result<()> {
        self.write_timeout = d;
        Ok(())
    }
}

/// Returns `true` if a server is listening at the given named pipe path.
pub fn is_listening<P: AsRef<Path>>(path: P) -> io::Result<bool> {
    Ok(SeqpacketConn::connect(path).is_ok())
}

/// The async connection type on Windows is the synchronous `SeqpacketConn` itself;
/// blocking I/O is wrapped in `tokio::task::block_in_place` for async compatibility.
pub type AsyncConn = SeqpacketConn;

impl SeqpacketConn {
    /// Convert to an async connection (identity on Windows).
    pub fn into_async_conn(self) -> io::Result<AsyncConn> {
        Ok(self)
    }
}

/// Async receive on a Windows named pipe IPC connection.
///
/// Wraps `recv_raw_blocking` in `tokio::task::block_in_place` so it can be awaited
/// without blocking the Tokio thread pool.  Requires a multi-thread Tokio runtime.
pub async fn recv_raw_async(
    conn: &AsyncConn,
    buf: &mut [u8],
) -> io::Result<(usize, Vec<OwnedHandle>)> {
    tokio::task::block_in_place(|| conn.recv_raw_blocking(buf))
}

/// Async send on a Windows named pipe IPC connection.
///
/// Wraps `send_raw_blocking` in `tokio::task::block_in_place`.
/// Server responses never carry handles (handles flow client→server only via in-band suffix).
pub async fn send_raw_async(conn: &AsyncConn, data: &[u8]) -> io::Result<()> {
    let mut data_vec = data.to_vec();
    tokio::task::block_in_place(|| conn.send_raw_blocking(&mut data_vec, &[]))
}
