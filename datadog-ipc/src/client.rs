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

/// Maximum number of fire-and-forget messages that may be outstanding
/// (sent but not yet acked) before the client blocks or drops new messages.
pub const MAX_OUTSTANDING: u64 = 100;

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
    /// Maximum allowed `send_count - ack_count` before `try_send` returns false.
    pub max_outstanding: u64,
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
            max_outstanding: MAX_OUTSTANDING,
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

    /// Number of sent-but-not-yet-acked messages.
    pub fn outstanding(&self) -> u64 {
        self.send_count - self.ack_count
    }

    /// Non-blocking drain of all pending acks.  Updates `ack_count`.
    pub fn drain_acks(&mut self) {
        while self.conn.try_recv_raw(&mut self.recv_buf).is_ok() {
            self.ack_count += 1;
        }
    }

    /// Non-blocking send.
    ///
    /// First drains pending acks, then checks the outstanding limit.
    /// Returns `false` if the socket would block (EAGAIN) or the outstanding
    /// limit has been reached.  `data` is unmodified after the call.
    pub fn try_send(&mut self, data: &mut Vec<u8>, fds: &[RawFd]) -> bool {
        self.drain_acks();
        if self.outstanding() >= self.max_outstanding {
            return false;
        }
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
    /// Sends `data`/`fds` (blocking), then receives in a loop, skipping any
    /// intermediate 0-byte acks for prior fire-and-forget messages, until the
    /// ack for this specific send arrives.  Returns the response bytes and any
    /// transferred file descriptors.
    pub fn call(
        &mut self,
        data: &mut Vec<u8>,
        fds: &[RawFd],
    ) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
        self.conn.send_raw_blocking(data, fds).inspect_err(|_| {
            self.closed = true;
        })?;
        self.send_count += 1;
        let target = self.send_count;
        loop {
            let (n, resp_fds) = self
                .conn
                .recv_raw_blocking(&mut self.recv_buf)
                .inspect_err(|_| {
                    self.closed = true;
                })?;
            self.ack_count += 1;
            if self.ack_count == target {
                return Ok((self.recv_buf[..n].to_vec(), resp_fds));
            }
            // Intermediate ack for a prior fire-and-forget message — discard.
        }
    }
}
