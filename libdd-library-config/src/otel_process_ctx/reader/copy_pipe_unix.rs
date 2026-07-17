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
use super::{PipeCopyError, ProcessMemoryCopy};
use crate::otel_process_ctx::last_error;

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

    fn copy(&self, addr: *const u8, len: usize) -> Result<Vec<u8>, PipeCopyError> {
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
                    return Err(PipeCopyError {
                        err: io::Error::new(
                            io::ErrorKind::WriteZero,
                            "zero-length write while copying process context memory",
                        ),
                        pipe_dirty: false,
                    });
                }
                Ok(written) => written,
                Err(err) => {
                    match err.raw_os_error() {
                        Some(errno) if errno == libc::EAGAIN || errno == libc::EWOULDBLOCK => {
                            return Err(PipeCopyError {
                                err: io::Error::other(
                                    "process context copy pipe blocked despite an empty pipe",
                                ),
                                pipe_dirty: true,
                            });
                        }
                        Some(libc::EFAULT) => {
                            return Err(PipeCopyError {
                                err: io::Error::new(
                                    io::ErrorKind::WouldBlock,
                                    "process context memory was unmapped during read",
                                ),
                                // actually false on Linux: if EFAULT is returned, nothing was
                                // written; if anything had been written already, we would get a
                                // short write
                                // However, this is not the case for macOS, despite what its manual
                                // says: See https://github.com/apple-oss-distributions/xnu/blob/5c306bec31e314fa4d8bbdafb2f6f5a6b7e7b291/bsd/man/man2/write.2#L168-L186
                                pipe_dirty: true,
                            });
                        }
                        _ => {
                            return Err(PipeCopyError {
                                err: io::Error::new(
                                    err.kind(),
                                    format!("failed to copy process context memory: {err}"),
                                ),
                                pipe_dirty: false,
                            });
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
                        return Err(PipeCopyError {
                            err: io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "process context copy pipe reported EOF",
                            ),
                            pipe_dirty: true,
                        });
                    }
                    Ok(read) => drained += read,
                    Err(err) => {
                        return Err(PipeCopyError {
                            err: io::Error::new(
                                err.kind(),
                                format!("failed to drain process context copy pipe: {err}"),
                            ),
                            pipe_dirty: true,
                        });
                    }
                }
            }

            offset += written;
        }

        Ok(bytes)
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

#[cfg(target_os = "macos")]
fn create_pipe() -> io::Result<(OwnedFd, OwnedFd, usize)> {
    let mut fds = [0; 2];
    // SAFETY: fds points to space for the two descriptors returned by pipe.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(last_error("failed to create process context copy pipe"));
    }

    // SAFETY: pipe initialized both descriptors and ownership is transferred exactly once.
    let (read_fd, write_fd) =
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    configure_fd(&read_fd)?;
    configure_fd(&write_fd)?;

    // POSIX guarantees that an empty pipe accepts at least PIPE_BUF bytes without blocking.
    // SAFETY: write_fd is a valid pipe descriptor.
    let chunk_size = unsafe { libc::fpathconf(write_fd.as_raw_fd(), libc::_PC_PIPE_BUF) };
    if chunk_size <= 0 {
        return Err(last_error(
            "failed to query process context copy pipe capacity",
        ));
    }

    Ok((read_fd, write_fd, chunk_size as usize))
}

#[cfg(target_os = "macos")]
fn configure_fd(fd: &OwnedFd) -> io::Result<()> {
    // SAFETY: fd is a valid descriptor.
    let status = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if status < 0 {
        return Err(last_error(
            "failed to query process context copy pipe status flags",
        ));
    }
    // SAFETY: fd is valid and F_SETFL accepts the status flags returned above.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, status | libc::O_NONBLOCK) } < 0 {
        return Err(last_error(
            "failed to make process context copy pipe non-blocking",
        ));
    }

    // SAFETY: fd is a valid descriptor.
    let descriptor = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
    if descriptor < 0 {
        return Err(last_error(
            "failed to query process context copy pipe descriptor flags",
        ));
    }
    // SAFETY: fd is valid and F_SETFD accepts the descriptor flags returned above.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, descriptor | libc::FD_CLOEXEC) } < 0 {
        return Err(last_error(
            "failed to mark process context copy pipe close-on-exec",
        ));
    }
    Ok(())
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

        let copied = pipe
            .copy(source.as_ptr(), source.len())
            .expect("memory copy should succeed");

        assert_eq!(copied, source);
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

        let err = CopyPipe::new()
            .expect("pipe creation should succeed")
            // SAFETY: the last byte of the first page is inside the live mapping.
            .copy(unsafe { address.cast::<u8>().add(page_size - 1) }, 2)
            .expect_err("a copy crossing into inaccessible memory should fail");

        assert_eq!(err.err.kind(), io::ErrorKind::WouldBlock);
        assert!(err.pipe_dirty);
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
