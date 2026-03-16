// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Message-boundary-preserving IPC sockets for Unix.
//!
//! - Linux: `AF_UNIX SOCK_SEQPACKET` with `SCM_RIGHTS` for fd transfer.
//! - macOS: `AF_UNIX SOCK_DGRAM` with an fd-passing connection handshake. This emulates the
//!   semantics which SOCK_SEQPACKET provides us on Linux.

use nix::sys::socket::{recvmsg, sendmsg, AddressFamily, SockFlag, SockType};
pub use nix::sys::socket::{ControlMessage, ControlMessageOwned, MsgFlags, UnixAddr};
use std::{
    io,
    os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::io::unix::AsyncFd;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(not(target_os = "linux"))]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::{bind_abstract, connect_abstract, is_listening};
#[cfg(not(target_os = "linux"))]
pub use macos::is_listening;

use crate::platform::message::MAX_FDS;
#[cfg(not(target_os = "macos"))]
use linux::get_peer_credentials;
#[cfg(target_os = "macos")]
use macos::get_peer_credentials;

/// Global socket buffer size. Determines `max_message_size()` and is applied to new connections.
static SOCKET_BUFFER_SIZE: AtomicUsize = AtomicUsize::new(4 * 1024 * 1024);

/// Set the socket send/receive buffer size used for all future connections.
///
/// This also determines [`max_message_size()`]. Call before creating connections for the new
/// size to take effect on `socketpair`/`connect` calls (macOS).
pub fn set_socket_buffer_size(size: usize) {
    SOCKET_BUFFER_SIZE.store(size, Ordering::Relaxed);
}

/// Maximum IPC message payload size, equal to the configured socket buffer size.
pub fn max_message_size() -> usize {
    SOCKET_BUFFER_SIZE.load(Ordering::Relaxed)
}

/// Extra receive-buffer overhead for the wire format.  Zero on Unix because fds are
/// transferred out-of-band via `SCM_RIGHTS`; non-zero on Windows (see `sockets.rs`).
pub const HANDLE_SUFFIX_SIZE: usize = 0;

/// Credentials of the connected peer, obtained once at connection time.
#[derive(Debug, Clone, Copy, Default)]
pub struct PeerCredentials {
    pub pid: u32,
    pub uid: u32,
}

pub(super) fn create_unix_socket(sock_type: SockType) -> io::Result<OwnedFd> {
    let fd = nix::sys::socket::socket(AddressFamily::Unix, sock_type, SockFlag::empty(), None)
        .map_err(io::Error::from)?;
    // Set close-on-exec (portable across Linux and macOS).
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if flags >= 0 {
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    }
    Ok(fd)
}

pub(super) fn set_nonblocking(fd: RawFd, nonblocking: bool) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let new_flags = if nonblocking {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFL, new_flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn sendmsg_raw(
    fd: RawFd,
    data: &[u8],
    fds: &[RawFd],
    flags: MsgFlags,
) -> io::Result<()> {
    let iov = [io::IoSlice::new(data)];
    if fds.is_empty() {
        sendmsg::<UnixAddr>(fd, &iov, &[], flags, None)
    } else {
        sendmsg::<UnixAddr>(fd, &iov, &[ControlMessage::ScmRights(fds)], flags, None)
    }
    .map(|_| ())
    .map_err(io::Error::from)
}

pub(super) fn recvmsg_raw(
    fd: RawFd,
    buf: &mut [u8],
    flags: MsgFlags,
) -> io::Result<(usize, Vec<OwnedFd>)> {
    let cmsg_space =
        unsafe { libc::CMSG_SPACE((size_of::<libc::c_int>() * MAX_FDS) as libc::c_uint) } as usize;
    let mut cmsg_buf = vec![0u8; cmsg_space];
    let mut iov = [io::IoSliceMut::new(buf)];

    let msg =
        recvmsg::<UnixAddr>(fd, &mut iov, Some(&mut cmsg_buf), flags).map_err(io::Error::from)?;

    let bytes = msg.bytes;
    if bytes == 0 {
        // 0 bytes means EOF (peer closed connection), not a valid datagram.
        // Legitimate acks are always at least 1 byte.
        return Err(io::Error::from(io::ErrorKind::BrokenPipe));
    }
    let mut owned_fds = Vec::new();
    for cmsg in msg.cmsgs().map_err(io::Error::from)? {
        if let ControlMessageOwned::ScmRights(raw_fds) = cmsg {
            for raw_fd in raw_fds {
                owned_fds.push(unsafe { OwnedFd::from_raw_fd(raw_fd) });
            }
        }
    }
    Ok((bytes, owned_fds))
}

pub(super) fn poll_with_timeout(
    fd: RawFd,
    event: libc::c_short,
    timeout: Option<Duration>,
) -> io::Result<()> {
    let timeout_ms: i32 = match timeout {
        None => -1,
        Some(d) => d.as_millis().min(i32::MAX as u128) as i32,
    };
    let mut pfd = libc::pollfd {
        fd,
        events: event,
        revents: 0,
    };
    loop {
        let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if ret > 0 {
            return Ok(());
        }
        if ret == 0 {
            return Err(io::Error::from(io::ErrorKind::TimedOut));
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// A listening socket for accepting IPC connections.
///
/// - Linux: `AF_UNIX SOCK_SEQPACKET` with `listen`/`accept`.
/// - macOS: `AF_UNIX SOCK_DGRAM` rendezvous socket; clients connect via fd-passing handshake.
///
/// Also constructable from a pre-bound fd (e.g. received from a parent process).
/// Implements `IntoRawFd` so the fd can be transferred to a child process via `spawn_worker`.
pub struct SeqpacketListener {
    pub inner: OwnedFd,
}

impl SeqpacketListener {
    /// Construct from a pre-bound fd (e.g. received from a parent process via `spawn_worker`).
    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Self { inner: fd }
    }

    /// Wrap in a Tokio `AsyncFd` for use in async server accept loops.
    ///
    /// Sets the socket to non-blocking mode, then wraps in `AsyncFd<SeqpacketListener>`.
    /// Requires a running Tokio runtime.
    pub fn into_async_listener(self) -> io::Result<AsyncFd<SeqpacketListener>> {
        set_nonblocking(self.inner.as_raw_fd(), true)?;
        AsyncFd::new(self)
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

impl AsRawFd for SeqpacketListener {
    fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

impl IntoRawFd for SeqpacketListener {
    fn into_raw_fd(self) -> RawFd {
        self.inner.into_raw_fd()
    }
}

/// A connected socket providing message-boundary-preserving IPC.
///
/// - Linux: `AF_UNIX SOCK_SEQPACKET`.
/// - macOS: `AF_UNIX SOCK_DGRAM` socketpair endpoint (4 MiB buffers).
pub struct SeqpacketConn {
    pub(super) inner: OwnedFd,
    /// On macOS, closing any local fd for the peer end of a SOCK_DGRAM socketpair
    /// immediately disconnects this socket, even if the peer is still alive in another
    /// process. Keep `_peer` alive here so the connection remains valid until this
    /// `SeqpacketConn` is dropped.
    #[cfg(target_os = "macos")]
    _peer: Option<OwnedFd>,
    /// macOS only: one end of a liveness pipe.  Client holds the write end, server holds
    /// the read end.  Polling either end for `POLLHUP` detects peer disconnection:
    /// write-end POLLHUP ← server closed read end; read-end POLLHUP ← client closed write end.
    #[cfg(target_os = "macos")]
    liveness: Option<OwnedFd>,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl SeqpacketConn {
    pub(super) fn from_owned(
        fd: OwnedFd,
        #[cfg(target_os = "macos")] liveness: Option<OwnedFd>,
    ) -> io::Result<Self> {
        set_nonblocking(fd.as_raw_fd(), true)?;
        Ok(Self {
            inner: fd,
            #[cfg(target_os = "macos")]
            _peer: None,
            #[cfg(target_os = "macos")]
            liveness,
            read_timeout: None,
            write_timeout: None,
        })
    }

    /// Retrieve the peer process's credentials (pid, uid).
    pub fn peer_credentials(&self) -> io::Result<PeerCredentials> {
        get_peer_credentials(self.inner.as_raw_fd())
    }

    /// Non-blocking send. Returns `Err(WouldBlock)` if the socket buffer is full.
    ///
    /// `data` is passed as `&mut Vec<u8>` for API symmetry with the Windows implementation
    /// (which appends handle bytes in-place and truncates back after the write).  On Unix the
    /// Vec is never modified.
    ///
    /// Note: `O_NONBLOCK` is always set on `SeqpacketConn` sockets (via `from_owned`), so
    /// `MSG_DONTWAIT` is not needed and is intentionally omitted — on macOS `AF_UNIX SOCK_DGRAM`
    /// socketpairs, `MSG_DONTWAIT` can return EINVAL instead of EAGAIN.
    #[allow(clippy::ptr_arg)] // windows interface compat
    pub fn try_send_raw(&self, data: &mut Vec<u8>, fds: &[RawFd]) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        self.poll_liveness_pipe()?;
        sendmsg_raw(self.inner.as_raw_fd(), data, fds, MsgFlags::empty())
    }

    /// Blocking send. Polls for writability (respecting write_timeout), then sends.
    #[allow(clippy::ptr_arg)] // windows interface compat
    pub fn send_raw_blocking(&self, data: &mut Vec<u8>, fds: &[RawFd]) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        self.poll_liveness_pipe()?;
        let fd = self.inner.as_raw_fd();
        loop {
            match sendmsg_raw(fd, data, fds, MsgFlags::empty()) {
                Ok(()) => return Ok(()),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    poll_with_timeout(fd, libc::POLLOUT, self.write_timeout)?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Non-blocking receive. Returns `Err(WouldBlock)` if no message available.
    pub fn try_recv_raw(&self, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
        recvmsg_raw(self.inner.as_raw_fd(), buf, MsgFlags::empty())
    }

    /// Blocking receive. Polls for readability (respecting read_timeout), then receives.
    pub fn recv_raw_blocking(&self, buf: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
        let fd = self.inner.as_raw_fd();
        loop {
            match recvmsg_raw(fd, buf, MsgFlags::empty()) {
                Ok(r) => return Ok(r),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                    poll_with_timeout(fd, libc::POLLIN, self.read_timeout)?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn set_read_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.read_timeout = d;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.write_timeout = d;
        Ok(())
    }

    fn setsockopt_int(&self, optname: libc::c_int, size: usize) -> io::Result<()> {
        let size_c = size as libc::c_int;
        let ret = unsafe {
            libc::setsockopt(
                self.inner.as_raw_fd(),
                libc::SOL_SOCKET,
                optname,
                &size_c as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn set_sndbuf_size(&self, size: usize) -> io::Result<()> {
        set_socket_buffer_size(size);
        self.setsockopt_int(libc::SO_SNDBUF, size)
    }

    pub fn set_rcvbuf_size(&self, size: usize) -> io::Result<()> {
        self.setsockopt_int(libc::SO_RCVBUF, size)
    }

    /// Convert to an async connection for use in async server dispatch loops.
    pub fn into_async_conn(self) -> io::Result<AsyncConn> {
        AsyncFd::new(self.inner)
    }

    pub fn as_raw_fd(&self) -> RawFd {
        self.inner.as_raw_fd()
    }
}

/// The async connection type on Unix: a Tokio `AsyncFd` wrapping the raw fd.
pub type AsyncConn = AsyncFd<OwnedFd>;

/// Async receive on a Tokio `AsyncFd`-wrapped IPC connection.
///
/// Allocates a buffer sized to `max_message_size()` per call and returns only the received
/// bytes (truncated), so no large buffer is held between receives.
///
/// Used by the server dispatch loop (generated by `#[service]` macro).
pub async fn recv_raw_async(fd: &AsyncConn) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    loop {
        let mut guard = fd.readable().await?;
        let mut buf = Vec::with_capacity(max_message_size());
        // SAFETY: all bit patterns are valid for u8; recvmsg writes exactly n bytes into
        // the spare capacity before set_len(n) is called below.
        let slice = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr(), max_message_size()) };
        match guard.try_io(|inner| recvmsg_raw(inner.as_raw_fd(), slice, MsgFlags::empty())) {
            Ok(Ok((n, fds))) => {
                unsafe { buf.set_len(n) };
                return Ok((buf, fds));
            }
            Ok(Err(e)) => return Err(e),
            Err(_would_block) => continue,
        }
    }
}

/// Async send on a Tokio `AsyncFd`-wrapped IPC connection.
///
/// Used by the server dispatch loop (generated by `#[service]` macro) to send acks and
/// responses without blocking the async thread.
/// Server responses never carry fds (fds flow client→server only via SCM_RIGHTS).
pub async fn send_raw_async(fd: &AsyncConn, data: &[u8]) -> io::Result<()> {
    loop {
        let mut guard = fd.writable().await?;
        match guard.try_io(|inner| sendmsg_raw(inner.as_raw_fd(), data, &[], MsgFlags::empty())) {
            Ok(result) => return result,
            Err(_would_block) => continue,
        }
    }
}
