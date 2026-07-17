// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{ffi::c_void, ptr};
use std::io;

use crate::otel_process_ctx::{
    last_error,
    reader::{PipeCopyError, ProcessMemoryCopy},
};

type Handle = *mut c_void;

const REQUESTED_PIPE_BUFFER_SIZE: u32 = 4096;
const ERROR_NOACCESS: i32 = 998;
const ERROR_INVALID_USER_BUFFER: i32 = 1784;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreatePipe(
        read_pipe: *mut Handle,
        write_pipe: *mut Handle,
        pipe_attributes: *mut c_void,
        size: u32,
    ) -> i32;
    fn ReadFile(
        file: Handle,
        buffer: *mut c_void,
        bytes_to_read: u32,
        bytes_read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn GetNamedPipeInfo(
        named_pipe: Handle,
        flags: *mut u32,
        out_buffer_size: *mut u32,
        in_buffer_size: *mut u32,
        max_instances: *mut u32,
    ) -> i32;
    fn WriteFile(
        file: Handle,
        buffer: *const c_void,
        bytes_to_write: u32,
        bytes_written: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CloseHandle(object: Handle) -> i32;
}

/// A cached anonymous pipe used to probe-copy process memory through the Windows kernel.
pub struct CopyPipe {
    read_handle: Handle,
    write_handle: Handle,
    chunk_size: u32,
}

// SAFETY: the owned handles may move between threads. CopyPipe is not Sync because copy(&self)
// mutates the pipe state, so concurrent calls could interleave.
unsafe impl Send for CopyPipe {}

impl ProcessMemoryCopy for CopyPipe {
    fn new() -> io::Result<Self> {
        let mut read_handle = ptr::null_mut();
        let mut write_handle = ptr::null_mut();
        // SAFETY: both handle pointers are valid out-parameters; security attributes are optional.
        let result = unsafe {
            CreatePipe(
                &mut read_handle,
                &mut write_handle,
                ptr::null_mut(),
                REQUESTED_PIPE_BUFFER_SIZE,
            )
        };
        if result == 0 {
            return Err(last_error("failed to create process context copy pipe"));
        }

        let mut pipe = Self {
            read_handle,
            write_handle,
            chunk_size: 0,
        };
        // SAFETY: read_handle is the readable end returned by CreatePipe. Bytes written through
        // write_handle are incoming data for this handle, so this reports the relevant capacity.
        let result = unsafe {
            GetNamedPipeInfo(
                pipe.read_handle,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut pipe.chunk_size,
                ptr::null_mut(),
            )
        };
        if result == 0 {
            return Err(last_error(
                "failed to query process context copy pipe capacity",
            ));
        }
        if pipe.chunk_size == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process context copy pipe reported zero capacity",
            ));
        }

        Ok(pipe)
    }

    fn copy(&self, addr: *const u8, len: usize) -> Result<Vec<u8>, PipeCopyError> {
        let mut bytes: Vec<u8> = Vec::with_capacity(len);
        let mut offset = 0;

        while offset < len {
            let chunk_len = (len - offset).min(self.chunk_size as usize) as u32;
            let chunk_addr = addr.wrapping_add(offset);
            let mut written = 0;

            // SAFETY: WriteFile asks the kernel to copy from chunk_addr. An inaccessible source
            // is reported as a Win32 invalid-buffer error rather than dereferenced by Rust.
            let result = unsafe {
                WriteFile(
                    self.write_handle,
                    chunk_addr.cast(),
                    chunk_len,
                    &mut written,
                    ptr::null_mut(),
                )
            };
            if result == 0 {
                let err = io::Error::last_os_error();
                let err = if matches!(
                    err.raw_os_error(),
                    Some(ERROR_NOACCESS) | Some(ERROR_INVALID_USER_BUFFER)
                ) {
                    io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "process context memory was unmapped during read",
                    )
                } else {
                    io::Error::new(
                        err.kind(),
                        format!("failed to copy process context memory: {err}"),
                    )
                };
                return Err(PipeCopyError {
                    err,
                    pipe_dirty: written != 0,
                });
            }
            if written == 0 {
                return Err(PipeCopyError {
                    err: io::Error::new(
                        io::ErrorKind::WriteZero,
                        "zero-length write while copying process context memory",
                    ),
                    pipe_dirty: false,
                });
            }

            let mut drained = 0;
            while drained < written {
                let mut read = 0;
                // SAFETY: bytes owns len bytes of spare capacity and
                // offset + written <= len, so the destination is writable.
                let result = unsafe {
                    ReadFile(
                        self.read_handle,
                        bytes.as_mut_ptr().add(offset + drained as usize).cast(),
                        written - drained,
                        &mut read,
                        ptr::null_mut(),
                    )
                };
                if result == 0 {
                    return Err(PipeCopyError {
                        err: last_error("failed to drain process context copy pipe"),
                        pipe_dirty: true,
                    });
                }
                if read == 0 {
                    return Err(PipeCopyError {
                        err: io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "process context copy pipe reported EOF",
                        ),
                        pipe_dirty: true,
                    });
                }
                drained += read;
            }

            offset += written as usize;
        }

        // SAFETY: every byte was initialized by ReadFile above.
        unsafe { bytes.set_len(len) };
        Ok(bytes)
    }
}

impl Drop for CopyPipe {
    fn drop(&mut self) {
        // SAFETY: both handles are owned by self and closed exactly once during drop.
        unsafe {
            CloseHandle(self.read_handle);
            CloseHandle(self.write_handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use core::{ffi::c_void, ptr};

    use super::{io, CopyPipe, ProcessMemoryCopy};

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_NOACCESS: u32 = 0x01;
    const PAGE_READWRITE: u32 = 0x04;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(
            address: *mut c_void,
            size: usize,
            allocation_type: u32,
            protect: u32,
        ) -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
        fn VirtualProtect(
            address: *mut c_void,
            size: usize,
            new_protect: u32,
            old_protect: *mut u32,
        ) -> i32;
    }

    #[test]
    fn copies_valid_memory_across_multiple_chunks() {
        let pipe = CopyPipe::new().expect("pipe creation should succeed");
        let len = pipe.chunk_size as usize + 1;
        let source: Vec<_> = (0..len).map(|index| index as u8).collect();

        let copied = pipe
            .copy(source.as_ptr(), source.len())
            .expect("memory copy should succeed");

        assert_eq!(copied, source);
    }

    #[test]
    fn rejects_inaccessible_memory() {
        // SAFETY: the arguments reserve and commit one writable page.
        let address = unsafe {
            VirtualAlloc(
                ptr::null_mut(),
                4096,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        };
        assert!(!address.is_null());
        let mut old_protect = 0;
        // SAFETY: address names the committed page allocated above and old_protect is writable.
        assert_ne!(
            unsafe { VirtualProtect(address, 4096, PAGE_NOACCESS, &mut old_protect) },
            0
        );

        let err = CopyPipe::new()
            .expect("pipe creation should succeed")
            .copy(address.cast(), 1)
            .expect_err("inaccessible memory should fail");

        assert_eq!(err.err.kind(), io::ErrorKind::WouldBlock);
        assert!(!err.pipe_dirty);
        // SAFETY: address was returned by VirtualAlloc and MEM_RELEASE requires size zero.
        assert_ne!(unsafe { VirtualFree(address, 0, MEM_RELEASE) }, 0);
    }
}
