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
//! receiver can read directly into the caller-provided buffer, then strip the suffix in-place -
//! no intermediate copy needed.  The caller's buffer must have at least `HANDLE_SUFFIX_SIZE`
//! bytes beyond the maximum expected payload size.

use crate::platform::message::MAX_FDS;
use std::future::Future;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::pin::Pin;
use std::ptr::{null, null_mut};
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::task::{Context, Poll};

// winapi – only used for things not cleanly available in windows-sys
use winapi::shared::minwindef::ULONG;
use winapi::shared::winerror::ERROR_PIPE_CONNECTED;
use winapi::um::handleapi::{CloseHandle, DuplicateHandle, INVALID_HANDLE_VALUE};
use winapi::um::minwinbase::SECURITY_ATTRIBUTES;
use winapi::um::processthreadsapi::{GetCurrentProcess, GetCurrentProcessId, OpenProcess};
use winapi::um::winbase::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use winapi::um::winnt::{DUPLICATE_SAME_ACCESS, HANDLE, PROCESS_DUP_HANDLE};

// windows-sys – used for all pipe/IO/threading syscalls
use windows_sys::Win32::Foundation::{HANDLE as SysHANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Storage::FileSystem::{
    ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeA, PeekNamedPipe, SetNamedPipeHandleState, PIPE_NOWAIT,
    PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventA, SetEvent, WaitForMultipleObjects, WaitForSingleObject, INFINITE,
};
use windows_sys::Win32::System::IO::{CancelIo, CancelIoEx, GetOverlappedResult, OVERLAPPED, OVERLAPPED_0};

/// Wire-format suffix overhead: 4-byte count + 8 bytes per handle slot.
///
/// Receive buffers must be at least `expected_payload_max + HANDLE_SUFFIX_SIZE` bytes.
pub const HANDLE_SUFFIX_SIZE: usize = 4 + 8 * MAX_FDS;

/// Global pipe buffer size used by `create_pipe_server`.
///
/// Defaults to 4 MiB payload + handle suffix.  Changed via [`set_pipe_buffer_size`]
/// before binding a listener or creating a socketpair.
static PIPE_BUFFER_SIZE: AtomicUsize = AtomicUsize::new(4 * 1024 * 1024 + HANDLE_SUFFIX_SIZE);

/// Maximum IPC message payload size, equal to the pipe buffer minus the handle suffix.
pub fn max_message_size() -> usize {
    PIPE_BUFFER_SIZE.load(Ordering::Relaxed) - HANDLE_SUFFIX_SIZE
}

/// Set the named-pipe send/receive buffer size used for all future [`SeqpacketListener::bind`]
/// and [`SeqpacketConn::socketpair`] calls.
///
/// Named-pipe buffer sizes are fixed at creation time on Windows; this must be called *before*
/// creating a listener or socketpair to take effect on new connections.
pub fn set_pipe_buffer_size(size: usize) {
    PIPE_BUFFER_SIZE.store(size, Ordering::Relaxed);
}

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
    if unsafe {
        ReadFile(
            h,
            buf.as_mut_ptr() as _,
            buf.len() as u32,
            &mut read,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    parse_message(buf, read as usize)
}

fn pipe_write(h: SysHANDLE, data: &[u8], blocking: bool) -> io::Result<()> {
    if !blocking {
        let mode = PIPE_NOWAIT | PIPE_READMODE_MESSAGE;
        unsafe { SetNamedPipeHandleState(h, &mode, null(), null()) };
    }

    let mut written: u32 = 0;
    let ok = unsafe {
        WriteFile(
            h,
            data.as_ptr() as _,
            data.len() as u32,
            &mut written,
            null_mut(),
        )
    };

    if !blocking {
        let mode = PIPE_WAIT | PIPE_READMODE_MESSAGE;
        unsafe { SetNamedPipeHandleState(h, &mode, null(), null()) };
    }

    if ok == 0 {
        let err = io::Error::last_os_error();
        if !blocking
            && err.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_NO_DATA as i32)
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        return Err(err);
    }
    Ok(())
}

fn create_pipe_server(name: &[u8], first_instance: bool) -> io::Result<OwnedHandle> {
    let open_mode = PIPE_ACCESS_DUPLEX
        | FILE_FLAG_OVERLAPPED
        | if first_instance {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };

    let h = unsafe {
        let buf_size = PIPE_BUFFER_SIZE.load(Ordering::Relaxed) as u32;
        let mut sec_attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: 1, // We want this one to be inherited
        };
        CreateNamedPipeA(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            buf_size,
            buf_size,
            0,
            &mut sec_attributes as *mut SECURITY_ATTRIBUTES as *mut _,
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
        Anonymous: OVERLAPPED_0 {
            Pointer: null_mut(),
        },
        hEvent: event,
    }
}

/// A named-pipe server that accepts message-mode IPC connections.
///
/// `try_accept` swaps the connected pipe instance for a fresh server instance so the listener
/// remains ready for the next client.  `accept_async` does the same but awaits the connection
/// using overlapped I/O with proper cancellation support.  Interior mutability (`Mutex`) allows
/// `&self` in both methods.
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
    /// fails with `ERROR_ACCESS_DENIED` - the signal used by `attempt_listen` to detect that
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
            .map(|s| {
                let mut b = s.into_bytes();
                b.push(0);
                b
            })
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

        // PID handshake: write our PID to the client so it can correctly DuplicateHandle into us.
        //
        // The named pipe creator is determined by who calls CreateNamedPipeA.  When PHP creates the
        // listener and passes it to the sidecar, GetNamedPipeServerProcessId on the client side
        // returns PHP's own PID - not the sidecar's - causing DuplicateHandle to target the wrong
        // process.  This one-shot 4-byte message lets the client discover the actual acceptor PID
        // before sending any handles.
        let my_pid = unsafe { GetCurrentProcessId() };
        let pid_bytes = my_pid.to_le_bytes();
        let mut written: u32 = 0;
        unsafe {
            WriteFile(
                conn_handle.as_raw_handle() as SysHANDLE,
                pid_bytes.as_ptr() as _,
                4,
                &mut written,
                null_mut(),
            )
        };

        Ok(SeqpacketConn {
            handle: conn_handle,
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

impl AsRawHandle for SeqpacketListener {
    fn as_raw_handle(&self) -> RawHandle {
        SeqpacketListener::as_raw_handle(self)
    }
}

impl IntoRawHandle for SeqpacketListener {
    fn into_raw_handle(self) -> RawHandle {
        self.inner
            .into_inner()
            .map(|h| h.into_raw_handle())
            .unwrap_or(null_mut())
    }
}

/// A connected named pipe providing message-boundary-preserving IPC.
pub struct SeqpacketConn {
    handle: OwnedHandle,
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

        // PID handshake: read the 4-byte PID written by try_accept() so that we know the real
        // acceptor PID, not the pipe-creator PID returned by GetNamedPipeServerProcessId.
        //
        // When PHP creates the listener and passes it to the sidecar, GetNamedPipeServerProcessId
        // returns PHP's own PID.  Using that for DuplicateHandle silently duplicates handles back
        // into PHP rather than into the sidecar, causing ERROR_INVALID_HANDLE on the sidecar side.
        let mut pid_buf = [0u8; 4];
        let mut read_bytes: u32 = 0;
        let pid_ok = unsafe {
            ReadFile(
                h as SysHANDLE,
                pid_buf.as_mut_ptr() as _,
                4,
                &mut read_bytes,
                null_mut(),
            )
        };
        let server_pid: ULONG = if pid_ok != 0 && read_bytes == 4 {
            u32::from_le_bytes(pid_buf)
        } else {
            // Fallback: use GetNamedPipeServerProcessId (returns the creator's PID, which may be
            // our own PID if we created the pipe and passed it to the sidecar).
            let mut spid: ULONG = 0;
            unsafe { GetNamedPipeServerProcessId(h as HANDLE, &mut spid) };
            spid
        };

        let handle = unsafe { OwnedHandle::from_raw_handle(h as RawHandle) };
        Ok(Self {
            handle,
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

        // connect() blocks reading the 4-byte PID handshake that try_accept() writes after
        // accepting.  Run connect() on a thread so we can wait for ConnectNamedPipe and write
        // the PID bytes concurrently, matching what try_accept() does.
        let client_thread = std::thread::spawn(move || Self::connect(name_str));

        // Wait for the client to connect (ConnectNamedPipe completes).
        unsafe {
            WaitForSingleObject(event, INFINITE);
            CloseHandle(event as HANDLE);
        }

        // Write PID handshake to unblock the client thread's ReadFile in connect().
        let pid_bytes = pid.to_le_bytes();
        let mut written: u32 = 0;
        unsafe {
            WriteFile(
                srv_raw,
                pid_bytes.as_ptr() as _,
                4,
                &mut written,
                null_mut(),
            )
        };

        let client = client_thread
            .join()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))??;

        let server = Self {
            handle: server_handle,
            peer_pid: pid,
            read_timeout: None,
            write_timeout: None,
        };
        Ok((server, client))
    }

    /// Build a `SeqpacketConn` from a server-side pipe handle (after `ConnectNamedPipe`).
    pub fn from_server_handle(handle: OwnedHandle, client_pid: u32) -> Self {
        Self {
            handle,
            peer_pid: client_pid,
            read_timeout: None,
            write_timeout: None,
        }
    }

    fn raw_handle(&self) -> SysHANDLE {
        self.handle.as_raw_handle() as SysHANDLE
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
    /// back to its original length - whether the write succeeded or failed.  On `WouldBlock`
    /// the caller can retry without re-encoding `data`.
    pub fn try_send_raw(&self, data: &mut Vec<u8>, handles: &[RawHandle]) -> io::Result<()> {
        let orig_len = data.len();
        if let Err(e) = append_handle_suffix(data, handles, self.peer_pid) {
            data.truncate(orig_len);
            return Err(e);
        }
        let result = pipe_write(self.raw_handle(), data, false);
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
        let result = pipe_write(self.raw_handle(), data, true);
        data.truncate(orig_len);
        result
    }

    /// Non-blocking receive. Returns `Err(WouldBlock)` when no message is available.
    ///
    /// `buf` must be at least `payload_max + HANDLE_SUFFIX_SIZE` bytes.
    pub fn try_recv_raw(&self, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedHandle>)> {
        pipe_read(self.raw_handle(), buf, false)
    }

    /// Blocking receive.
    ///
    /// `buf` must be at least `payload_max + HANDLE_SUFFIX_SIZE` bytes.
    pub fn recv_raw_blocking(&self, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedHandle>)> {
        pipe_read(self.raw_handle(), buf, true)
    }

    pub fn as_raw_handle(&self) -> RawHandle {
        self.raw_handle() as RawHandle
    }

    pub fn set_read_timeout(&mut self, d: Option<std::time::Duration>) -> io::Result<()> {
        self.read_timeout = d;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, d: Option<std::time::Duration>) -> io::Result<()> {
        self.write_timeout = d;
        Ok(())
    }

    /// Sets the pipe buffer size for future connections.
    ///
    /// Named-pipe buffer sizes are fixed at creation time on Windows, so this does not affect
    /// the current connection.  It updates the global [`PIPE_BUFFER_SIZE`] used by all
    /// subsequent [`SeqpacketListener::bind`] / [`try_accept`] / [`SeqpacketConn::socketpair`]
    /// calls - i.e. it takes effect on the next reconnect.
    pub fn set_sndbuf_size(&self, size: usize) -> io::Result<()> {
        set_pipe_buffer_size(size);
        Ok(())
    }
}

/// Returns `true` if a server is listening at the given named pipe path.
pub fn is_listening<P: AsRef<Path>>(path: P) -> io::Result<bool> {
    Ok(SeqpacketConn::connect(path).is_ok())
}

/// On Windows, `AsyncConn` is the same type as `SeqpacketConn` — both hold an
/// `OwnedHandle` and a peer PID.  The async serve loop drives I/O via
/// `block_in_place` + raw `ReadFile`/`WriteFile`, bypassing mio entirely.
pub type AsyncConn = SeqpacketConn;

impl SeqpacketConn {
    /// No-op on Windows: the connection is already usable as an `AsyncConn`.
    pub fn into_async_conn(self) -> io::Result<AsyncConn> {
        Ok(self)
    }
}

/// ConnectFuture is a cancellable overlapped ConnectNamedPipe
///
/// A future that resolves when a client connects to the pipe server handle, or
/// returns `Interrupted` if dropped before completion.
///
/// On drop, `SetEvent(cancel_event)` is called.  The dedicated OS thread
/// detects this via `WaitForMultipleObjects`, calls `CancelIoEx` to abort the
/// overlapped `ConnectNamedPipe`, and then exits - no Tokio `spawn_blocking`
/// task is left behind.
struct ConnectFuture {
    rx: tokio::sync::oneshot::Receiver<io::Result<AsyncConn>>,
    /// Windows manual-reset event shared with the worker thread.  Signalled
    /// here on drop to tell the thread to cancel its pending operation.
    cancel_event: Arc<OwnedHandle>,
}

impl Drop for ConnectFuture {
    fn drop(&mut self) {
        unsafe { SetEvent(self.cancel_event.as_raw_handle() as SysHANDLE) };
    }
}

impl Future for ConnectFuture {
    type Output = io::Result<AsyncConn>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.rx).poll(cx).map(|r| {
            r.unwrap_or_else(|_| Err(io::Error::from(io::ErrorKind::BrokenPipe)))
        })
    }
}

impl SeqpacketListener {
    /// Asynchronously accept one client connection.
    ///
    /// Installs a fresh server handle *before* any `await` point so the
    /// listener remains ready even if this future is dropped mid-accept.
    ///
    /// A dedicated OS thread (not a Tokio `spawn_blocking` task) manages the
    /// overlapped `ConnectNamedPipe` call.  When the future is dropped
    /// (`select!` shutdown), `SetEvent` signals the thread to call
    /// `CancelIoEx` and exit immediately - Tokio's runtime shutdown is never
    /// blocked waiting for a lingering thread-pool task.
    pub async fn accept_async(&self) -> io::Result<AsyncConn> {
        // Create the replacement server handle before taking the lock.
        let new_server = create_pipe_server(&self.name, false)?;

        // Atomically swap: the listener now holds a fresh handle ready for
        // the *next* accept; `current` is the handle we will connect.
        let current = {
            let mut guard = self
                .inner
                .lock()
                .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
            std::mem::replace(&mut *guard, new_server)
        };

        // Cancel event shared between the future's Drop and the worker thread.
        let cancel_raw = unsafe { CreateEventA(null_mut(), 1, 0, null_mut()) };
        if cancel_raw == 0 {
            return Err(io::Error::last_os_error());
        }
        let cancel_arc =
            Arc::new(unsafe { OwnedHandle::from_raw_handle(cancel_raw as RawHandle) });
        let cancel_for_thread = Arc::clone(&cancel_arc);

        let (tx, rx) = tokio::sync::oneshot::channel::<io::Result<AsyncConn>>();

        std::thread::spawn(move || {
            let raw = current.as_raw_handle() as SysHANDLE;
            let cancel_raw = cancel_for_thread.as_raw_handle() as SysHANDLE;

            // Create event for the overlapped ConnectNamedPipe.
            let overlapped_event = unsafe { CreateEventA(null_mut(), 1, 0, null_mut()) };
            if overlapped_event == 0 {
                let _ = tx.send(Err(io::Error::last_os_error()));
                return;
            }

            // `ov` is on the thread's stack - stable for the thread's lifetime.
            let mut overlapped = make_overlapped(overlapped_event);

            let connect_result = unsafe { ConnectNamedPipe(raw, &mut overlapped) };
            let connect_err = io::Error::last_os_error();

            // conn_result: io::Result<OwnedHandle> - PID handshake applied below.
            let conn_result: io::Result<OwnedHandle> = if connect_result != 0
                || connect_err.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32)
            {
                // Already connected (e.g. client arrived before ConnectNamedPipe).
                unsafe { CloseHandle(overlapped_event as HANDLE) };
                Ok(current)
            } else if connect_err.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_IO_PENDING as i32)
            {
                // Overlapped pending - wait for connection or cancellation.
                let handles = [overlapped_event, cancel_raw];
                let wait =
                    unsafe { WaitForMultipleObjects(2, handles.as_ptr() as _, 0, INFINITE) };

                unsafe { CloseHandle(overlapped_event as HANDLE) };

                if wait == WAIT_OBJECT_0 {
                    // Connected.
                    let mut transferred: u32 = 0;
                    let ok = unsafe { GetOverlappedResult(raw, &overlapped, &mut transferred, 0) };
                    if ok != 0 {
                        Ok(current)
                    } else {
                        Err(io::Error::last_os_error())
                    }
                } else {
                    // Cancelled (or error) - abort the overlapped op.
                    unsafe { CancelIoEx(raw, &overlapped) };
                    let mut transferred: u32 = 0;
                    // bWait=1: block until the cancellation IOCP completion arrives.
                    unsafe { GetOverlappedResult(raw, &overlapped, &mut transferred, 1) };
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                }
            } else {
                unsafe { CloseHandle(overlapped_event as HANDLE) };
                Err(connect_err)
            };

            // Write PID handshake and build AsyncConn on success.
            let result = conn_result.map(|conn_handle| {
                let conn_raw = conn_handle.as_raw_handle() as SysHANDLE;
                let mut client_pid: ULONG = 0;
                unsafe {
                    GetNamedPipeClientProcessId(conn_raw as HANDLE, &mut client_pid);
                }
                let pid_bytes = unsafe { GetCurrentProcessId() }.to_le_bytes();
                let mut written: u32 = 0;
                unsafe {
                    WriteFile(conn_raw, pid_bytes.as_ptr() as _, 4, &mut written, null_mut());
                }
                SeqpacketConn::from_server_handle(conn_handle, client_pid)
            });

            let _ = tx.send(result);
            // cancel_for_thread (Arc<OwnedHandle>) is dropped here.
        });

        ConnectFuture {
            rx,
            cancel_event: cancel_arc,
        }
        .await
    }
}

/// Async receive on a Windows named pipe IPC connection.
///
/// Uses `block_in_place` + raw `ReadFile` to avoid mio's 4 KB internal read-
/// buffer limit.  For message-mode pipes a single `ReadFile` delivers the
/// entire message.
pub async fn recv_raw_async(conn: &AsyncConn) -> io::Result<(Vec<u8>, Vec<OwnedHandle>)> {
    let raw = conn.as_raw_handle() as SysHANDLE;
    tokio::task::block_in_place(|| {
        let size = max_message_size() + HANDLE_SUFFIX_SIZE;
        let mut buf = vec![0u8; size];
        let (payload_len, handles) = pipe_read(raw, &mut buf, true)?;
        buf.truncate(payload_len);
        Ok((buf, handles))
    })
}

/// Async send on a Windows named pipe IPC connection.
///
/// Server responses never carry handles; a zero-handle-count suffix is
/// appended.  Uses `block_in_place` + raw `WriteFile` to bypass mio.
pub async fn send_raw_async(conn: &AsyncConn, data: &[u8]) -> io::Result<()> {
    let raw = conn.as_raw_handle() as SysHANDLE;
    let mut buf = data.to_vec();
    buf.extend_from_slice(&0u32.to_le_bytes()); // zero handle count
    tokio::task::block_in_place(move || pipe_write(raw, &buf, true))
}
