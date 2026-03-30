// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Generic IPC client connection state shared by all generated channel types.

use crate::platform::{max_message_size, SeqpacketConn, HANDLE_SUFFIX_SIZE};

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

    /// Non-blocking drain of all pending acks. Updates `ack_count`.
    ///
    /// On Linux uses `recvmmsg` to batch-receive up to 64 acks per syscall.
    pub fn drain_acks(&mut self) {
        match self.conn.drain_acks_nonblocking() {
            Ok(count) => self.ack_count += count as u64,
            Err(e) => {
                warn!("drain_acks: connection error ({}), marking closed", e);
                self.closed = true;
            }
        }
    }

    /// Attempt a non-blocking send.
    ///
    /// Returns `false` if the socket would block (EAGAIN).
    /// `data` is unmodified after the call.
    pub fn try_send(&mut self, data: &mut Vec<u8>, fds: &[RawFd]) -> bool {
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

    /// Blocking send (no response wait).
    ///
    /// Used when draining the outbox of state-change messages.
    pub fn send_blocking(&mut self, data: &mut Vec<u8>, fds: &[RawFd]) -> io::Result<()> {
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
    /// descriptors.
    pub fn call(
        &mut self,
        data: &mut Vec<u8>,
        fds: &[RawFd],
    ) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
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
            self.send_count,
            target,
            self.ack_count
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
