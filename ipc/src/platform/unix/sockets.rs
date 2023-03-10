// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

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
            prelude::{AsRawFd, FromRawFd, OsStrExt},
        },
        path::Path,
    };

    use io_lifetimes::OwnedFd;
    use nix::sys::socket::{
        bind, connect, listen, socket, AddressFamily, SockFlag, SockType, UnixAddr,
    };

    fn socket_stream() -> io::Result<OwnedFd> {
        let fd = socket(
            AddressFamily::Unix,
            SockType::Stream,
            SockFlag::SOCK_CLOEXEC,
            None,
        )?;

        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
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
        listen(sock.as_raw_fd(), 128)?;
        Ok(sock.into())
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;
