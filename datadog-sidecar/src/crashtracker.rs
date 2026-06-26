// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Crashtracker integration over the single sidecar IPC socket.
//!
//! Instead of a second dedicated listener, the crash-time collector connects to the same
//! `SOCK_SEQPACKET` IPC socket and sends an
//! [`enter_crashtracker_receiver`](crate::service::SidecarInterface::enter_crashtracker_receiver)
//! request as its first message — a normal codec-encoded message (see
//! [`crashtracker_receiver_request_bytes`]), decoded by the regular serve loop with no marker or
//! peeking — then streams the crash report over the same connection.
//!
//! The receiver consumes an `AsyncBufRead` byte stream; since SEQPACKET preserves message
//! boundaries, [`SeqpacketStreamReader`] concatenates the collector's datagrams back into a
//! contiguous stream (a zero-length datagram is EOF).

#[cfg(unix)]
use datadog_ipc::AsyncConn;

/// The IPC socket the crash-time collector connects to — the same socket the sidecar serves on, so
/// this mirrors how the sidecar picks it:
///   - thread mode (`master_pid != 0`): the master's in-process listener, keyed by the master PID
///     (like `connect_to_master`), independent of `ipc_mode`.
///   - subprocess mode: exactly `start_or_connect_to_sidecar`'s choice, via the shared
///     `liaison_for_ipc_mode`.
///
/// On Linux this is an abstract socket name; elsewhere a filesystem path.
#[cfg(unix)]
pub fn crashtracker_ipc_socket_path(
    master_pid: u32,
    ipc_mode: crate::config::IpcMode,
) -> std::path::PathBuf {
    let liaison = if master_pid != 0 {
        crate::setup::DefaultLiason::ipc_for_pid(master_pid)
    } else {
        crate::setup::liaison_for_ipc_mode(ipc_mode)
    };
    #[cfg(target_os = "linux")]
    {
        liaison.path().to_path_buf()
    }
    #[cfg(not(target_os = "linux"))]
    {
        liaison.socket_path().to_path_buf()
    }
}

/// The codec-encoded `enter_crashtracker_receiver` request the collector sends as its first
/// message. The request is parameterless, so the bytes are fixed — computed once and valid for the
/// process lifetime (safe to send from a crash handler).
#[cfg(unix)]
pub fn crashtracker_receiver_request_bytes() -> &'static [u8] {
    use crate::service::SidecarInterfaceRequest;
    static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BYTES.get_or_init(|| {
        datadog_ipc::codec::encode(&SidecarInterfaceRequest::EnterCrashtrackerReceiver {})
    })
}

/// `unix_socket_connector` for the crashtracker config: connect a `SOCK_SEQPACKET` socket to the
/// sidecar IPC socket at `unix_socket_path` and send the encoded `enter_crashtracker_receiver`
/// request as the first message, returning the connected fd (`-1` on error). Runs in the crash
/// handler with no allocation, so [`crashtracker_receiver_request_bytes`] must be primed earlier.
#[cfg(unix)]
pub fn connect_to_sidecar_receiver(unix_socket_path: &str) -> std::os::fd::RawFd {
    use nix::sys::socket;
    use std::os::fd::{AsRawFd, IntoRawFd};

    let fd = match socket::socket(
        socket::AddressFamily::Unix,
        socket::SockType::SeqPacket,
        socket::SockFlag::empty(),
        None,
    ) {
        Ok(fd) => fd,
        Err(_) => return -1,
    };
    // Close-on-exec (portable; SOCK_CLOEXEC isn't available everywhere).
    let raw = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
    if flags >= 0 {
        unsafe { libc::fcntl(raw, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    }

    #[cfg(target_os = "linux")]
    let addr = if unix_socket_path.starts_with(['.', '/']) {
        socket::UnixAddr::new(unix_socket_path)
    } else {
        socket::UnixAddr::new_abstract(unix_socket_path.as_bytes())
    };
    #[cfg(not(target_os = "linux"))]
    let addr = socket::UnixAddr::new(unix_socket_path);
    let addr = match addr {
        Ok(a) => a,
        Err(_) => return -1,
    };

    if socket::connect(fd.as_raw_fd(), &addr).is_err() {
        return -1;
    }
    if socket::send(
        fd.as_raw_fd(),
        crashtracker_receiver_request_bytes(),
        socket::MsgFlags::empty(),
    )
    .is_err()
    {
        return -1;
    }
    fd.into_raw_fd()
}

#[cfg(unix)]
mod adapter {
    use super::*;
    use datadog_ipc::platform::sockets::max_message_size;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, ReadBuf};

    /// `AsyncRead` adapter that concatenates an ordered stream of SEQPACKET datagrams into a
    /// contiguous byte stream. Borrows the handler-owned connection; reads are serial (the
    /// `enter_crashtracker_receiver` handler runs it to completion), so there is no concurrent
    /// reader. A zero-length datagram is EOF; a datagram larger than the caller's buffer is
    /// buffered and drained on later reads, so boundaries never truncate data.
    pub struct SeqpacketStreamReader<'a> {
        conn: &'a AsyncConn,
        /// Leftover bytes from a datagram that did not fit in the caller's buffer.
        leftover: Vec<u8>,
        leftover_pos: usize,
        eof: bool,
    }

    impl<'a> SeqpacketStreamReader<'a> {
        pub fn new(conn: &'a AsyncConn) -> Self {
            Self {
                conn,
                leftover: Vec::new(),
                leftover_pos: 0,
                eof: false,
            }
        }
    }

    impl AsyncRead for SeqpacketStreamReader<'_> {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            out: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();

            // Drain any buffered leftover first.
            if this.leftover_pos < this.leftover.len() {
                let remaining = &this.leftover[this.leftover_pos..];
                let n = remaining.len().min(out.remaining());
                out.put_slice(&remaining[..n]);
                this.leftover_pos += n;
                if this.leftover_pos >= this.leftover.len() {
                    this.leftover.clear();
                    this.leftover_pos = 0;
                }
                return Poll::Ready(Ok(()));
            }

            if this.eof {
                return Poll::Ready(Ok(()));
            }

            loop {
                let mut guard = match this.conn.poll_read_ready(cx) {
                    Poll::Ready(Ok(g)) => g,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                };

                // One recv() returns one datagram on SEQPACKET; a max-sized buffer avoids
                // truncation. The report is plain bytes (no fds), so no SCM_RIGHTS handling.
                let read_result = guard.try_io(|inner| {
                    let mut tmp = vec![0u8; max_message_size()];
                    let n = unsafe {
                        libc::recv(
                            inner.as_raw_fd(),
                            tmp.as_mut_ptr() as *mut libc::c_void,
                            tmp.len(),
                            libc::MSG_DONTWAIT,
                        )
                    };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        tmp.truncate(n as usize);
                        Ok(tmp)
                    }
                });

                match read_result {
                    Ok(Ok(payload)) => {
                        if payload.is_empty() {
                            this.eof = true;
                            return Poll::Ready(Ok(()));
                        }
                        let n = payload.len().min(out.remaining());
                        out.put_slice(&payload[..n]);
                        if n < payload.len() {
                            this.leftover = payload;
                            this.leftover_pos = n;
                        }
                        return Poll::Ready(Ok(()));
                    }
                    Ok(Err(e)) => {
                        // Peer close = end-of-stream, not an error. A clean Linux SEQPACKET close
                        // is a 0-byte recv (above); macOS SOCK_DGRAM
                        // surfaces ECONNRESET, and some paths report
                        // BrokenPipe/UnexpectedEof.
                        if matches!(
                            e.kind(),
                            io::ErrorKind::BrokenPipe
                                | io::ErrorKind::UnexpectedEof
                                | io::ErrorKind::ConnectionReset
                        ) {
                            this.eof = true;
                            return Poll::Ready(Ok(()));
                        }
                        return Poll::Ready(Err(e));
                    }
                    Err(_would_block) => continue,
                }
            }
        }
    }
}

#[cfg(unix)]
pub use adapter::SeqpacketStreamReader;

/// Drive the crashtracker receiver over a connection whose `enter_crashtracker_receiver` request
/// the serve loop already consumed: wrap the shared connection in a [`SeqpacketStreamReader`] and
/// feed the reconstructed byte stream (the streamed crash report) to the receiver.
#[cfg(unix)]
pub async fn run_crashtracker_receiver(conn: &AsyncConn) {
    use tokio::io::BufReader;

    let reader = BufReader::new(SeqpacketStreamReader::new(conn));
    if let Err(e) = libdd_crashtracker::async_receiver_entry_point_stream(reader).await {
        tracing::warn!("Got error while receiving crash report over IPC: {e}");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::{FromRawFd, OwnedFd};
    use tokio::io::unix::AsyncFd;
    use tokio::io::{AsyncReadExt, BufReader};

    /// A connected AF_UNIX `SOCK_DGRAM` socketpair: like the production `SOCK_SEQPACKET` it
    /// preserves message boundaries, exercising the adapter identically while also working on macOS
    /// (where `SeqpacketConn::socketpair` is unavailable).
    fn dgram_pair() -> (OwnedFd, OwnedFd) {
        let mut fds = [0i32; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) };
        assert_eq!(
            rc,
            0,
            "socketpair failed: {}",
            std::io::Error::last_os_error()
        );
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    /// Send each slice as its own datagram on `fd`.
    fn send_datagrams(fd: &OwnedFd, chunks: &[&[u8]]) {
        use std::os::fd::AsRawFd;
        for chunk in chunks {
            let n = unsafe {
                libc::send(
                    fd.as_raw_fd(),
                    chunk.as_ptr() as *const libc::c_void,
                    chunk.len(),
                    0,
                )
            };
            assert_eq!(
                n,
                chunk.len() as isize,
                "send: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    /// The adapter concatenates ordered datagrams back into the original stream — including one
    /// larger than the read buffer (the leftover path) — and reports EOF on peer close.
    #[tokio::test]
    async fn test_seqpacket_stream_reader_multichunk() {
        let (a, b) = dgram_pair();

        let big = "X".repeat(2000);
        let chunks: Vec<Vec<u8>> = vec![
            b"DD_CRASHTRACK_BEGIN_CONFIG\n".to_vec(),
            b"{\"some\":\"config\"}\n".to_vec(),
            b"DD_CRASHTRACK_END_CONFIG\n".to_vec(),
            format!("{big}\n").into_bytes(),
            b"DD_CRASHTRACK_DONE\n".to_vec(),
        ];

        let writer = tokio::task::spawn_blocking(move || {
            let refs: Vec<&[u8]> = chunks.iter().map(|c| c.as_slice()).collect();
            send_datagrams(&a, &refs);
            // Drop `a` to signal EOF to the reader.
            drop(a);
        });

        let async_fd = AsyncFd::new(b).expect("AsyncFd");
        // Small buffer to force the adapter's leftover path on the big chunk.
        let mut reader = BufReader::with_capacity(64, SeqpacketStreamReader::new(&async_fd));
        let mut got = Vec::new();
        reader.read_to_end(&mut got).await.expect("read_to_end");
        writer.await.expect("writer task");

        let expected = format!(
            "DD_CRASHTRACK_BEGIN_CONFIG\n{{\"some\":\"config\"}}\nDD_CRASHTRACK_END_CONFIG\n{big}\nDD_CRASHTRACK_DONE\n"
        );
        assert_eq!(String::from_utf8(got).unwrap(), expected);
    }

    /// The collector's first-message bytes must decode, via the normal IPC codec, back to the
    /// `EnterCrashtrackerReceiver` request — i.e. a real protocol message, not a magic marker.
    #[test]
    fn receiver_request_bytes_decode_to_variant() {
        use crate::service::SidecarInterfaceRequest;
        let bytes = crashtracker_receiver_request_bytes();
        let decoded = datadog_ipc::codec::decode::<SidecarInterfaceRequest>(bytes)
            .expect("request bytes must decode as a SidecarInterfaceRequest");
        assert!(matches!(
            decoded,
            SidecarInterfaceRequest::EnterCrashtrackerReceiver {}
        ));
    }
}
