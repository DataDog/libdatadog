// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Blocking TCP transport implemented with `rustix` system calls.
//!
//! [`TcpStream`] implements both the synchronous `embedded-io` traits and the
//! asynchronous `embedded-io-async` traits used by `reqwless`. The asynchronous
//! implementations intentionally make blocking system calls and never yield.
//! Use a runtime-specific transport when the calling task must remain
//! non-blocking.
//!
//! Without a timeout, an unresponsive peer blocks the caller indefinitely. Use
//! [`TcpStream::connect_timeout`] or [`TcpConnector::with_timeout`] whenever
//! the caller must make progress, for example from a crash handler.

use core::{fmt, net::SocketAddr, time::Duration};

use ::rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fd::OwnedFd,
    io::{Errno, ioctl_fionbio, retry_on_intr},
    net::{
        self, AddressFamily, SendFlags, SocketType,
        sockopt::{self, Timeout},
    },
    time::{ClockId, clock_gettime},
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
        let Self::Os(error) = *self else {
            return ErrorKind::WriteZero;
        };
        match error {
            Errno::ACCESS | Errno::PERM => ErrorKind::PermissionDenied,
            Errno::CONNREFUSED => ErrorKind::ConnectionRefused,
            Errno::CONNRESET => ErrorKind::ConnectionReset,
            Errno::CONNABORTED => ErrorKind::ConnectionAborted,
            Errno::NOTCONN => ErrorKind::NotConnected,
            Errno::ADDRINUSE => ErrorKind::AddrInUse,
            Errno::ADDRNOTAVAIL => ErrorKind::AddrNotAvailable,
            Errno::PIPE => ErrorKind::BrokenPipe,
            Errno::INVAL => ErrorKind::InvalidInput,
            // Sockets are blocking, so `EAGAIN` only means a socket timeout expired.
            Errno::TIMEDOUT | Errno::AGAIN => ErrorKind::TimedOut,
            Errno::INTR => ErrorKind::Interrupted,
            Errno::NOMEM => ErrorKind::OutOfMemory,
            _ => ErrorKind::Other,
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
        let socket = open_socket(remote)?;
        net::connect(&socket, &remote)?;
        Ok(Self { socket })
    }

    /// Opens a blocking TCP connection to `remote`, bounding the connection
    /// attempt and every later read and write by `timeout`.
    ///
    /// Reads and writes that exceed `timeout` fail with an error whose kind is
    /// [`ErrorKind::TimedOut`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Os`] when `timeout` is zero, when socket creation or
    /// configuration fails, or when the connection fails or times out.
    pub fn connect_timeout(remote: SocketAddr, timeout: Duration) -> Result<Self, Error> {
        if timeout.is_zero() {
            return Err(Errno::INVAL.into());
        }
        let socket = open_socket(remote)?;
        sockopt::set_socket_timeout(&socket, Timeout::Recv, Some(timeout))?;
        sockopt::set_socket_timeout(&socket, Timeout::Send, Some(timeout))?;

        ioctl_fionbio(&socket, true)?;
        match net::connect(&socket, &remote) {
            Ok(()) => {}
            Err(Errno::INPROGRESS) => wait_until_connected(&socket, timeout)?,
            Err(error) => return Err(error.into()),
        }
        ioctl_fionbio(&socket, false)?;
        Ok(Self { socket })
    }
}

/// Creates a TCP socket for `remote` that is closed on exec and does not raise
/// `SIGPIPE`.
fn open_socket(remote: SocketAddr) -> Result<OwnedFd, Error> {
    let family = match remote {
        SocketAddr::V4(_) => AddressFamily::INET,
        SocketAddr::V6(_) => AddressFamily::INET6,
    };
    #[cfg(not(any(target_vendor = "apple", target_os = "aix")))]
    let socket = net::socket_with(
        family,
        SocketType::STREAM,
        net::SocketFlags::CLOEXEC,
        Some(net::ipproto::TCP),
    )?;
    // `SOCK_CLOEXEC` is unavailable here, so set the flag right after creation.
    #[cfg(any(target_vendor = "apple", target_os = "aix"))]
    let socket = {
        let socket = net::socket(family, SocketType::STREAM, Some(net::ipproto::TCP))?;
        ::rustix::io::fcntl_setfd(&socket, ::rustix::io::FdFlags::CLOEXEC)?;
        socket
    };
    #[cfg(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    suppress_sigpipe(&socket)?;
    Ok(socket)
}

/// Waits for a non-blocking connection attempt to complete within `timeout`.
fn wait_until_connected(socket: &OwnedFd, timeout: Duration) -> Result<(), Error> {
    let start = monotonic_now()?;
    loop {
        let remaining = timeout
            .checked_sub(monotonic_now()?.saturating_sub(start))
            .filter(|remaining| !remaining.is_zero())
            .ok_or(Errno::TIMEDOUT)?;
        let remaining = Timespec::try_from(remaining).map_err(|_| Errno::INVAL)?;
        let mut fds = [PollFd::new(socket, PollFlags::OUT)];
        match poll(&mut fds, Some(&remaining)) {
            Ok(0) => return Err(Errno::TIMEDOUT.into()),
            Ok(_) => return sockopt::socket_error(socket)?.map_err(Into::into),
            Err(Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn monotonic_now() -> Result<Duration, Error> {
    Duration::try_from(clock_gettime(ClockId::Monotonic)).map_err(|_| Errno::INVAL.into())
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

// Each future runs a blocking call when polled.
impl embedded_io_async::Read for TcpStream {
    fn read(
        &mut self,
        buffer: &mut [u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>> {
        core::future::poll_fn(move |_| {
            core::task::Poll::Ready(embedded_io::Read::read(self, buffer))
        })
    }
}

impl embedded_io_async::Write for TcpStream {
    fn write(
        &mut self,
        buffer: &[u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>> {
        core::future::poll_fn(move |_| {
            core::task::Poll::Ready(embedded_io::Write::write(self, buffer))
        })
    }

    fn flush(&mut self) -> impl core::future::Future<Output = Result<(), Self::Error>> {
        core::future::poll_fn(move |_| core::task::Poll::Ready(embedded_io::Write::flush(self)))
    }
}

/// Creates blocking [`TcpStream`] connections for the async `reqwless` client.
///
/// Connections block indefinitely unless a timeout is configured with
/// [`TcpConnector::with_timeout`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpConnector {
    timeout: Option<Duration>,
}

impl TcpConnector {
    /// Creates a connector whose connections never time out.
    #[must_use]
    pub const fn new() -> Self {
        Self { timeout: None }
    }

    /// Creates a connector that bounds each connection attempt and every later
    /// read and write by `timeout`. See [`TcpStream::connect_timeout`].
    #[must_use]
    pub const fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
        }
    }
}

impl embedded_nal_async::TcpConnect for TcpConnector {
    type Error = Error;
    type Connection<'a> = TcpStream;

    fn connect(
        &self,
        remote: SocketAddr,
    ) -> impl core::future::Future<Output = Result<Self::Connection<'_>, Self::Error>> {
        core::future::poll_fn(move |_| {
            core::task::Poll::Ready(match self.timeout {
                Some(timeout) => TcpStream::connect_timeout(remote, timeout),
                None => TcpStream::connect(remote),
            })
        })
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
    use core::{error::Error as StdError, net::SocketAddr, time::Duration};
    use std::{
        io::{Error as IoError, ErrorKind as IoErrorKind, Read as _, Write as _},
        net::TcpListener,
    };

    use embedded_io::{Error as _, ErrorKind};

    use super::{Error, TcpStream};

    /// Connects with `connect` to a local echo server and checks a round trip.
    fn assert_echo_round_trip(
        connect: impl FnOnce(SocketAddr) -> Result<TcpStream, Error>,
    ) -> Result<(), Box<dyn StdError>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let remote = listener.local_addr()?;

        std::thread::scope(|scope| -> Result<(), Box<dyn StdError>> {
            let server = scope.spawn(|| -> Result<(), IoError> {
                let (mut connection, _) = listener.accept()?;
                let mut request = [0_u8; 4];
                connection.read_exact(&mut request)?;
                connection.write_all(&request)
            });

            let mut client = connect(remote)?;
            embedded_io::Write::write_all(&mut client, b"ping")?;
            let mut response = [0_u8; 4];
            embedded_io::Read::read_exact(&mut client, &mut response)?;
            if &response != b"ping" {
                return Err(IoError::new(IoErrorKind::InvalidData, "unexpected response").into());
            }

            server
                .join()
                .map_err(|_| IoError::other("server thread panicked"))??;
            Ok(())
        })
    }

    #[test]
    fn connects_and_exchanges_bytes() -> Result<(), Box<dyn StdError>> {
        assert_echo_round_trip(TcpStream::connect)
    }

    #[test]
    fn connect_timeout_exchanges_bytes() -> Result<(), Box<dyn StdError>> {
        assert_echo_round_trip(|remote| TcpStream::connect_timeout(remote, Duration::from_secs(5)))
    }

    #[test]
    fn read_times_out_when_peer_is_silent() -> Result<(), Box<dyn StdError>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let remote = listener.local_addr()?;

        let mut client = TcpStream::connect_timeout(remote, Duration::from_millis(50))?;
        // Keep the accepted connection open without ever writing to it.
        let (_connection, _) = listener.accept()?;
        let mut response = [0_u8; 1];
        let error = embedded_io::Read::read(&mut client, &mut response)
            .err()
            .ok_or("read unexpectedly succeeded")?;
        if error.kind() != ErrorKind::TimedOut {
            return Err(format!("expected a timeout, got {error}").into());
        }
        Ok(())
    }

    #[test]
    fn connect_timeout_rejects_zero_timeout() -> Result<(), Box<dyn StdError>> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let error = TcpStream::connect_timeout(listener.local_addr()?, Duration::ZERO)
            .err()
            .ok_or("connect unexpectedly succeeded")?;
        if !matches!(error, Error::Os(_)) || error.kind() != ErrorKind::InvalidInput {
            return Err(format!("expected an invalid-input error, got {error}").into());
        }
        Ok(())
    }
}
