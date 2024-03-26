// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{pad_to, page_size, InBoundsPtr};
use core::ptr;
use std::io;

#[cfg(unix)]
mod os {
    use std::{io, ptr};

    /// Allocates virtual memory of the given size, which must be a
    /// multiple of a page boundary and may not be zero.
    pub fn virtual_alloc(size: usize) -> io::Result<ptr::NonNull<()>> {
        let result = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size as libc::size_t,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };

        if result == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }

        if result.is_null() {
            unsafe { libc::munmap(result, size as libc::size_t) };
            Err(io::Error::new(
                io::ErrorKind::Other,
                "mmap returned a null pointer",
            ))
        } else {
            // SAFETY: checked that the ptr was not null above.
            Ok(unsafe { ptr::NonNull::new_unchecked(result).cast() })
        }
    }

    /// # Safety
    ///  1. The ptr must have been previously allocated by [virtual_alloc].
    ///  2. The size should be the exact size it was allocated with.
    pub unsafe fn virtual_free(ptr: ptr::NonNull<()>, size: usize) -> io::Result<()> {
        let result = libc::munmap(ptr.as_ptr().cast(), size as libc::size_t);
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
mod os {
    use std::{io, ptr};
    use windows_sys::Win32::System::Memory;

    pub fn virtual_alloc(size: usize) -> io::Result<ptr::NonNull<()>> {
        let result = unsafe {
            Memory::VirtualAlloc(
                ptr::null(),
                size,
                Memory::MEM_COMMIT | Memory::MEM_RESERVE,
                Memory::PAGE_READWRITE,
            )
        };
        if result.is_null() {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: checked that the ptr was not null above.
            Ok(unsafe { ptr::NonNull::new_unchecked(result).cast() })
        }
    }

    /// # Safety
    ///  1. The ptr must have been previously allocated by [virtual_alloc].
    ///  2. The size should be the exact size it was allocated with.
    pub unsafe fn virtual_free(ptr: ptr::NonNull<()>, _size: usize) -> io::Result<()> {
        let result = Memory::VirtualFree(ptr.as_ptr().cast(), 0, Memory::MEM_RELEASE);
        if result == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

pub use os::*;

#[repr(C)]
pub struct Mapping {
    base: ptr::NonNull<()>,
    size: usize,
}

/// # Safety
/// A mapping can move to a new thread, no problem. It's not Sync, though.
unsafe impl Send for Mapping {}

impl Mapping {
    pub fn new(min_size: usize) -> io::Result<Mapping> {
        let page_size = page_size();
        match pad_to(min_size, page_size) {
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("requested virtual allocation of {min_size} bytes could not be padded to the page size {page_size}"),
            )),
            Some(size) => {
                if size > isize::MAX as usize {
                    Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("padded virtual allocation of {min_size} exceeds isize::MAX"),
                    ))
                } else {
                    Ok(Mapping {
                        base: virtual_alloc(size)?,
                        size,
                    })
                }
            }
        }
    }

    pub fn base_in_bounds_ptr(&self) -> InBoundsPtr {
        let slice = ptr::slice_from_raw_parts_mut(self.base.cast::<u8>().as_ptr(), self.size);
        // SAFETY: derived from mapping's NonNull pointer.
        let base = unsafe { ptr::NonNull::new_unchecked(slice) };
        InBoundsPtr { base, offset: 0 }
    }

    pub fn allocation_size(&self) -> usize {
        self.size
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: passing ptr and size exactly as received from alloc.
        let _result = unsafe { virtual_free(self.base, self.size) };

        // If this fails, there's not much that can be done about it. It
        // could panic but panic in drops are generally frowned on.
        // Compromise: in debug builds, panic if it's invalid but in
        // release builds just move on.
        #[cfg(debug_assertions)]
        if let Err(err) = _result {
            panic!("failed to drop mapping: {err}");
        }
    }
}
