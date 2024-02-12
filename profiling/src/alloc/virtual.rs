// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::{pad_to, page_size};
use core::{mem, ptr, slice};
use std::io;

#[cfg(unix)]
mod unix {

    pub mod raw {
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
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::System::Memory;

    pub mod raw {
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
        ///  1. The ptr must have been previously allocated by [crate::alloc::r#virtual::raw::virtual_alloc].
        ///  2. The size should be the exact size it was allocated with.
        pub unsafe fn virtual_free(ptr: ptr::NonNull<()>, size: usize) -> io::Result<()> {
            let result = Memory::VirtualFree(ptr.as_ptr().cast(), 0, Memory::MEM_RELEASE);
            if result == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    pub fn alloc(min_size: usize) -> io::Result<Mapping> {
        let page_size = page_size();
        match pad_to(min_size, page_size) {
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("requested virtual allocation {min_size} could not be padded to the page size {page_size}"),
            )),
            Some(size) => {
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
                    Ok(Mapping {
                        // SAFETY: checked that the ptr was not null above.
                        base: unsafe { ptr::NonNull::new_unchecked(result).cast() },
                        size,
                    })
                }
            }
        }
    }
}

#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
pub use windows::*;

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
            Some(size) => Ok(Mapping {
                base: raw::virtual_alloc(size)?,
                size,
            })

        }
    }

    pub fn base_non_null_ptr<T>(&self) -> ptr::NonNull<T> {
        self.base.cast()
    }

    pub fn allocation_size(&self) -> usize {
        self.size
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: passing ptr and size exactly as received from alloc.
        let _result = unsafe { raw::virtual_free(self.base, self.size) };

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

impl core::ops::Deref for Mapping {
    type Target = [mem::MaybeUninit<u8>];

    fn deref(&self) -> &Self::Target {
        // SAFETY: todo
        unsafe { slice::from_raw_parts(self.base.cast().as_ptr(), self.size) }
    }
}

impl core::ops::DerefMut for Mapping {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: todo
        unsafe { slice::from_raw_parts_mut(self.base.cast().as_ptr(), self.size) }
    }
}
