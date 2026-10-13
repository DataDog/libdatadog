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

mod reader;
mod writer;
use reader::PipeReader;
use writer::PipeWriter;

use crate::platform::ProcessIdentityGuard;
use crate::platform::message::MAX_FDS;
use std::task::{Context, Poll};
use std::{
    cell::RefCell,
    future::Future,
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle},
    path::Path,
    pin::Pin,
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle,
    ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{
    CancelIo, CancelIoEx, GetOverlappedResult, OVERLAPPED, OVERLAPPED_0,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeA, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
    PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    SetNamedPipeHandleState,
};
use windows_sys::Win32::System::Threading::{
    CreateEventA, GetCurrentProcess, GetCurrentProcessId, INFINITE, OpenProcess,
    PROCESS_DUP_HANDLE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

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

pub use crate::platform::peer_credentials::PeerCredentials;

/// Append `handles` (duplicated into `peer_pid`) followed by the 4-byte count to `data`.
/// `peer` caches the peer process handle for the connection, opened on first use.
///
/// The returned guard rolls duplicates back only before their values can reach the peer.
fn append_handle_suffix(
    data: &mut Vec<u8>,
    handles: &[RawHandle],
    peer_pid: u32,
    peer: &mut Option<Arc<OwnedHandle>>,
) -> io::Result<PendingHandleTransfers> {
    let count = handles.len();
    let count_u32 = u32::try_from(count)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many IPC handles"))?;
    let mut pending = PendingHandleTransfers {
        peer: None,
        handles: Vec::with_capacity(count),
        exposed: false,
    };

    if count > 0 {
        let peer = match peer {
            Some(peer) => peer,
            None => {
                let _identity = ProcessIdentityGuard::enter();
                let peer_proc = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, peer_pid) };
                if peer_proc == 0 {
                    return Err(io::Error::last_os_error());
                }
                peer.insert(Arc::new(unsafe {
                    OwnedHandle::from_raw_handle(peer_proc as RawHandle)
                }))
            }
        };
        let peer_proc = peer.as_raw_handle() as HANDLE;
        pending.peer = Some(Arc::clone(peer));
        for &h in handles {
            let mut dup: HANDLE = 0;
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
                return Err(io::Error::last_os_error());
            }
            pending.handles.push(dup);
            // Preserve the handle's pointer-width bits without sign-extending on 32-bit Windows.
            // Every supported Windows pointer width fits into the 64-bit wire representation.
            data.extend_from_slice(&(dup as usize as u64).to_le_bytes());
        }
    }

    data.extend_from_slice(&count_u32.to_le_bytes());
    Ok(pending)
}

/// Tracks handles duplicated into the peer so they can be rolled back until
/// their numeric values may have been transmitted.
struct PendingHandleTransfers {
    peer: Option<Arc<OwnedHandle>>,
    handles: Vec<HANDLE>,
    exposed: bool,
}

impl PendingHandleTransfers {
    fn expose(&mut self) {
        self.exposed = true;
    }
}

impl Drop for PendingHandleTransfers {
    fn drop(&mut self) {
        if self.exposed {
            return;
        }
        let Some(peer) = &self.peer else {
            return;
        };
        for handle in self.handles.drain(..) {
            let mut local = 0;
            // No write has exposed these values. Roll preparation back by moving each
            // duplicate here; the retained process handle identifies the original peer.
            let ok = unsafe {
                DuplicateHandle(
                    peer.as_raw_handle() as HANDLE,
                    handle,
                    GetCurrentProcess(),
                    &mut local,
                    0,
                    0,
                    DUPLICATE_CLOSE_SOURCE | DUPLICATE_SAME_ACCESS,
                )
            };
            if ok != 0 {
                unsafe { CloseHandle(local) };
            }
        }
    }
}

// PID handshakes precede writer registration and always wait for completion.
fn pipe_write_blocking(h: HANDLE, data: &[u8]) -> io::Result<()> {
    let len = u32::try_from(data.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "IPC message is too large"))?;
    let event = unsafe { CreateEventA(null(), 1, 0, null()) };
    if event == 0 {
        return Err(io::Error::last_os_error());
    }
    let event = unsafe { OwnedHandle::from_raw_handle(event as RawHandle) };
    // The low event bit suppresses IOCP notifications. This handshake precedes
    // writer registration and does not reserve a TP_IO callback.
    let mut overlapped = make_overlapped(event.as_raw_handle() as HANDLE | 1);
    if unsafe { WriteFile(h, data.as_ptr() as _, len, null_mut(), &mut overlapped) } == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(windows_sys::Win32::Foundation::ERROR_IO_PENDING as i32) {
            return Err(error);
        }
    }

    let mut written = 0;
    if unsafe { GetOverlappedResult(h, &overlapped, &mut written, 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if written != len {
        return Err(io::ErrorKind::WriteZero.into());
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

    // Created as the process identity so that pipes are not owned by an impersonated user.
    let _identity = ProcessIdentityGuard::enter();
    let h = unsafe {
        let buf_size = PIPE_BUFFER_SIZE.load(Ordering::Relaxed) as u32;
        let sec_attributes = SECURITY_ATTRIBUTES {
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
            &sec_attributes,
        )
    };

    if h == INVALID_HANDLE_VALUE {
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

fn make_overlapped(event: HANDLE) -> OVERLAPPED {
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
        let raw: HANDLE = guard.as_raw_handle() as HANDLE;

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
                                CloseHandle(event);
                            }
                            return Err(io::ErrorKind::WouldBlock.into());
                        }
                        _ => {
                            unsafe { CloseHandle(event) };
                            return Err(io::Error::last_os_error());
                        }
                    }
                }
                _ => {
                    unsafe { CloseHandle(event) };
                    return Err(connect_err);
                }
            }
        };
        unsafe { CloseHandle(event) };

        if !connected {
            return Err(io::Error::last_os_error());
        }

        let mut client_pid: u32 = 0;
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
        pipe_write_blocking(conn_handle.as_raw_handle() as HANDLE, &pid_bytes)?;

        Ok(SeqpacketConn {
            writer: OnceLock::new(),
            reader: PipeReader::default(),
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
    // Fields drop in declaration order: quiesce writer callbacks before closing pipe.
    writer: OnceLock<Result<PipeWriter, i32>>,
    reader: PipeReader,
    handle: OwnedHandle,
    peer_pid: u32,
    read_timeout: Option<std::time::Duration>,
    write_timeout: Option<std::time::Duration>,
}

unsafe impl Send for SeqpacketConn {}

impl SeqpacketConn {
    /// Connect to a server at the given named pipe path (e.g. `\\\\.\\pipe\\…`).
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        use windows_sys::Win32::Foundation::{ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE};
        use windows_sys::Win32::Storage::FileSystem::{CreateFileA, OPEN_EXISTING};

        let name = path_to_null_terminated(path.as_ref());
        let _identity = ProcessIdentityGuard::enter();
        let h = unsafe {
            CreateFileA(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                0,
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
        // Take ownership before any fallible configuration or handshake operation.
        let handle = unsafe { OwnedHandle::from_raw_handle(h as RawHandle) };
        if unsafe { SetNamedPipeHandleState(h, &mode, null(), null()) } == 0 {
            return Err(io::Error::last_os_error());
        }

        // PID handshake: read the 4-byte PID written by try_accept() so that we know the real
        // acceptor PID, not the pipe-creator PID returned by GetNamedPipeServerProcessId.
        //
        // When PHP creates the listener and passes it to the sidecar, GetNamedPipeServerProcessId
        // returns PHP's own PID.  Using that for DuplicateHandle silently duplicates handles back
        // into PHP rather than into the sidecar, causing ERROR_INVALID_HANDLE on the sidecar side.
        let mut pid_buf = [0u8; 4];
        let pid_read = (|| -> io::Result<u32> {
            let event = unsafe { CreateEventA(null(), 1, 0, null()) };
            if event == 0 {
                return Err(io::Error::last_os_error());
            }
            let event = unsafe { OwnedHandle::from_raw_handle(event as RawHandle) };
            // Suppress IOCP notification: the writer has not been registered yet.
            let mut overlapped = make_overlapped(event.as_raw_handle() as HANDLE | 1);
            if unsafe { ReadFile(h, pid_buf.as_mut_ptr() as _, 4, null_mut(), &mut overlapped) }
                == 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error()
                    != Some(windows_sys::Win32::Foundation::ERROR_IO_PENDING as i32)
                {
                    return Err(error);
                }
            }

            let mut read = 0;
            if unsafe { GetOverlappedResult(h, &overlapped, &mut read, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(read)
        })();
        let server_pid: u32 = if matches!(pid_read, Ok(4)) {
            u32::from_le_bytes(pid_buf)
        } else {
            // Fallback: use GetNamedPipeServerProcessId (returns the creator's PID, which may be
            // our own PID if we created the pipe and passed it to the sidecar).
            let mut spid: u32 = 0;
            unsafe { GetNamedPipeServerProcessId(h, &mut spid) };
            spid
        };

        Ok(Self {
            writer: OnceLock::new(),
            reader: PipeReader::default(),
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
        let srv_raw = server_handle.as_raw_handle() as HANDLE;
        unsafe { ConnectNamedPipe(srv_raw, &mut ov) };

        // connect() blocks reading the 4-byte PID handshake that try_accept() writes after
        // accepting.  Run connect() on a thread so we can wait for ConnectNamedPipe and write
        // the PID bytes concurrently, matching what try_accept() does.
        let client_thread = std::thread::spawn(move || Self::connect(name_str));

        // Wait for the client to connect (ConnectNamedPipe completes).
        unsafe {
            WaitForSingleObject(event, INFINITE);
            CloseHandle(event);
        }

        // Write PID handshake to unblock the client thread's ReadFile in connect().
        let pid_bytes = pid.to_le_bytes();
        pipe_write_blocking(srv_raw, &pid_bytes)?;

        let client = client_thread
            .join()
            .map_err(|_| io::Error::from(io::ErrorKind::Other))??;

        let server = Self {
            writer: OnceLock::new(),
            reader: PipeReader::default(),
            handle: server_handle,
            peer_pid: pid,
            read_timeout: None,
            write_timeout: None,
        };
        Ok((server, client))
    }

    /// Build a `SeqpacketConn` from a server-side pipe handle (after `ConnectNamedPipe`).
    /// The handle must have been opened with `FILE_FLAG_OVERLAPPED` and use `PIPE_WAIT`.
    pub fn from_server_handle(handle: OwnedHandle, client_pid: u32) -> Self {
        Self {
            writer: OnceLock::new(),
            reader: PipeReader::default(),
            handle,
            peer_pid: client_pid,
            read_timeout: None,
            write_timeout: None,
        }
    }

    fn raw_handle(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    /// Retrieve the peer process's credentials (pid, uid).
    pub fn peer_credentials(&self) -> io::Result<PeerCredentials> {
        Ok(PeerCredentials {
            pid: self.peer_pid,
            uid: 0,
            gid: 0,
        })
    }

    /// Non-blocking send.
    ///
    /// Takes ownership of `data`. Success means the transport accepted the message;
    /// its write may still be pending. `WouldBlock` rejects before duplicating
    /// handles or submitting any bytes, but still consumes `data`. Later write
    /// failures are reported by subsequent send/receive calls.
    pub fn try_send_raw(&self, data: Vec<u8>, handles: &[RawHandle]) -> io::Result<()> {
        self.writer()?.send(data, handles, self.peer_pid, false)
    }

    /// Wait for earlier accepted writes and this message's terminal write result.
    ///
    /// This does not wait for a server ACK or drain replies. An error after
    /// submission cannot establish whether the peer received the message.
    pub fn send_raw_blocking(&self, data: Vec<u8>, handles: &[RawHandle]) -> io::Result<()> {
        self.writer()?.send(data, handles, self.peer_pid, true)
    }

    /// Non-blocking receive. Returns `Err(WouldBlock)` when no message is available.
    ///
    /// `buf` must be at least `payload_max + HANDLE_SUFFIX_SIZE` bytes.
    pub fn try_recv_raw(&self, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedHandle>)> {
        self.check_write_error()?;
        self.reader.read(self.raw_handle(), buf, false)
    }

    /// Non-blocking drain of up to `max` available ack messages. Returns the count drained.
    ///
    /// `max` should be `send_count - ack_count`. Acks are always 1-byte payloads with no
    /// handles; the buffer is sized for the wire format: 1 payload byte + the 4-byte
    /// handle-count suffix = `1 + HANDLE_SUFFIX_SIZE`.
    pub fn drain_acks_nonblocking(&self, max: usize) -> io::Result<usize> {
        self.check_write_error()?;
        let mut buf = [0u8; 1 + HANDLE_SUFFIX_SIZE];
        let mut total = 0usize;
        loop {
            if total >= max {
                return Ok(total);
            }
            match self.try_recv_raw(&mut buf) {
                Ok(_) => total += 1,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(total),
                Err(e) => return Err(e),
            }
        }
    }

    /// Blocking receive.
    ///
    /// `buf` must be at least `payload_max + HANDLE_SUFFIX_SIZE` bytes.
    pub fn recv_raw_blocking(&self, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedHandle>)> {
        self.check_write_error()?;
        self.reader.read(self.raw_handle(), buf, true)
    }

    /// Borrow the pipe handle. The connection owns its I/O and completion-port
    /// association; callers must not register it with another driver or submit
    /// competing writes/unreserved completion-port operations.
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

    fn writer(&self) -> io::Result<&PipeWriter> {
        self.writer
            .get_or_init(|| PipeWriter::new(self.raw_handle()))
            .as_ref()
            .map_err(|error| io::Error::from_raw_os_error(*error))
    }

    fn check_write_error(&self) -> io::Result<()> {
        match self.writer.get() {
            Some(Ok(writer)) => writer.check_error(),
            Some(Err(error)) => Err(io::Error::from_raw_os_error(*error)),
            None => Ok(()),
        }
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
        unsafe { SetEvent(self.cancel_event.as_raw_handle() as HANDLE) };
    }
}

impl Future for ConnectFuture {
    type Output = io::Result<AsyncConn>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.rx)
            .poll(cx)
            .map(|r| r.unwrap_or_else(|_| Err(io::Error::from(io::ErrorKind::BrokenPipe))))
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
        let cancel_arc = Arc::new(unsafe { OwnedHandle::from_raw_handle(cancel_raw as RawHandle) });
        let cancel_for_thread = Arc::clone(&cancel_arc);

        let (tx, rx) = tokio::sync::oneshot::channel::<io::Result<AsyncConn>>();

        std::thread::spawn(move || {
            let raw = current.as_raw_handle() as HANDLE;
            let cancel_raw = cancel_for_thread.as_raw_handle() as HANDLE;

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
                unsafe { CloseHandle(overlapped_event) };
                Ok(current)
            } else if connect_err.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_IO_PENDING as i32)
            {
                // Overlapped pending - wait for connection or cancellation.
                let handles = [overlapped_event, cancel_raw];
                let wait = unsafe { WaitForMultipleObjects(2, handles.as_ptr() as _, 0, INFINITE) };

                unsafe { CloseHandle(overlapped_event) };

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
                unsafe { CloseHandle(overlapped_event) };
                Err(connect_err)
            };

            // Write PID handshake and build AsyncConn on success.
            let result = conn_result.and_then(|conn_handle| {
                let conn_raw = conn_handle.as_raw_handle() as HANDLE;
                let mut client_pid: u32 = 0;
                unsafe {
                    GetNamedPipeClientProcessId(conn_raw, &mut client_pid);
                }
                let pid_bytes = unsafe { GetCurrentProcessId() }.to_le_bytes();
                pipe_write_blocking(conn_raw, &pid_bytes)?;
                Ok(SeqpacketConn::from_server_handle(conn_handle, client_pid))
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
/// Receive one IPC message and decode it in-place using the supplied callback.
///
/// Uses a thread-local buffer sized to `max_message_size() + HANDLE_SUFFIX_SIZE` so that
/// the per-call heap allocation for the receive buffer is eliminated once the thread-local
/// has grown to full size.  `decode` is called synchronously inside `block_in_place` with
/// a slice of the received bytes before the buffer is made available for the next receive.
pub async fn recv_raw_async<F, T>(conn: &AsyncConn, decode: F) -> io::Result<(T, Vec<OwnedHandle>)>
where
    F: FnOnce(&[u8]) -> T,
{
    thread_local! {
        /// Reusable receive buffer. Grows on first use; never shrinks.
        static RECV_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    }
    let raw = conn.as_raw_handle() as HANDLE;
    tokio::task::block_in_place(|| {
        conn.check_write_error()?;
        RECV_BUF.with_borrow_mut(|buf| {
            let size = max_message_size() + HANDLE_SUFFIX_SIZE;
            if buf.len() < size {
                buf.resize(size, 0u8);
            }
            match conn.reader.read(raw, buf, true) {
                Err(e) => Err(e),
                Ok((payload_len, handles)) => Ok((decode(&buf[..payload_len]), handles)),
            }
        })
    })
}

/// Async send on a Windows named pipe IPC connection.
///
/// Server responses never carry handles; a zero-handle-count suffix is
/// appended. Takes ownership of `data` so the same ordered writer used by
/// synchronous callers can retain its allocation, inside `block_in_place` to
/// bypass mio.
pub async fn send_raw_async(conn: &AsyncConn, data: Vec<u8>) -> io::Result<()> {
    tokio::task::block_in_place(|| conn.writer()?.send(data, &[], conn.peer_pid, true))
}
