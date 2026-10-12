// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Generic IPC client connection state shared by all generated channel types.

use crate::platform::{HANDLE_SUFFIX_SIZE, SeqpacketConn, max_message_size};

#[cfg(unix)]
use std::os::unix::io::{OwnedFd, RawFd};
#[cfg(windows)]
use std::os::windows::io::{OwnedHandle as OwnedFd, RawHandle as RawFd};

use std::io;
use std::time::Duration;
use tracing::{trace, warn};

/// Client-side state for a single IPC connection.
///
/// Tracks in-flight message counts for ack-based flow control.
/// `SeqpacketConn` is non-blocking; blocking behavior is implemented via
/// `libc::poll` internally.
pub struct IpcClientConn {
    pub conn: SeqpacketConn,
    /// Number of messages sent (incremented on each successful send).
    send_count: u64,
    /// Number of server replies received (acks or typed responses).
    ack_count: u64,
    /// Reusable receive buffer.  Sized to hold a maximum payload plus the platform wire overhead
    /// (`HANDLE_SUFFIX_SIZE`), so that messages can be read directly without an intermediate copy.
    recv_buf: Vec<u8>,
    /// Set to true when a fatal I/O error occurs on send or receive.
    closed: bool,
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn recoverable_packet_rejection_preserves_accounting_and_legacy_policy() {
        for recoverable in [false, true] {
            let (conn, peer) = SeqpacketConn::socketpair().unwrap();
            let bytes: libc::c_int = 4096;
            // SAFETY: valid socket and pointer/length for a live integer.
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        conn.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_SNDBUF,
                        (&bytes as *const libc::c_int).cast(),
                        std::mem::size_of_val(&bytes).try_into().unwrap(),
                    )
                },
                0
            );
            let mut client = IpcClientConn::new(conn);
            if recoverable {
                let err = client
                    .try_send_with_rejection(vec![0; 16 * 1024], &[])
                    .unwrap_err();
                assert_eq!(err.raw_os_error(), Some(libc::EMSGSIZE));
            } else {
                assert!(!client.try_send(vec![0; 16 * 1024], &[]));
            }
            assert_eq!(client.outstanding(), 0);
            assert_eq!(client.is_closed(), !recoverable);
            if recoverable {
                client.try_send_with_rejection(vec![7], &[]).unwrap();
                assert_eq!(client.outstanding(), 1);
                let mut buf = [0; 1];
                assert_eq!(peer.try_recv_raw(&mut buf).unwrap().0, 1);
                assert_eq!(buf, [7]);
                peer.try_send_raw(vec![0], &[]).unwrap();
                client.drain_acks();
                assert_eq!(client.outstanding(), 0);
                assert!(!client.is_closed());
            }
        }
    }
}

impl IpcClientConn {
    pub fn new(conn: SeqpacketConn) -> Self {
        Self {
            conn,
            send_count: 0,
            ack_count: 0,
            recv_buf: vec![0u8; max_message_size() + HANDLE_SUFFIX_SIZE],
            closed: false,
        }
    }

    pub fn set_read_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.conn.set_read_timeout(d)
    }

    pub fn set_write_timeout(&mut self, d: Option<Duration>) -> io::Result<()> {
        self.conn.set_write_timeout(d)
    }

    /// Returns `true` if a fatal I/O error has occurred on this connection.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Number of sent-but-not-yet-acked messages on client side.
    pub fn outstanding(&self) -> u64 {
        self.send_count - self.ack_count
    }

    /// Non-blocking drain of pending acks up to the number of outstanding messages.
    ///
    /// Passing `outstanding()` as the bound avoids initialising more kernel structures than
    /// needed and skips the final `WouldBlock` probe when all expected acks have arrived.
    /// On Linux uses `recvmmsg` to batch-receive up to 100 acks per syscall.
    pub fn drain_acks(&mut self) {
        let max = self.outstanding() as usize;
        match self.conn.drain_acks_nonblocking(max) {
            Ok(count) => self.ack_count += count as u64,
            Err(e) => {
                warn!("drain_acks: connection error ({}), marking closed", e);
                self.closed = true;
            }
        }
    }

    /// Attempt a non-blocking send.
    ///
    /// Returns `false` if the socket would block (EAGAIN). `data` is consumed
    /// regardless of whether the transport accepts it.
    pub fn try_send(&mut self, data: Vec<u8>, fds: &[RawFd]) -> bool {
        match self.conn.try_send_raw(data, fds) {
            Ok(()) => {
                self.send_count += 1;
                true
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => false,
            Err(_) => {
                // Fatal error (e.g. EPIPE): mark connection as closed.
                self.closed = true;
                false
            }
        }
    }

    /// Attempt a non-blocking send with recoverable packet rejection.
    ///
    /// Like [`Self::try_send`], but returns the I/O error and leaves the connection
    /// open on Unix `EMSGSIZE`: the packet was rejected, not the connection.
    /// This lets callers drop oversized observations without reconnecting.
    /// `WouldBlock` is also nonfatal. Only accepted sends advance ACK accounting;
    /// `data` is consumed even when rejected. Existing `try_send` policy is unchanged.
    pub fn try_send_with_rejection(&mut self, data: Vec<u8>, fds: &[RawFd]) -> io::Result<()> {
        match self.conn.try_send_raw(data, fds) {
            Ok(()) => {
                self.send_count += 1;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(e),
            #[cfg(unix)]
            Err(e) if e.raw_os_error() == Some(libc::EMSGSIZE) => Err(e),
            Err(e) => {
                self.closed = true;
                Err(e)
            }
        }
    }

    /// Blocking send (no response wait).
    ///
    /// Used when draining the outbox of state-change messages. Takes ownership
    /// of `data` so a pending Windows write can retain its allocation.
    pub fn send_blocking(&mut self, data: Vec<u8>, fds: &[RawFd]) -> io::Result<()> {
        self.conn.send_raw_blocking(data, fds).inspect_err(|_| {
            self.closed = true;
        })?;
        self.send_count += 1;
        Ok(())
    }

    /// Blocking send + blocking receive of response.
    ///
    /// Drains any pending fire-and-forget acks (non-blocking, batched on Linux via `recvmmsg`)
    /// before sending, so the subsequent blocking recv loop only needs to wait for the single
    /// response ack.  Sends `data`/`fds` (blocking), then receives in a loop until the ack
    /// for this specific send arrives.  Returns the response bytes and any transferred file
    /// descriptors. Takes ownership of `data` so a pending Windows write can retain
    /// its allocation.
    pub fn call(&mut self, data: Vec<u8>, fds: &[RawFd]) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
        self.drain_acks();
        if self.closed {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        }
        self.conn.send_raw_blocking(data, fds).inspect_err(|e| {
            warn!("call: send failed ({}), marking closed", e);
            self.closed = true;
        })?;
        self.send_count += 1;
        let target = self.send_count;
        trace!(
            "call: sent packet {}, waiting for ack {} (ack_count={})",
            self.send_count, target, self.ack_count
        );
        loop {
            let (n, resp_fds) = self
                .conn
                .recv_raw_blocking(&mut self.recv_buf)
                .inspect_err(|e| {
                    warn!("call: recv failed ({}), marking closed", e);
                    self.closed = true;
                })?;
            self.ack_count += 1;
            trace!("call: got ack {} (target={})", self.ack_count, target);
            if self.ack_count == target {
                return Ok((self.recv_buf[..n].to_vec(), resp_fds));
            }
            // Intermediate ack for a prior fire-and-forget message — continue.
        }
    }
}
