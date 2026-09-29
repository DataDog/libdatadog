// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! One bounded packet exchange on an existing Linux socket, with a private completion pipe.
//!
//! Construction and destruction require ordinary thread context. The exchange also works in
//! raw clone workers with inherited TLS: it does not allocate, lock, access TLS, call libc, or
//! unwind. Audit its generated call graph in debug and optimized builds when changing it.

use crate::SeqpacketConn;
use std::cell::Cell;
use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
mod syscalls;
use syscalls::raw_syscall6;

/// A pre-encoded packet sent on an existing connection, with a private completion pipe.
/// The duplicated send descriptor keeps the exact connection valid until normal-context drop.
/// It shares packet ordering with the client, but never consumes the client's replies.
pub struct PreparedRequest {
    send_fd: i32,
    receive_fd: i32,
    message: libc::msghdr,
    request_len: usize,
    _sender: SeqpacketConn,
    _receiver: std::io::PipeReader,
    completion: Cell<Option<OwnedFd>>,
    _request: Box<[u8]>,
    _iov: Box<libc::iovec>,
    _control: Box<[usize; 4]>,
}

impl PreparedRequest {
    /// Prepare a packet transferring the pipe writer `completion` through SCM_RIGHTS. The server
    /// closes it when done and sends no reply on `sender`. `receiver` is the pipe's read end.
    /// Ordinary context only.
    pub fn new(
        sender: &SeqpacketConn,
        request: Box<[u8]>,
        receiver: std::io::PipeReader,
        completion: OwnedFd,
    ) -> std::io::Result<Self> {
        let sender = sender.try_clone()?;
        let mut iov = Box::new(libc::iovec {
            iov_base: request.as_ptr().cast_mut().cast(),
            iov_len: request.len(),
        });
        // Linux x86_64/AArch64 cmsghdr + one descriptor, padded to native word alignment.
        // Unsupported architectures never execute the raw exchange.
        let mut control = Box::new([0usize; 4]);
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut *iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        // musl uses u32 for these lengths; glibc uses usize
        message.msg_controllen =
            unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as _;
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as _;
            libc::CMSG_DATA(header)
                .cast::<i32>()
                .write(completion.as_raw_fd());
        }
        Ok(Self {
            send_fd: sender.as_raw_fd(),
            receive_fd: receiver.as_raw_fd(),
            message,
            request_len: request.len(),
            _sender: sender,
            _receiver: receiver,
            completion: Cell::new(Some(completion)),
            _request: request,
            _iov: iov,
            _control: control,
        })
    }

    /// Send the prepared packet and wait for pipe EOF, with one ten-second deadline.
    /// Returns zero when the server releases the writer, or a negative Linux errno.
    /// EOF also occurs if the server exits; it does not distinguish completion from server death.
    ///
    /// # Safety
    /// Guarantee exclusive one-shot use and keep this object alive until return. Block all
    /// worker signals and do not use an inherited object after fork. The original client may
    /// continue sending and receiving concurrently.
    #[inline(always)]
    pub unsafe fn exchange(&self) -> i32 {
        exchange(self)
    }
}

// Direct syscalls use the 64-bit Linux kernel ABI, not libc's platform types.
#[repr(C)]
struct KernelTimespec {
    seconds: i64,
    nanoseconds: i64,
}

// Match the pollfd layout consumed directly by the kernel.
#[repr(C)]
struct KernelPollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[inline(never)]
unsafe fn exchange(channel: &PreparedRequest) -> i32 {
    // Raw Linux syscalls return `-errno` directly. They do not set libc's thread-local `errno`.
    const EINTR: isize = 4;
    const EAGAIN: isize = 11;
    const CLOCK_MONOTONIC: usize = 1;
    const MSG_DONTWAIT: usize = 0x40;
    const MSG_NOSIGNAL: usize = 0x4000;
    const POLLOUT: i16 = 4;
    const POLLHUP: i16 = 16;

    let send_fd = channel.send_fd;
    let receive_fd = channel.receive_fd;
    let request_len = channel.request_len;

    let mut now = KernelTimespec {
        seconds: 0,
        nanoseconds: 0,
    };
    let result = raw_syscall6(
        libc::SYS_clock_gettime as usize,
        CLOCK_MONOTONIC,
        (&raw mut now).cast::<u8>() as usize,
        0,
        0,
        0,
        0,
    );
    if result < 0 {
        return result as i32;
    }
    // Use one absolute deadline for send, backpressure, and completion. Restarting a relative
    // timeout after EINTR or EAGAIN could otherwise keep process termination alive
    // indefinitely. Wrapping arithmetic is intentional: debug overflow checks would introduce
    // panic paths.
    let deadline_seconds = now.seconds.wrapping_add(10);
    let deadline_nanoseconds = now.nanoseconds;
    // Poll when send hits backpressure, and after sending to await pipe closure.
    let mut sent = false;
    let mut poll = false;

    loop {
        let result = raw_syscall6(
            libc::SYS_clock_gettime as usize,
            CLOCK_MONOTONIC,
            (&raw mut now).cast::<u8>() as usize,
            0,
            0,
            0,
            0,
        );
        if result < 0 {
            return result as i32;
        }
        let mut remaining_seconds = deadline_seconds.wrapping_sub(now.seconds);
        let mut remaining_nanoseconds = deadline_nanoseconds.wrapping_sub(now.nanoseconds);
        if remaining_nanoseconds < 0 {
            remaining_nanoseconds = remaining_nanoseconds.wrapping_add(1_000_000_000);
            remaining_seconds = remaining_seconds.wrapping_sub(1);
        }
        if remaining_seconds < 0 || (remaining_seconds == 0 && remaining_nanoseconds == 0) {
            return -libc::ETIMEDOUT;
        }
        let mut remaining = KernelTimespec {
            seconds: remaining_seconds,
            nanoseconds: remaining_nanoseconds,
        };

        if poll {
            let mut pollfd = KernelPollFd {
                fd: if sent { receive_fd } else { send_fd },
                // POLLHUP is reported even with no requested events. No pipe read is needed.
                events: if sent { 0 } else { POLLOUT },
                revents: 0,
            };
            let result = raw_syscall6(
                libc::SYS_ppoll as usize,
                (&raw mut pollfd).cast::<u8>() as usize,
                1,
                (&raw mut remaining).cast::<u8>() as usize,
                0,
                // The kernel's 64-bit signal-set size is eight bytes. No mask is supplied, but
                // passing the ABI size keeps this a valid direct ppoll syscall on both targets.
                8,
                0,
            );
            if result > 0 {
                if sent {
                    return if pollfd.revents & POLLHUP != 0 {
                        0
                    } else {
                        -libc::EIO
                    };
                }
                poll = false;
                continue;
            }
            if result == -EINTR {
                continue;
            }
            if result == 0 {
                return -libc::ETIMEDOUT;
            }
            return result as i32;
        }

        let result = raw_syscall6(
            libc::SYS_sendmsg as usize,
            send_fd as usize,
            (&raw const channel.message) as usize,
            // Nonblocking I/O lets the single ppoll deadline bound backpressure. MSG_NOSIGNAL
            // prevents a closed sidecar socket from delivering SIGPIPE to the raw worker.
            MSG_DONTWAIT | MSG_NOSIGNAL,
            0,
            0,
            0,
        );
        // SOCK_SEQPACKET preserves message boundaries: a positive short send is a protocol
        // failure rather than progress that can be resumed with a pointer offset.
        if result == request_len as isize {
            // The packet now owns the transferred writer. Release our copy so the server's
            // close produces EOF. Taking ownership also prevents a second close during drop.
            if let Some(completion) = channel.completion.replace(None) {
                raw_syscall6(
                    libc::SYS_close as usize,
                    completion.into_raw_fd() as usize,
                    0,
                    0,
                    0,
                    0,
                    0,
                );
            }
            sent = true;
            poll = true;
            continue;
        }

        if result == -EINTR {
            continue;
        }
        if result == -EAGAIN {
            // Retry the send after readiness, within the original deadline.
            poll = true;
            continue;
        }
        if result < 0 {
            return result as i32;
        }
        // Any other nonnegative result is an impossible short packet for this protocol.
        return -libc::EPROTO;
    }
}
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
unsafe fn exchange(_channel: &PreparedRequest) -> i32 {
    -libc::ENOTSUP
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{Duration, Instant};

    struct Peer {
        request: SeqpacketConn,
        completion: std::io::PipeWriter,
    }
    impl Peer {
        fn as_raw_fd(&self) -> i32 {
            self.request.as_raw_fd()
        }
    }

    fn pair() -> (PreparedRequest, Peer) {
        let (sender, request) = SeqpacketConn::socketpair().unwrap();
        let (receiver, completion) = std::io::pipe().unwrap();
        let handle = completion.try_clone().unwrap().into();
        let flush =
            PreparedRequest::new(&sender, vec![42].into_boxed_slice(), receiver, handle).unwrap();
        (
            flush,
            Peer {
                request,
                completion,
            },
        )
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native IPC sockets and inline assembly")]
    fn waits_for_pipe_close_even_if_data_is_readable() {
        let (flush, peer) = pair();
        let server = std::thread::spawn(move || {
            let mut pollfd = libc::pollfd {
                fd: peer.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(unsafe { libc::poll(&mut pollfd, 1, 1000) }, 1);
            let mut request = 0u8;
            assert_eq!(
                unsafe { libc::recv(peer.as_raw_fd(), (&mut request as *mut u8).cast(), 1, 0) },
                1
            );
            assert_eq!(request, 42);
            (&peer.completion).write_all(&[0]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            drop(peer);
        });
        let start = Instant::now();
        assert_eq!(unsafe { flush.exchange() }, 0);
        assert!(start.elapsed() >= Duration::from_millis(15));
        server.join().unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native IPC sockets and inline assembly")]
    fn retries_send_after_backpressure() {
        let (flush, peer) = pair();
        let filler = [0u8; 512];
        loop {
            let sent = unsafe {
                libc::send(
                    flush.send_fd,
                    filler.as_ptr().cast(),
                    filler.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
            if sent < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::EAGAIN)
                );
                break;
            }
        }
        let server = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let mut bytes = [0u8; 512];
            loop {
                let mut pollfd = libc::pollfd {
                    fd: peer.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                assert_eq!(unsafe { libc::poll(&mut pollfd, 1, 1000) }, 1);
                let received = unsafe {
                    libc::recv(peer.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), 0)
                };
                assert!(received > 0);
                if received == 1 && bytes[0] == 42 {
                    break;
                }
                assert_eq!(received, bytes.len() as isize);
            }
            drop(peer);
        });
        assert_eq!(unsafe { flush.exchange() }, 0);
        server.join().unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native IPC sockets and inline assembly")]
    fn rejects_a_closed_peer_without_sigpipe() {
        let (flush, peer) = pair();
        drop(peer);
        assert!(unsafe { flush.exchange() } < 0);
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native IPC sockets and inline assembly")]
    fn open_pipe_has_one_ten_second_deadline() {
        let (flush, _peer) = pair();
        let start = Instant::now();
        assert_eq!(unsafe { flush.exchange() }, -libc::ETIMEDOUT);
        assert!(start.elapsed() >= Duration::from_secs(9));
        assert!(start.elapsed() < Duration::from_secs(15));
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native IPC sockets and inline assembly")]
    fn shares_packet_order_and_transfers_a_private_completion_pipe() {
        let (sender, server) = SeqpacketConn::socketpair().unwrap();
        let (receiver, completion) = std::io::pipe().unwrap();
        for fd in [
            sender.as_raw_fd(),
            receiver.as_raw_fd(),
            completion.as_raw_fd(),
        ] {
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, libc::FD_CLOEXEC);
        }
        let prepared = PreparedRequest::new(
            &sender,
            vec![42].into_boxed_slice(),
            receiver,
            completion.into(),
        )
        .unwrap();
        sender.try_send_raw(vec![11], &[]).unwrap();
        let server = std::thread::spawn(move || {
            let mut bytes = [0u8; 32];
            let (n, handles) = server.recv_raw_blocking(&mut bytes).unwrap();
            assert_eq!(&bytes[..n], &[11]);
            assert!(handles.is_empty());
            let (n, mut handles) = server.recv_raw_blocking(&mut bytes).unwrap();
            assert_eq!(&bytes[..n], &[42]);
            assert_eq!(handles.len(), 1);
            let completion = handles.pop().unwrap();
            // An unrelated normal reply must remain on the normal transport.
            server.try_send_raw(vec![23], &[]).unwrap();
            drop(completion);
        });
        assert_eq!(unsafe { prepared.exchange() }, 0);
        let mut reply = [0u8; 32];
        let (n, _) = sender.recv_raw_blocking(&mut reply).unwrap();
        assert_eq!(&reply[..n], &[23]);
        server.join().unwrap();
    }
}
