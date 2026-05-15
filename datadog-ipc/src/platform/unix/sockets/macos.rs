// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! macOS IPC socket implementation.
//!
//! ## Design
//!
//! macOS does not support `AF_UNIX SOCK_SEQPACKET`.  We emulate the same semantics:
//!
//! **Actual connection**: `socketpair(AF_UNIX, SOCK_DGRAM)` with 4 MiB send/recv buffers.
//!
//! **Handshake rendezvous** (how the server learns about new clients): an
//! `AF_UNIX SOCK_STREAM` server socket bound to the well-known path.
//!
//! Using STREAM for the rendezvous (rather than DGRAM) avoids `EMSGSIZE` errors
//! that macOS returns from `recvmsg` under high concurrency when SCM_RIGHTS are
//! sent over a DGRAM socket.  SOCK_STREAM + SCM_RIGHTS is the standard POSIX
//! approach for fd passing and has no such limitation.
//!
//! ## Connection protocol
//!
//! **Server side** (`SeqpacketListener`):
//! - Binds a `SOCK_STREAM` socket and calls `listen()` → the "rendezvous" socket.
//! - `try_accept()` calls `accept(2)`, switches the accepted fd to blocking mode (it
//!   inherits `O_NONBLOCK` from the listener), writes a 1-byte ACK to the client, then
//!   reads the client fd via `recvmsg` with SCM_RIGHTS, and closes the STREAM connection.
//!
//! **Client side** (`SeqpacketConn::connect`):
//! - Creates a `socketpair(AF_UNIX, SOCK_DGRAM)` with 4 MiB buffers and a liveness pipe.
//! - Connects a fresh `SOCK_STREAM` socket to the rendezvous path, reads the 1-byte ACK
//!   (waits for the server to accept), then sends the server end of the socketpair plus
//!   the read end of the liveness pipe via SCM_RIGHTS, then closes the STREAM connection.
//! - The ACK handshake is required because on macOS, `sendmsg` with SCM_RIGHTS returns
//!   `ENOTCONN` if called before the server has called `accept()` on the connection.
//!
//! **Liveness** (`is_listening`):
//! - Attempts a `connect(2)` to the rendezvous STREAM socket.
//! - Success → live server.  `ECONNREFUSED`/`ENOENT` → stale or absent socket file.

use super::{
    create_unix_socket, max_message_size, sendmsg, set_nonblocking, ControlMessage, MsgFlags,
    SeqpacketConn, SeqpacketListener, UnixAddr,
};
use crate::PeerCredentials;
use nix::sys::socket::{bind, SockType};
use std::mem;
use std::os::fd::RawFd;
use std::{
    ffi::CString,
    io,
    os::unix::{
        ffi::OsStrExt,
        io::{AsRawFd, FromRawFd, OwnedFd},
    },
    path::Path,
};

/// macOS `sockaddr_un::sun_path` is only 104 bytes (103 usable). When the socket path
/// exceeds that, cd to the socket's parent directory for the operation using the
/// thread-local `pthread_chdir_np` (unlike `chdir`, this does not affect other threads),
/// then restore via `pthread_fchdir_np`.
fn with_short_path<T, F: FnOnce(&Path) -> io::Result<T>>(path: &Path, f: F) -> io::Result<T> {
    const SUN_PATH_MAX: usize = 103;
    if path.as_os_str().len() <= SUN_PATH_MAX {
        return f(path);
    }
    extern "C" {
        fn pthread_chdir_np(path: *const libc::c_char) -> libc::c_int;
        fn pthread_fchdir_np(fd: libc::c_int) -> libc::c_int;
    }
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no filename"))?;
    // Save the calling thread's CWD as an fd so we can restore it unconditionally.
    let saved = unsafe { libc::open(b".\0".as_ptr() as *const libc::c_char, libc::O_RDONLY) };
    if saved < 0 {
        return Err(io::Error::last_os_error());
    }
    let saved_owned = unsafe { OwnedFd::from_raw_fd(saved) };
    let dir_cstr = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket dir path contains NUL"))?;
    if unsafe { pthread_chdir_np(dir_cstr.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let result = f(Path::new(name));
    unsafe { pthread_fchdir_np(saved_owned.as_raw_fd()) };
    result
}

fn create_stream_socket() -> io::Result<OwnedFd> {
    create_unix_socket(SockType::Stream)
}

fn set_dgram_buffers(fd: i32) -> io::Result<()> {
    let size = max_message_size() as libc::c_int;
    let len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                &size as *const _ as *const libc::c_void,
                len,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Set `SO_RCVTIMEO` on a socket (seconds, microseconds).
fn set_rcv_timeout(fd: RawFd, secs: libc::time_t, usecs: libc::suseconds_t) -> io::Result<()> {
    let tv = libc::timeval {
        tv_sec: secs,
        tv_usec: usecs,
    };
    if unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &tv as *const _ as *const libc::c_void,
            mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    } < 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl SeqpacketListener {
    /// Bind to a filesystem path using a `SOCK_STREAM` rendezvous socket.
    ///
    /// Uses STREAM (not DGRAM) so that `recvmsg` with SCM_RIGHTS during
    /// `try_accept` is not subject to `EMSGSIZE` under high connection concurrency
    /// (a macOS-specific issue with DGRAM + SCM_RIGHTS).
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        let _ = std::fs::remove_file(path);
        let fd = create_stream_socket()?;
        with_short_path(path, |short| {
            let addr = UnixAddr::new(short).map_err(io::Error::from)?;
            bind(fd.as_raw_fd(), &addr).map_err(io::Error::from)?;
            if unsafe { libc::listen(fd.as_raw_fd(), 128) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })?;
        set_nonblocking(fd.as_raw_fd(), true)?;
        Ok(Self { inner: fd })
    }

    /// Accept a connection by receiving a client fd via SCM_RIGHTS over a STREAM handshake.
    ///
    /// Returns `Err(WouldBlock)` when no connection is pending.
    pub fn try_accept(&self) -> io::Result<SeqpacketConn> {
        loop {
            // Accept the STREAM handshake connection.  The listener is in non-blocking mode so
            // this returns EAGAIN/EWOULDBLOCK when there is nothing to accept.
            let hfd = unsafe {
                libc::accept(self.inner.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut())
            };
            if hfd < 0 {
                return Err(io::Error::last_os_error()); // WouldBlock propagates to tokio
            }
            let hfd = unsafe { OwnedFd::from_raw_fd(hfd) };

            // The accepted fd inherits O_NONBLOCK from the listener.  Switch to blocking
            // mode so that SO_RCVTIMEO applies and recvmsg_raw does not return EAGAIN
            // immediately when the client's data hasn't arrived yet.
            let _ = set_nonblocking(hfd.as_raw_fd(), false);

            // Send a 1-byte ACK so the client knows the connection is fully accepted.
            // On macOS, sendmsg with SCM_RIGHTS returns ENOTCONN if the client sends
            // before the server has called accept().  The client reads this ACK before
            // sending its fds, ensuring the connection is in ESTABLISHED state.
            // Errors are intentionally ignored: if the peer is a liveness probe and has
            // already closed, write() fails with EPIPE, and we continue normally.
            let ack = [0u8; 1];
            unsafe { libc::write(hfd.as_raw_fd(), ack.as_ptr() as *const libc::c_void, 1) };

            // Allow up to 50 ms for the client to send its fds.  On a loopback
            // connection this is always < 1 ms; the timeout guards against stale
            // half-open connections (e.g. from a crashed client before it could send).
            let _ = set_rcv_timeout(hfd.as_raw_fd(), 0, 50_000);

            let mut buf = [0u8; 8];
            let result = super::recvmsg_raw(hfd.as_raw_fd(), &mut buf, MsgFlags::empty());
            // hfd is dropped here, which closes the ephemeral STREAM connection.
            drop(hfd);

            let owned_fds = match result {
                Ok((_, fds)) => fds,
                Err(ref e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut
                        || e.raw_os_error() == Some(libc::EAGAIN) =>
                {
                    // Timeout: the client connected but didn't send fds within 50 ms.
                    // This is a liveness probe (`is_listening`) or a stale half-open
                    // connection.  Discard and try the next pending accept.
                    continue;
                }
                Err(ref e) if e.kind() == io::ErrorKind::BrokenPipe => {
                    // EOF before data: same as timeout case (probe / crashed client).
                    continue;
                }
                Err(e) => return Err(e),
            };

            let mut it = owned_fds.into_iter();
            if let Some(client_fd) = it.next() {
                // The second fd (if present) is the liveness pipe read end from `connect()`.
                return SeqpacketConn::from_owned(client_fd, it.next());
            }
            // No SCM_RIGHTS (shouldn't happen over STREAM, but guard defensively).
        }
    }
}

impl SeqpacketConn {
    /// Create a connected pair (SOCK_DGRAM with 4 MiB buffers, for testing / in-process use).
    pub fn socketpair() -> io::Result<(Self, Self)> {
        let mut fds = [0i32; 2];
        if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let fd0 = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let fd1 = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        set_dgram_buffers(fd0.as_raw_fd())?;
        set_dgram_buffers(fd1.as_raw_fd())?;
        Ok((Self::from_owned(fd0, None)?, Self::from_owned(fd1, None)?))
    }

    /// Connect to a server at the given filesystem path using the fd-passing handshake.
    ///
    /// Creates a `SOCK_DGRAM` socketpair with 4 MiB buffers and a liveness pipe, then
    /// connects a transient `SOCK_STREAM` socket to the rendezvous path and sends the
    /// server end of the socketpair **and** the read end of the liveness pipe to the
    /// server via SCM_RIGHTS.  Returns the client end of the socketpair.
    ///
    /// Using STREAM for the rendezvous avoids `EMSGSIZE` errors that macOS produces
    /// under high connection concurrency with DGRAM + SCM_RIGHTS.
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut fds = [0i32; 2];
        if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let fd_client = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let fd_server = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        set_dgram_buffers(fd_client.as_raw_fd())?;
        set_dgram_buffers(fd_server.as_raw_fd())?;

        // Create a liveness pipe.  The read end is sent to the daemon; we keep the
        // write end.  When the daemon drops its connection (closing liveness_read),
        // poll on liveness_write returns POLLHUP — enabling disconnect detection even
        // though _peer keeps the socketpair alive to prevent EINVAL on sendmsg.
        let mut pipe_fds = [-1i32; 2];
        if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let liveness_read = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
        let liveness_write = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
        // Set FD_CLOEXEC on both pipe ends so they are not inherited across exec().
        for &fd in &[liveness_read.as_raw_fd(), liveness_write.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags >= 0 {
                unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
            }
        }

        // Connect a transient STREAM socket to the rendezvous and send the fds.
        let handshake_fd = create_stream_socket()?;
        let server_raw = fd_server.as_raw_fd();
        let liveness_r_raw = liveness_read.as_raw_fd();
        let iov = [std::io::IoSlice::new(&[0u8])];
        with_short_path(path.as_ref(), |short| {
            // connect(2) to the STREAM rendezvous socket.
            let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
            sa.sun_family = libc::AF_UNIX as _;
            let path_bytes = short.as_os_str().as_bytes();
            if path_bytes.len() >= sa.sun_path.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "socket path too long for sun_path",
                ));
            }
            // SAFETY: sun_path is a C char array; we copy the path bytes in.
            for (i, &b) in path_bytes.iter().enumerate() {
                sa.sun_path[i] = b as libc::c_char;
            }
            let sa_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1)
                as libc::socklen_t;
            if unsafe {
                libc::connect(
                    handshake_fd.as_raw_fd(),
                    &sa as *const libc::sockaddr_un as *const libc::sockaddr,
                    sa_len,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            // On macOS, sendmsg with SCM_RIGHTS over a STREAM socket returns ENOTCONN
            // when the connection is in the server's listen backlog (not yet accepted).
            // Reading the 1-byte ACK sent by try_accept() ensures the server has
            // called accept() before we send the SCM_RIGHTS.
            let mut ack = [0u8; 1];
            if unsafe { libc::read(handshake_fd.as_raw_fd(), ack.as_mut_ptr() as *mut libc::c_void, 1) } <= 0 {
                return Err(io::Error::last_os_error());
            }
            sendmsg::<UnixAddr>(
                handshake_fd.as_raw_fd(),
                &iov,
                &[ControlMessage::ScmRights(&[server_raw, liveness_r_raw])],
                MsgFlags::empty(),
                None,
            )
            .map_err(io::Error::from)
        })?;
        // liveness_read was sent via SCM_RIGHTS; drop our local copy (daemon has the reference).
        drop(liveness_read);
        // handshake_fd drop closes the STREAM connection to the rendezvous.
        drop(handshake_fd);
        // Keep fd_server (_peer) to prevent EINVAL: on macOS, closing the local fd for the
        // peer end of a SOCK_DGRAM socketpair disconnects this end even when the peer socket
        // is alive in the daemon via SCM_RIGHTS.
        // Keep liveness_w (liveness_write) to detect daemon death via POLLHUP.
        Self::from_owned_pair(fd_client, fd_server, Some(liveness_write))
    }

    pub(super) fn poll_liveness_pipe(&self) -> io::Result<()> {
        if let Some(ref lw) = self.liveness {
            let mut pfd = libc::pollfd {
                fd: lw.as_raw_fd(),
                events: libc::POLLHUP as libc::c_short,
                revents: 0,
            };
            let ret = unsafe { libc::poll(&mut pfd, 1, 0) };
            if ret > 0 && pfd.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
        }
        Ok(())
    }

    /// Create from a connected fd plus a peer fd that must be kept alive.
    ///
    /// On macOS, the peer fd must be kept open locally to maintain the SOCK_DGRAM
    /// socketpair connection on this end.  It is stored in `_peer` and closed when
    /// this `SeqpacketConn` is dropped.
    pub(super) fn from_owned_pair(
        client: OwnedFd,
        peer: OwnedFd,
        liveness: Option<OwnedFd>,
    ) -> io::Result<Self> {
        set_nonblocking(client.as_raw_fd(), true)?;
        Ok(Self {
            inner: client,
            _peer: Some(peer),
            liveness,
            read_timeout: None,
            write_timeout: None,
        })
    }
}

/// Returns `true` if a live server is listening at the given socket path.
///
/// Connects a transient `SOCK_STREAM` socket to the path:
/// - Success → live server.
/// - `ECONNREFUSED` / `ENOENT` / `ENOTSOCK` → stale or absent socket.
pub fn is_listening<P: AsRef<Path>>(path: P) -> io::Result<bool> {
    if !path.as_ref().exists() {
        return Ok(false);
    }
    let probe = create_stream_socket()?;
    let result = with_short_path(path.as_ref(), |short| {
        let mut sa: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        sa.sun_family = libc::AF_UNIX as _;
        let path_bytes = short.as_os_str().as_bytes();
        if path_bytes.len() >= sa.sun_path.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket path too long",
            ));
        }
        for (i, &b) in path_bytes.iter().enumerate() {
            sa.sun_path[i] = b as libc::c_char;
        }
        let sa_len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1)
            as libc::socklen_t;
        let ret = unsafe {
            libc::connect(
                probe.as_raw_fd(),
                &sa as *const libc::sockaddr_un as *const libc::sockaddr,
                sa_len,
            )
        };
        if ret < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(true)
        }
    });
    match result {
        Ok(v) => Ok(v),
        Err(ref e)
            if e.kind() == io::ErrorKind::ConnectionRefused
                || e.kind() == io::ErrorKind::NotFound
                || e.raw_os_error() == Some(libc::ENOTSOCK)
                || e.raw_os_error() == Some(libc::EPROTOTYPE) =>
        {
            // EPROTOTYPE: stale socket file of the wrong type (e.g. DGRAM from a
            // previous build); treat as not listening so the caller removes it.
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

pub fn get_peer_credentials(fd: RawFd) -> io::Result<PeerCredentials> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCredentials {
        pid: pid as u32,
        uid: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that connect/accept round-trip works for both directions.
    #[test]
    fn test_connect_accept_send_recv() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let path = tmpdir.path().join("test.sock");
        let listener = SeqpacketListener::bind(&path).expect("bind");
        let client = SeqpacketConn::connect(&path).expect("connect");
        let server = listener.try_accept().expect("try_accept");

        // Client → server
        client
            .try_send_raw(&mut vec![1u8; 10], &[])
            .expect("client send");
        let mut buf = vec![0u8; 64];
        let (n, _) = server.try_recv_raw(&mut buf).expect("server recv");
        assert_eq!(&buf[..n], &[1u8; 10]);

        // Server → client (use a large enough buffer for 220 bytes)
        let mut buf220 = vec![0u8; 256];
        server
            .try_send_raw(&mut vec![2u8; 220], &[])
            .expect("server send 220B");
        let (n, _) = client.try_recv_raw(&mut buf220).expect("client recv");
        assert_eq!(n, 220);
    }

    /// Confirm macOS-specific SOCK_DGRAM socketpair behaviour: closing one end of a
    /// socketpair in the same process disconnects the other end.  This documents why
    /// `SeqpacketConn::connect` keeps `fd_server` alive in `_peer`.
    #[test]
    fn test_socketpair_peer_drop_disconnects() {
        let (conn0, conn1) = SeqpacketConn::socketpair().expect("socketpair");

        // Both ends alive: send must succeed.
        conn0
            .try_send_raw(&mut vec![42u8; 10], &[])
            .expect("send with peer alive");

        // Drop the peer: on macOS this disconnects conn0.
        drop(conn1);
        assert!(
            conn0.try_send_raw(&mut vec![42u8; 10], &[]).is_err(),
            "expected send error after dropping peer on macOS"
        );
    }

    /// Verify that is_listening returns true for a live server and false when nothing is bound.
    #[test]
    fn test_is_listening() {
        let tmpdir = tempfile::tempdir().expect("tempdir");
        let path = tmpdir.path().join("listen.sock");

        assert!(!is_listening(&path).expect("not listening yet"));

        let _listener = SeqpacketListener::bind(&path).expect("bind");
        assert!(is_listening(&path).expect("should be listening"));
    }
}
