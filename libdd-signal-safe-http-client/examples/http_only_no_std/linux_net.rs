// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::net::SocketAddr;

use libdd_signal_safe_http_client::io::{
    embedded_io::{ErrorKind, ErrorType},
    embedded_io_async::{Read, Write},
    embedded_nal_async::TcpConnect,
};
use rustix::{
    fd::OwnedFd,
    io::Errno,
    net::{self, AddressFamily, RecvFlags, SendFlags, SocketType},
};

pub(super) struct RustixTcpConnector;

impl TcpConnect for RustixTcpConnector {
    type Error = ErrorKind;
    type Connection<'a>
        = RustixTcpConnection
    where
        Self: 'a;

    async fn connect(&self, remote: SocketAddr) -> Result<Self::Connection<'_>, Self::Error> {
        let family = match remote {
            SocketAddr::V4(_) => AddressFamily::INET,
            SocketAddr::V6(_) => AddressFamily::INET6,
        };
        let fd =
            net::socket(family, SocketType::STREAM, Some(net::ipproto::TCP)).map_err(map_errno)?;
        net::connect(&fd, &remote).map_err(map_errno)?;

        Ok(RustixTcpConnection { fd })
    }
}

pub(super) struct RustixTcpConnection {
    fd: OwnedFd,
}

impl ErrorType for RustixTcpConnection {
    type Error = ErrorKind;
}

impl Read for RustixTcpConnection {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        net::recv(&self.fd, buffer, RecvFlags::empty())
            .map(|(read, _)| read)
            .map_err(map_errno)
    }
}

impl Write for RustixTcpConnection {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        let written = net::send(&self.fd, buffer, SendFlags::empty()).map_err(map_errno)?;
        if written == 0 && !buffer.is_empty() {
            return Err(ErrorKind::WriteZero);
        }

        Ok(written)
    }

    async fn write_all(&mut self, mut buffer: &[u8]) -> Result<(), Self::Error> {
        while !buffer.is_empty() {
            let written = self.write(buffer).await?;
            if written == 0 {
                return Err(ErrorKind::WriteZero);
            }
            buffer = &buffer[written..];
        }

        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

const fn map_errno(errno: Errno) -> ErrorKind {
    match errno {
        Errno::INTR => ErrorKind::Interrupted,
        Errno::CONNREFUSED => ErrorKind::ConnectionRefused,
        Errno::CONNRESET => ErrorKind::ConnectionReset,
        Errno::CONNABORTED => ErrorKind::ConnectionAborted,
        Errno::NOTCONN => ErrorKind::NotConnected,
        Errno::ADDRINUSE => ErrorKind::AddrInUse,
        Errno::ADDRNOTAVAIL => ErrorKind::AddrNotAvailable,
        Errno::PIPE => ErrorKind::BrokenPipe,
        Errno::EXIST => ErrorKind::AlreadyExists,
        Errno::INVAL => ErrorKind::InvalidInput,
        Errno::TIMEDOUT => ErrorKind::TimedOut,
        Errno::NOMEM => ErrorKind::OutOfMemory,
        _ => ErrorKind::Other,
    }
}
