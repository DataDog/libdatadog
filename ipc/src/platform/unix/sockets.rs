// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{io, os::unix::net::UnixStream, path::Path};
pub fn is_listening<P: AsRef<Path>>(path: P) -> io::Result<bool> {
    if !path.as_ref().exists() {
        return Ok(false);
    }
    Ok(UnixStream::connect(path).is_ok())
}
#[cfg(target_os = "linux")]
mod linux {
    use std::{
        io,
        os::unix::{
            net::{UnixListener, UnixStream},
            prelude::{AsRawFd, OsStrExt},
        },
        path::Path,
    };

    use io_lifetimes::OwnedFd;
    use nix::sys::socket::{
        bind, connect, listen, socket, AddressFamily, Backlog, SockFlag, SockType, UnixAddr,
    };

    fn socket_stream() -> nix::Result<OwnedFd> {
        socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::SOCK_CLOEXEC,
            None,
        )
    }

    pub fn connect_abstract<P: AsRef<Path>>(path: P) -> io::Result<UnixStream> {
        let sock = socket_stream()?;
        let addr = UnixAddr::new_abstract(path.as_ref().as_os_str().as_bytes())?;
        connect(sock.as_raw_fd(), &addr)?;
        Ok(sock.into())
    }

    pub fn bind_abstract<P: AsRef<Path>>(path: P) -> io::Result<UnixListener> {
        let sock = socket_stream()?;
        let addr = UnixAddr::new_abstract(path.as_ref().as_os_str().as_bytes())?;
        bind(sock.as_raw_fd(), &addr)?;
        // This was previously 128, but due to this bug in 0.29.0 which has
        // been fixed but not released, we're using 127:
        // https://github.com/nix-rust/nix/pull/2500
        const SOMAXCONN: i32 = 127;
        listen(&sock, Backlog::new(SOMAXCONN)?)?;
        Ok(sock.into())
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;
