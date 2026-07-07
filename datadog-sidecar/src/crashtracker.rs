// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Crashtracker integration over the single sidecar IPC socket.
//!
//! Instead of a second dedicated listener, the crash-time collector connects to the sidecar IPC
//! socket and sends an
//! [`enter_crashtracker_receiver`](crate::service::SidecarInterface::enter_crashtracker_receiver)
//! request as its first message — a normal codec-encoded message (see
//! [`crashtracker_receiver_request_bytes`]), decoded by the regular serve loop with no marker or
//! peeking — then streams the crash report over the same connection.
//!
//! The receiver consumes an `AsyncBufRead` byte stream; since the socket preserves message
//! boundaries, [`SeqpacketStreamReader`] concatenates the collector's datagrams back into a
//! contiguous stream (a zero-length datagram is EOF).

use datadog_ipc::AsyncConn;

/// Returns the abstract socket name the Linux listener binds.
#[cfg(target_os = "linux")]
pub fn crashtracker_ipc_socket_path(
    master_pid: u32,
    ipc_mode: crate::config::IpcMode,
) -> std::path::PathBuf {
    let liaison = if master_pid != 0 {
        crate::setup::DefaultLiason::ipc_for_pid(master_pid)
    } else {
        crate::setup::liaison_for_ipc_mode(ipc_mode)
    };
    liaison.path().to_path_buf()
}

/// The codec-encoded `enter_crashtracker_receiver` request the collector sends as its first
/// message. Fixed bytes, safe to send from a crash handler. This function must however be called
/// once before the crash handler runs, to initialize.
pub fn crashtracker_receiver_request_bytes() -> &'static [u8] {
    use crate::service::sidecar_interface::SidecarInterfaceRequest;
    static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BYTES.get_or_init(|| {
        datadog_ipc::codec::encode(&SidecarInterfaceRequest::EnterCrashtrackerReceiver {})
    })
}

/// `unix_socket_connector` for the crashtracker config: connect a `SOCK_SEQPACKET` socket to the
/// sidecar IPC socket at `unix_socket_path` and send the encoded `enter_crashtracker_receiver`
/// request as the first message, returning the connected fd (`-1` on error). Runs in the crash
/// handler with no allocation, so [`crashtracker_receiver_request_bytes`] must be primed earlier.
/// Linux-only (fresh `SOCK_SEQPACKET` connect); macOS reuses the existing sidecar fd instead.
#[cfg(target_os = "linux")]
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

    // A name starting with `.`/`/` is a filesystem path; anything else is an abstract name (the
    // listener binds abstract names by default).
    let addr = if unix_socket_path.starts_with(['.', '/']) {
        socket::UnixAddr::new(unix_socket_path)
    } else {
        socket::UnixAddr::new_abstract(unix_socket_path.as_bytes())
    };
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
        /// Scratch buffer for one `recv()`, avoiding reallocation.
        recv_buf: Vec<u8>,
        buf_pos: usize,
        eof: bool,
    }

    impl<'a> SeqpacketStreamReader<'a> {
        pub fn new(conn: &'a AsyncConn) -> Self {
            Self {
                conn,
                recv_buf: Vec::with_capacity(max_message_size()),
                buf_pos: 0,
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
            if this.buf_pos < this.recv_buf.len() {
                let remaining = &this.recv_buf[this.buf_pos..];
                let n = remaining.len().min(out.remaining());
                out.put_slice(&remaining[..n]);
                this.buf_pos += n;
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

                let recv_buf = &mut this.recv_buf;
                let read_result = guard.try_io(|inner| {
                    let n = unsafe {
                        libc::recv(
                            inner.as_raw_fd(),
                            recv_buf.as_mut_ptr() as *mut libc::c_void,
                            recv_buf.capacity(),
                            0,
                        )
                    };
                    if n < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(n as usize)
                    }
                });

                match read_result {
                    Ok(Ok(n)) => {
                        if n == 0 {
                            this.eof = true;
                            return Poll::Ready(Ok(()));
                        }
                        unsafe {
                            this.recv_buf.set_len(n);
                        }
                        let payload = this.recv_buf.as_slice();
                        let copied = payload.len().min(out.remaining());
                        out.put_slice(&payload[..copied]);
                        this.buf_pos = copied;
                        return Poll::Ready(Ok(()));
                    }
                    Ok(Err(e)) => {
                        // Treat peer close as EOF, not error.
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

pub use adapter::SeqpacketStreamReader;

/// Wrap `AsyncConn` and dispatch it to crashtracking receiver.
pub async fn run_crashtracker_receiver(conn: &AsyncConn) {
    use std::os::fd::AsRawFd;
    use tokio::io::BufReader;

    let reader = BufReader::new(SeqpacketStreamReader::new(conn));
    if let Err(e) = libdd_crashtracker::async_receiver_entry_point_stream(reader).await {
        tracing::warn!("Got error while receiving crash report over IPC: {e}");
    }

    // The connection ends with a crash. We don't go back to receive more data.
    unsafe { libc::shutdown(conn.as_raw_fd(), libc::SHUT_RD) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{FromRawFd, OwnedFd};
    use tokio::io::unix::AsyncFd;
    use tokio::io::{AsyncReadExt, BufReader};

    fn dgram_pair() -> (OwnedFd, OwnedFd) {
        use std::os::fd::AsRawFd;

        #[cfg(target_os = "linux")]
        let sock_type = libc::SOCK_SEQPACKET;
        #[cfg(not(target_os = "linux"))]
        let sock_type = libc::SOCK_DGRAM;

        let mut fds = [0i32; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, sock_type, 0, fds.as_mut_ptr()) };
        assert_eq!(
            rc,
            0,
            "socketpair failed: {}",
            std::io::Error::last_os_error()
        );
        let (a, b) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        for fd in [a.as_raw_fd(), b.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            assert!(
                flags >= 0,
                "fcntl(F_GETFL): {}",
                std::io::Error::last_os_error()
            );
            let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
            assert_eq!(rc, 0, "fcntl(F_SETFL): {}", std::io::Error::last_os_error());
        }
        (a, b)
    }

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
    #[cfg_attr(miri, ignore)]
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

    /// After the receiver consumes a full report, the collector never sends anything else on
    /// this connection — it only polls for the sidecar to hang up. `run_crashtracker_receiver`
    /// must therefore leave the connection in a state where the serve loop's *next* `recv`
    /// fails right away (its existing "connection closed" path already breaks the loop), instead
    /// of blocking forever in `readable()` for a request that will never arrive.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_run_crashtracker_receiver_unblocks_next_recv() {
        use datadog_ipc::recv_raw_async;

        let (collector, receiver) = dgram_pair();
        // Mirror the collector: send the report and never send/close anything else.
        send_datagrams(&collector, &[b"DD_CRASHTRACK_DONE\n"]);

        let async_fd = AsyncFd::new(receiver).expect("AsyncFd");
        run_crashtracker_receiver(&async_fd).await;

        // Simulates the serve loop's next iteration: without the fix this hangs forever, so
        // bound it with a timeout that would fail the test rather than hang the suite.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            recv_raw_async::<_, ()>(&async_fd, |_| ()),
        )
        .await
        .expect("serve loop's next recv did not return promptly after the receiver finished");
        assert!(
            result.is_err(),
            "expected the next recv to observe the connection as closed"
        );
    }

    /// The collector's first-message bytes must decode, via the normal IPC codec, back to the
    /// `EnterCrashtrackerReceiver` request — i.e. a real protocol message, not a magic marker.
    #[test]
    fn receiver_request_bytes_decode_to_variant() {
        use crate::service::sidecar_interface::SidecarInterfaceRequest;
        let bytes = crashtracker_receiver_request_bytes();
        let decoded = datadog_ipc::codec::decode::<SidecarInterfaceRequest>(bytes)
            .expect("request bytes must decode as a SidecarInterfaceRequest");
        assert!(matches!(
            decoded,
            SidecarInterfaceRequest::EnterCrashtrackerReceiver {}
        ));
    }
}
