// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Linux-specific IPC socket implementation using `AF_UNIX SOCK_SEQPACKET`.

use super::{create_unix_socket, PeerCredentials, SeqpacketConn, SeqpacketListener};
use nix::sys::socket::{accept, bind, connect, listen, Backlog, SockType, UnixAddr};
use std::os::fd::RawFd;
use std::{
    io,
    os::unix::{
        io::{AsRawFd, FromRawFd, OwnedFd},
        prelude::OsStrExt,
    },
    path::Path,
};

fn create_seqpacket_socket() -> io::Result<OwnedFd> {
    create_unix_socket(SockType::SeqPacket)
}

impl SeqpacketListener {
    /// Bind to a filesystem path and start listening (SEQPACKET, backlog 128).
    ///
    /// Removes any stale socket file before binding (standard Unix practice).
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let _ = std::fs::remove_file(path.as_ref());
        let fd = create_seqpacket_socket()?;
        let addr = UnixAddr::new(path.as_ref()).map_err(io::Error::from)?;
        bind(fd.as_raw_fd(), &addr).map_err(io::Error::from)?;
        listen(&fd, Backlog::new(128).map_err(io::Error::from)?).map_err(io::Error::from)?;
        Ok(Self { inner: fd })
    }

    /// Bind to a Linux abstract socket name and start listening.
    pub fn bind_abstract(name: &[u8]) -> io::Result<Self> {
        let fd = create_seqpacket_socket()?;
        let addr = UnixAddr::new_abstract(name).map_err(io::Error::from)?;
        bind(fd.as_raw_fd(), &addr).map_err(io::Error::from)?;
        listen(&fd, Backlog::new(128).map_err(io::Error::from)?).map_err(io::Error::from)?;
        Ok(Self { inner: fd })
    }

    /// Accept a new connection (non-blocking in non-blocking mode).
    ///
    /// Skips intermittent connections left by `is_listening` probes: after `accept()`, peek to
    /// check if the peer has already closed the connection (EOF). If so, discard and loop.
    pub fn try_accept(&self) -> io::Result<SeqpacketConn> {
        loop {
            let new_fd = accept(self.inner.as_raw_fd()).map_err(io::Error::from)?;
            let owned = unsafe { OwnedFd::from_raw_fd(new_fd) };
            let conn = SeqpacketConn::from_owned(owned)?;
            // Peek to detect EOF phantom connections left by is_listening probes.
            let mut probe = [0u8; 1];
            let n = unsafe {
                libc::recv(
                    conn.inner.as_raw_fd(),
                    probe.as_mut_ptr() as *mut libc::c_void,
                    1,
                    libc::MSG_PEEK | libc::MSG_DONTWAIT,
                )
            };
            if n == 0 {
                // EOF: peer closed before sending anything; discard this phantom connection.
                continue;
            }
            return Ok(conn);
        }
    }
}

impl SeqpacketConn {
    /// Create a connected pair (SEQPACKET, for testing / in-process use).
    pub fn socketpair() -> io::Result<(Self, Self)> {
        let mut fds = [0i32; 2];
        if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, fds.as_mut_ptr()) }
            == -1
        {
            return Err(io::Error::last_os_error());
        }
        let fd0 = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let fd1 = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        Ok((Self::from_owned(fd0)?, Self::from_owned(fd1)?))
    }

    /// Connect to a filesystem socket path.
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let fd = create_seqpacket_socket()?;
        let addr = UnixAddr::new(path.as_ref()).map_err(io::Error::from)?;
        connect(fd.as_raw_fd(), &addr).map_err(io::Error::from)?;
        Self::from_owned(fd)
    }

    /// Connect to a Linux abstract socket name.
    pub fn connect_abstract(name: &[u8]) -> io::Result<Self> {
        let fd = create_seqpacket_socket()?;
        let addr = UnixAddr::new_abstract(name).map_err(io::Error::from)?;
        connect(fd.as_raw_fd(), &addr).map_err(io::Error::from)?;
        Self::from_owned(fd)
    }
}

/// Returns `true` if a SEQPACKET server is listening on `path`.
///
/// Attempts `connect()` — succeeds only if a server is actively `accept()`-ing.
pub fn is_listening<P: AsRef<Path>>(path: P) -> io::Result<bool> {
    if !path.as_ref().exists() {
        return Ok(false);
    }
    Ok(SeqpacketConn::connect(path).is_ok())
}

/// Connect to a Linux abstract socket (path used as name bytes).
pub fn connect_abstract<P: AsRef<Path>>(path: P) -> io::Result<SeqpacketConn> {
    SeqpacketConn::connect_abstract(path.as_ref().as_os_str().as_bytes())
}

/// Bind an abstract socket (path used as name bytes).
pub fn bind_abstract<P: AsRef<Path>>(path: P) -> io::Result<SeqpacketListener> {
    SeqpacketListener::bind_abstract(path.as_ref().as_os_str().as_bytes())
}

pub fn get_peer_credentials(fd: RawFd) -> io::Result<PeerCredentials> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCredentials {
        pid: cred.pid as u32,
        uid: cred.uid,
    })
}
