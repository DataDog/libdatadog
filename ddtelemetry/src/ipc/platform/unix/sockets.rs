// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    io,
    os::unix::{
        net::{UnixListener, UnixStream},
        prelude::{FromRawFd, IntoRawFd, RawFd},
    },
    path::Path,
};

pub trait IsListening {
    fn is_listening<P: AsRef<Path>>(path: P) -> io::Result<bool>;
}

impl IsListening for UnixListener {
    fn is_listening<P: AsRef<Path>>(path: P) -> io::Result<bool> {
        if !path.as_ref().exists() {
            return Ok(false);
        }
        Ok(UnixStream::connect(path).is_ok())
    }
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

    pub trait UnixStreamConnectAbstract {
        fn connect_abstract<P: AsRef<Path>>(path: P) -> io::Result<UnixStream>;
    }

    impl UnixStreamConnectAbstract for UnixStream {
        fn connect_abstract<P: AsRef<Path>>(path: P) -> io::Result<UnixStream> {
            let sock = socket_stream()?;
            let addr = UnixAddr::new_abstract(path.as_ref().as_os_str().as_bytes())?;
            connect(sock.as_raw_fd(), &addr)?;
            Ok(sock.into())
        }
    }

    pub trait UnixListenerBindAbstract {
        fn bind_abstract<P: AsRef<Path>>(path: P) -> io::Result<UnixListener>;
    }

    impl UnixListenerBindAbstract for UnixListener {
        fn bind_abstract<P: AsRef<Path>>(path: P) -> io::Result<UnixListener> {
            let sock = socket_stream()?;
            let addr = UnixAddr::new_abstract(path.as_ref().as_os_str().as_bytes())?;
            bind(sock.as_raw_fd(), &addr)?;
            listen(sock.as_raw_fd(), 128)?;
            Ok(sock.into())
        }
    }
}

use io_lifetimes::OwnedFd;
#[cfg(target_os = "linux")]
pub use linux::*;

#[must_use]
#[derive(Debug, Clone)]
pub struct ForkableUnixHandlePair {
    local: RawFd,
    remote: RawFd,
}

impl ForkableUnixHandlePair {
    pub fn new() -> io::Result<Self> {
        let (local, remote) = UnixStream::pair()?;
        Ok(Self {
            local: local.into_raw_fd(),
            remote: remote.into_raw_fd(),
        })
    }

    /// returns socket from pair meant to use locally
    ///
    /// # Safety
    ///
    /// Caller must call the method only once per process instance
    pub unsafe fn local(&self) -> UnixStream {
        let _remote: OwnedFd = OwnedFd::from_raw_fd(self.remote);

        UnixStream::from_raw_fd(self.local)
    }

    /// returns socket from pair meant to used in spawned process
    ///
    /// # Safety
    ///
    /// Caller must call the method only once per process instance
    pub unsafe fn remote(&self) -> UnixStream {
        let _local: OwnedFd = OwnedFd::from_raw_fd(self.local);

        UnixStream::from_raw_fd(self.remote)
    }
}
