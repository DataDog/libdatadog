// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Fault-safe process-memory copying through a Unix pipe.
//!
//! [`CopyPipe`] asks the kernel to copy the source range into a pipe and then
//! drains those bytes into owned memory. On the supported Unix targets, an
//! inaccessible source range is reported as `EFAULT`, allowing the copy to
//! return [`io::ErrorKind::WouldBlock`] without dereferencing the source pointer
//! in Rust.

use core::ffi::c_void;
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use super::super::retry_on_eintr;
use super::ProcessMemoryCopy;

/// A cached pipe used to probe-copy process memory through the kernel.
///
/// The pipe is empty between calls to [`ProcessMemoryCopy::copy`].
pub(super) struct CopyPipe {
    read_fd: OwnedFd,
    write_fd: OwnedFd,
    chunk_size: usize,
}

impl ProcessMemoryCopy for CopyPipe {
    fn new() -> io::Result<Self> {
        create_pipe()
    }

    fn copy(self, addr: *const u8, len: usize) -> (io::Result<Vec<u8>>, Option<Self>) {
        let mut bytes = vec![0; len];
        let mut offset = 0;

        while offset < len {
            let chunk_len = (len - offset).min(self.chunk_size);
            let chunk_addr = addr.wrapping_add(offset);

            // SAFETY: write asks the kernel to copy from chunk_addr. Invalid user memory is
            // reported as EFAULT or a short write without being dereferenced by Rust.
            let written = match retry_on_eintr(|| {
                let result = unsafe {
                    libc::write(
                        self.write_fd.as_raw_fd(),
                        chunk_addr.cast::<c_void>(),
                        chunk_len,
                    )
                };
                if result < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(result as usize)
                }
            }) {
                Ok(0) => {
                    return (
                        Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "zero-length write while copying process context memory",
                        )),
                        Some(self),
                    );
                }
                Ok(written) => written,
                Err(err) => {
                    match err.raw_os_error() {
                        Some(errno) if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK => {
                            return (
                                Err(io::Error::other(
                                    "process context copy pipe blocked despite an empty pipe",
                                )),
                                None,
                            );
                        }
                        Some(libc::EFAULT) => {
                            return (
                                Err(io::Error::new(
                                    io::ErrorKind::WouldBlock,
                                    "process context memory was unmapped during read",
                                )),
                                // If EFAULT is returned, nothing was written; a partial copy would
                                // instead be reported as a short write.
                                Some(self),
                            );
                        }
                        _ => {
                            return (
                                Err(io::Error::new(
                                    err.kind(),
                                    format!("failed to copy process context memory: {err}"),
                                )),
                                Some(self),
                            );
                        }
                    }
                }
            };

            let mut drained = 0;
            while drained < written {
                // Bounds proof:
                // { offset < len
                //   && 0 < written <= chunk_len <= len - offset
                //   && 0 <= drained < written
                //   && bytes.len() == len }
                // offset + drained < offset + written <= len == bytes.len()
                // { offset + drained..offset + written is a valid, nonempty slice range }
                let destination = &mut bytes[offset + drained..offset + written];
                // SAFETY: destination is an exclusively borrowed byte slice, so its pointer is
                // valid and writable for destination.len() bytes. read_fd owns a live pipe
                // descriptor.
                match retry_on_eintr(|| {
                    let result = unsafe {
                        libc::read(
                            self.read_fd.as_raw_fd(),
                            destination.as_mut_ptr().cast::<c_void>(),
                            destination.len(),
                        )
                    };
                    if result < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(result as usize)
                    }
                }) {
                    Ok(0) => {
                        return (
                            Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "process context copy pipe reported EOF",
                            )),
                            None,
                        );
                    }
                    Ok(read) => drained += read,
                    Err(err) => {
                        return (
                            Err(io::Error::new(
                                err.kind(),
                                format!("failed to drain process context copy pipe: {err}"),
                            )),
                            None,
                        );
                    }
                }
            }

            offset += written;
        }

        (Ok(bytes), Some(self))
    }
}

#[cfg(target_os = "linux")]
fn create_pipe() -> io::Result<CopyPipe> {
    let mut fds = [0; 2];
    // SAFETY: fds points to space for the two descriptors returned by pipe2.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(last_error("failed to create process context copy pipe"));
    }

    // SAFETY: pipe2 initialized both descriptors and ownership is transferred exactly once.
    let (read_fd, write_fd) =
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };

    // SAFETY: write_fd is a valid pipe descriptor.
    let capacity = unsafe { libc::fcntl(write_fd.as_raw_fd(), libc::F_GETPIPE_SZ) };
    if capacity < 0 {
        return Err(last_error(
            "failed to query process context copy pipe capacity",
        ));
    }
    if capacity == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process context copy pipe has zero capacity",
        ));
    }

    Ok(CopyPipe {
        read_fd,
        write_fd,
        chunk_size: capacity as usize,
    })
}

fn last_error(context: &'static str) -> io::Error {
    let err = io::Error::last_os_error();
    io::Error::new(err.kind(), format!("{context}: {err}"))
}

#[cfg(test)]
mod tests {
    use core::ptr;

    use super::{io, retry_on_eintr, CopyPipe, ProcessMemoryCopy};

    #[test]
    #[cfg_attr(miri, ignore)]
    fn copies_valid_memory_across_multiple_chunks() {
        let pipe = CopyPipe::new().expect("pipe creation should succeed");
        let len = pipe.chunk_size + 1;
        let source: Vec<_> = (0..len).map(|index| index as u8).collect();

        let (result, pipe) = pipe.copy(source.as_ptr(), source.len());
        let copied = result.expect("memory copy should succeed");

        assert_eq!(copied, source);
        assert!(pipe.is_some(), "pipe should remain reusable");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn rejects_a_range_crossing_into_inaccessible_memory() {
        // SAFETY: the arguments reserve two writable anonymous pages.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(page_size > 0, "page size query should succeed");
        let page_size = page_size as usize;
        let len = page_size * 2;
        let address = retry_on_eintr(|| {
            let address = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                )
            };
            if address == libc::MAP_FAILED {
                Err(io::Error::last_os_error())
            } else {
                Ok(address)
            }
        })
        .expect("memory mapping should succeed");
        // SAFETY: the second page is part of the mapping above.
        retry_on_eintr(|| {
            if unsafe {
                libc::mprotect(
                    address.cast::<u8>().add(page_size).cast(),
                    page_size,
                    libc::PROT_NONE,
                )
            } == 0
            {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
        .expect("memory protection should succeed");

        let (result, pipe) = CopyPipe::new()
            .expect("pipe creation should succeed")
            // SAFETY: the last byte of the first page is inside the live mapping.
            .copy(unsafe { address.cast::<u8>().add(page_size - 1) }, 2);
        let err = result.expect_err("a copy crossing into inaccessible memory should fail");

        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert!(pipe.is_some(), "pipe should remain reusable");
        // SAFETY: address and len came from mmap above.
        retry_on_eintr(|| {
            if unsafe { libc::munmap(address, len) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
        .expect("memory unmapping should succeed");
    }
}
