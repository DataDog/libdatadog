// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! macOS IPC socket implementation using `AF_UNIX SOCK_DGRAM` with an fd-passing handshake.
//!
//! ## Connection protocol
//!
//! macOS does not support `AF_UNIX SOCK_SEQPACKET`, so we emulate the same semantics:
//!
//! **Server side** (`SeqpacketListener`):
//! - Binds a `SOCK_DGRAM` socket to a filesystem path (the "rendezvous" socket).
//! - Calls `try_accept()` which does `recvmsg()` and extracts the client fd from SCM_RIGHTS.
//!   Messages without SCM_RIGHTS (liveness probes) are silently discarded.
//!
//! **Client side** (`SeqpacketConn::connect`):
//! - Creates a `socketpair(AF_UNIX, SOCK_DGRAM)` with 4 MiB send/recv buffers.
//! - Sends one socketpair end to the server's rendezvous path via a **fresh, unconnected** DGRAM
//!   socket (using `sendmsg` with `SCM_RIGHTS`). The client retains the other end.
//!
//! **Liveness probe** (`is_listening`):
//! - Sends a 1-byte datagram **without** SCM_RIGHTS to the rendezvous socket.
//! - Success → live server.  `ECONNRESET` → stale socket file.

use super::{
    create_unix_socket, max_message_size, sendmsg, set_nonblocking, ControlMessage, MsgFlags,
    SeqpacketConn, SeqpacketListener, UnixAddr,
};
use crate::PeerCredentials;
use nix::sys::socket::{bind, AddressFamily, SockFlag, SockType};
use std::os::fd::RawFd;
use std::{
    io,
    os::unix::io::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
};

fn create_dgram_socket() -> io::Result<OwnedFd> {
    create_unix_socket(SockType::Datagram)
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

impl SeqpacketListener {
    /// Bind to a filesystem path (DGRAM rendezvous socket; no `listen()` needed).
    ///
    /// Removes any stale socket file before binding (standard Unix practice).
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let _ = std::fs::remove_file(path.as_ref());
        let fd = create_dgram_socket()?;
        let addr = UnixAddr::new(path.as_ref()).map_err(io::Error::from)?;
        bind(fd.as_raw_fd(), &addr).map_err(io::Error::from)?;
        Ok(Self { inner: fd })
    }

    /// Accept a connection by receiving a client fd via SCM_RIGHTS.
    ///
    /// Returns `Err(WouldBlock)` when no connection is pending.
    /// Silently discards messages without SCM_RIGHTS (liveness probes from `is_listening`).
    pub fn try_accept(&self) -> io::Result<SeqpacketConn> {
        loop {
            let mut buf = [0u8; 1];
            let (_, owned_fds) =
                super::recvmsg_raw(self.inner.as_raw_fd(), &mut buf, MsgFlags::MSG_DONTWAIT)?;
            let mut it = owned_fds.into_iter();
            if let Some(client_fd) = it.next() {
                // The second fd (if present) is the liveness pipe read end from `connect()`.
                // Holding it alive lets the client detect when we drop this connection.
                // Unlike socketpairs, pipes aren't autoclosed when the transferred end is closed
                // locally.
                return SeqpacketConn::from_owned(client_fd, it.next());
            }
            // No SCM_RIGHTS: liveness probe — discard and try the next message.
            // If the socket is empty, the next recvmsg call returns WouldBlock.
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
    /// sends the server end of the socketpair **and** the read end of the liveness pipe
    /// to the rendezvous socket via SCM_RIGHTS.  Returns the client end of the socketpair.
    ///
    /// The liveness pipe enables disconnect detection: when the daemon drops its
    /// `SeqpacketConn` (closing `liveness_read`), `POLLHUP` appears on `liveness_write`
    /// and subsequent sends return `BrokenPipe`.
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

        // A fresh unconnected DGRAM socket is required for the handshake sendmsg.
        // fd_client is already "connected" to fd_server and cannot reach the rendezvous path.
        let handshake_fd = create_dgram_socket()?;
        let addr = UnixAddr::new(path.as_ref()).map_err(io::Error::from)?;
        let server_raw = fd_server.as_raw_fd();
        let liveness_r_raw = liveness_read.as_raw_fd();
        let iov = [std::io::IoSlice::new(&[0u8])];
        sendmsg::<UnixAddr>(
            handshake_fd.as_raw_fd(),
            &iov,
            &[ControlMessage::ScmRights(&[server_raw, liveness_r_raw])],
            MsgFlags::empty(),
            Some(&addr),
        )
        .map_err(io::Error::from)?;
        // liveness_read was sent via SCM_RIGHTS; drop our local copy (daemon has the reference).
        drop(liveness_read);
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
/// Sends a 1-byte probe datagram (no SCM_RIGHTS) to the path.
/// - Success → live server (the server's `try_accept` silently discards the probe).
/// - `ECONNRESET` → stale socket file (no live receiver).
pub fn is_listening<P: AsRef<Path>>(path: P) -> io::Result<bool> {
    if !path.as_ref().exists() {
        return Ok(false);
    }
    let probe = nix::sys::socket::socket(
        AddressFamily::Unix,
        SockType::Datagram,
        SockFlag::empty(),
        None,
    )
    .map_err(io::Error::from)?;
    let addr = UnixAddr::new(path.as_ref()).map_err(io::Error::from)?;
    let iov = [std::io::IoSlice::new(&[0u8])];
    Ok(sendmsg::<UnixAddr>(probe.as_raw_fd(), &iov, &[], MsgFlags::empty(), Some(&addr)).is_ok())
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
}
