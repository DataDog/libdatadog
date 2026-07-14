// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::c_void;
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use super::{PipeCopyError, ProcessMemoryCopy};

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
        let (read_fd, write_fd, chunk_size) = create_pipe()?;

        Ok(Self {
            read_fd,
            write_fd,
            chunk_size,
        })
    }

    fn copy(&self, addr: *const u8, len: usize) -> Result<Vec<u8>, PipeCopyError> {
        let mut bytes: Vec<u8> = Vec::with_capacity(len);
        let mut offset = 0;

        while offset < len {
            let chunk_len = (len - offset).min(self.chunk_size);
            let chunk_addr = addr.wrapping_add(offset);

            // SAFETY: write asks the kernel to copy from chunk_addr. Invalid user memory is
            // reported as EFAULT or a short write without being dereferenced by Rust.
            let written = loop {
                let result = unsafe {
                    libc::write(
                        self.write_fd.as_raw_fd(),
                        chunk_addr.cast::<c_void>(),
                        chunk_len,
                    )
                };
                if result > 0 {
                    break result as usize;
                }
                if result == 0 {
                    return Err(PipeCopyError {
                        err: io::Error::new(
                            io::ErrorKind::WriteZero,
                            "zero-length write while copying process context memory",
                        ),
                        pipe_dirty: false,
                    });
                }

                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EINTR) => continue,
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
                            pipe_dirty: false,
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
            };

            let mut drained = 0;
            while drained < written {
                // SAFETY: bytes owns len bytes of spare capacity and
                // offset + written <= len, so the destination is writable.
                let result = unsafe {
                    libc::read(
                        self.read_fd.as_raw_fd(),
                        bytes.as_mut_ptr().add(offset + drained).cast::<c_void>(),
                        written - drained,
                    )
                };
                if result > 0 {
                    drained += result as usize;
                    continue;
                }
                if result == 0 {
                    return Err(PipeCopyError {
                        err: io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "process context copy pipe reported EOF",
                        ),
                        pipe_dirty: true,
                    });
                }

                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    _ => {
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

        // SAFETY: every byte was initialized by the pipe reads above.
        unsafe { bytes.set_len(len) };
        Ok(bytes)
    }
}

#[cfg(target_os = "linux")]
fn create_pipe() -> io::Result<(OwnedFd, OwnedFd, usize)> {
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

    Ok((read_fd, write_fd, capacity as usize))
}

fn last_error(context: &'static str) -> io::Error {
    let err = io::Error::last_os_error();
    io::Error::new(err.kind(), format!("{context}: {err}"))
}

#[cfg(test)]
mod tests {
    use core::ptr;

    use super::{io, CopyPipe, ProcessMemoryCopy};

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
        assert_ne!(address, libc::MAP_FAILED);
        // SAFETY: the second page is part of the mapping above.
        assert_eq!(
            unsafe {
                libc::mprotect(
                    address.cast::<u8>().add(page_size).cast(),
                    page_size,
                    libc::PROT_NONE,
                )
            },
            0
        );

        let err = CopyPipe::new()
            .expect("pipe creation should succeed")
            // SAFETY: the last byte of the first page is inside the live mapping.
            .copy(unsafe { address.cast::<u8>().add(page_size - 1) }, 2)
            .expect_err("a copy crossing into inaccessible memory should fail");

        assert_eq!(err.err.kind(), io::ErrorKind::WouldBlock);
        assert!(!err.pipe_dirty);
        // SAFETY: address and len came from mmap above.
        assert_eq!(unsafe { libc::munmap(address, len) }, 0);
    }
}
