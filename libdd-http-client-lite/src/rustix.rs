// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Blocking TCP transport implemented with `rustix` system calls.
//!
//! [`TcpStream`] implements both the synchronous `embedded-io` traits and the
//! asynchronous `embedded-io-async` traits used by `reqwless`. The asynchronous
//! implementations intentionally make blocking system calls and never yield.
//! Use a runtime-specific transport when the calling task must remain
//! non-blocking.

use core::{fmt, net::SocketAddr};

use ::rustix::{
    fd::OwnedFd,
    io::{retry_on_intr, Errno},
    net::{self, AddressFamily, SendFlags, SocketType},
};
use embedded_io::{ErrorKind, ErrorType};

/// Error returned by the `rustix` TCP transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// An operating-system call failed.
    Os(Errno),
    /// A non-empty write completed without writing bytes.
    WriteZero,
}

impl Error {
    /// Returns the underlying operating-system error number, if present.
    #[must_use]
    pub const fn raw_os_error(self) -> Option<i32> {
        match self {
            Self::Os(error) => Some(error.raw_os_error()),
            Self::WriteZero => None,
        }
    }
}

impl From<Errno> for Error {
    fn from(error: Errno) -> Self {
        Self::Os(error)
    }
}

impl embedded_io::Error for Error {
    fn kind(&self) -> ErrorKind {
        let Self::Os(error) = self else {
            return ErrorKind::WriteZero;
        };

        if *error == Errno::ACCESS || *error == Errno::PERM {
            ErrorKind::PermissionDenied
        } else if *error == Errno::CONNREFUSED {
            ErrorKind::ConnectionRefused
        } else if *error == Errno::CONNRESET {
            ErrorKind::ConnectionReset
        } else if *error == Errno::CONNABORTED {
            ErrorKind::ConnectionAborted
        } else if *error == Errno::NOTCONN {
            ErrorKind::NotConnected
        } else if *error == Errno::ADDRINUSE {
            ErrorKind::AddrInUse
        } else if *error == Errno::ADDRNOTAVAIL {
            ErrorKind::AddrNotAvailable
        } else if *error == Errno::PIPE {
            ErrorKind::BrokenPipe
        } else if *error == Errno::INVAL {
            ErrorKind::InvalidInput
        } else if *error == Errno::TIMEDOUT {
            ErrorKind::TimedOut
        } else if *error == Errno::INTR {
            ErrorKind::Interrupted
        } else if *error == Errno::NOMEM {
            ErrorKind::OutOfMemory
        } else {
            ErrorKind::Other
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Os(error) => write!(formatter, "TCP system call failed: {error}"),
            Self::WriteZero => formatter.write_str("TCP write returned zero bytes"),
        }
    }
}

#[cfg(feature = "std")]
impl core::error::Error for Error {}

/// A blocking TCP connection backed by an owned `rustix` socket.
///
/// Dropping the stream closes its file descriptor.
#[derive(Debug)]
pub struct TcpStream {
    socket: OwnedFd,
}

impl TcpStream {
    /// Opens a blocking TCP connection to `remote`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Os`] when socket creation, configuration, or connection
    /// fails.
    pub fn connect(remote: SocketAddr) -> Result<Self, Error> {
        let family = match remote {
            SocketAddr::V4(_) => AddressFamily::INET,
            SocketAddr::V6(_) => AddressFamily::INET6,
        };
        let socket = net::socket(family, SocketType::STREAM, Some(net::ipproto::TCP))?;
        suppress_sigpipe(&socket)?;
        net::connect(&socket, &remote)?;
        Ok(Self { socket })
    }
}

impl ErrorType for TcpStream {
    type Error = Error;
}

impl embedded_io::Read for TcpStream {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        retry_on_intr(|| ::rustix::io::read(&self.socket, &mut *buffer)).map_err(Into::into)
    }
}

impl embedded_io::Write for TcpStream {
    fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        let written = retry_on_intr(|| net::send(&self.socket, buffer, send_flags()))?;
        if buffer.is_empty() || written != 0 {
            Ok(written)
        } else {
            Err(Error::WriteZero)
        }
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl embedded_io_async::Read for TcpStream {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        embedded_io::Read::read(self, buffer)
    }
}

impl embedded_io_async::Write for TcpStream {
    async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
        embedded_io::Write::write(self, buffer)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_io::Write::flush(self)
    }
}

/// Creates blocking [`TcpStream`] connections for the async `reqwless` client.
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpConnector;

impl embedded_nal_async::TcpConnect for TcpConnector {
    type Error = Error;
    type Connection<'a> = TcpStream;

    async fn connect(&self, remote: SocketAddr) -> Result<Self::Connection<'_>, Self::Error> {
        TcpStream::connect(remote)
    }
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd",
    target_os = "netbsd"
))]
fn suppress_sigpipe(socket: &OwnedFd) -> Result<(), Error> {
    net::sockopt::set_socket_nosigpipe(socket, true).map_err(Into::into)
}

#[cfg(not(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "openbsd",
    target_os = "netbsd"
)))]
fn suppress_sigpipe(_socket: &OwnedFd) -> Result<(), Error> {
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const fn send_flags() -> SendFlags {
    SendFlags::NOSIGNAL
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
const fn send_flags() -> SendFlags {
    SendFlags::empty()
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use core::error::Error as StdError;
    use std::{
        io::{Error as IoError, ErrorKind as IoErrorKind, Read as _, Write as _},
        net::TcpListener,
    };

    use super::TcpStream;

    #[test]
    fn connects_and_exchanges_bytes() -> Result<(), Box<dyn StdError>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let remote = listener.local_addr()?;

        std::thread::scope(|scope| -> Result<(), Box<dyn StdError>> {
            let server = scope.spawn(|| -> Result<(), IoError> {
                let (mut connection, _) = listener.accept()?;
                let mut request = [0_u8; 4];
                connection.read_exact(&mut request)?;
                if &request != b"ping" {
                    return Err(IoError::new(IoErrorKind::InvalidData, "unexpected request"));
                }
                connection.write_all(b"pong")
            });

            let mut client = TcpStream::connect(remote)?;
            embedded_io::Write::write_all(&mut client, b"ping")?;
            let mut response = [0_u8; 4];
            embedded_io::Read::read_exact(&mut client, &mut response)?;
            if &response != b"pong" {
                return Err(IoError::new(IoErrorKind::InvalidData, "unexpected response").into());
            }

            server
                .join()
                .map_err(|_| IoError::other("server thread panicked"))??;
            Ok(())
        })
    }
}
