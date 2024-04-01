// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
pub trait VirtualAllocator {
    /// Allocates virtual memory of the given size, which must be a multiple
    /// of the page size and may not be zero.
    fn virtual_alloc(&self, size: usize) -> io::Result<ptr::NonNull<[u8]>>;

    /// # Safety
    /// The fatptr must have been previously allocated by [virtual_alloc] by
    /// this allocator, and must have the same address and length as it was
    /// returned with.
    unsafe fn virtual_free(&self, fatptr: ptr::NonNull<[u8]>) -> io::Result<()>;
}

#[cfg(unix)]
mod os {
    use super::VirtualAllocator;
    use std::{io, ptr};

    pub struct OsVirtualAllocator {}

    impl VirtualAllocator for OsVirtualAllocator {
        fn virtual_alloc(&self, size: usize) -> io::Result<ptr::NonNull<[u8]>> {
            let null = ptr::null_mut();
            let len = size as libc::size_t;
            let prot = libc::PROT_READ | libc::PROT_WRITE;
            let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
            // SAFETY: creates a new mapping (no weird behavior), akin to malloc.
            let result = unsafe { libc::mmap(null, len, prot, flags, -1, 0) };

            if result == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }

            let slice = ptr::slice_from_raw_parts_mut(result.cast(), size);
            // SAFETY: from my understanding of the spec, it's not possible to get
            // a mapping which starts at 0 (aka null) when MAP_FIXED wasn't given
            // and the specified address is 0.
            Ok(unsafe { ptr::NonNull::new_unchecked(slice) })
        }

        unsafe fn virtual_free(&self, fatptr: ptr::NonNull<[u8]>) -> io::Result<()> {
            // SAFETY: if the caller meets the safety conditions of this function,
            // then this is safe by extension.
            if libc::munmap(fatptr.as_ptr().cast(), fatptr.len() as libc::size_t) == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

#[cfg(windows)]
mod os {
    use std::{io, ptr};
    use windows_sys::Win32::System::Memory;
    pub struct OsVirtualAllocator {}

    impl VirtualAllocator for OsVirtualAllocator {
        fn virtual_alloc(&self, size: usize) -> io::Result<ptr::NonNull<[u8]>> {
            let null = ptr::null_mut();
            let alloc_type = Memory::MEM_COMMIT | Memory::MEM_RESERVE;
            let protection = Memory::PAGE_READWRITE;
            let result = unsafe { Memory::VirtualAlloc(null, size, alloc_type, protection) };

            match ptr::NonNull::new(result.cast::<u8>()) {
                Some(addr) => {
                    // todo: on rust 1.70+ use NonNull::slice_from_raw_parts to
                    //       avoid another use of unsafe.
                    let slice = ptr::slice_from_raw_parts_mut(addr.as_ptr(), size);
                    // SAFETY: `addr` is `NonNull`, so inherently not null.
                    Ok(unsafe { ptr::NonNull::new_unchecked(slice) })
                }
                None => Err(io::Error::last_os_error()),
            }
        }

        /// # Safety
        /// The fatptr must have been previously allocated by [virtual_alloc], and
        /// must have the same address and length as it was returned with.
        unsafe fn virtual_free(&self, fatptr: ptr::NonNull<[u8]>) -> io::Result<()> {
            if Memory::VirtualFree(fatptr.as_ptr().cast(), 0, Memory::MEM_RELEASE) == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }
}

use super::{pad_to, page_size, InBoundsPtr};
use std::{io, ptr};

pub use os::*;

#[derive(Debug)]
pub struct Mapping<A: VirtualAllocator = OsVirtualAllocator> {
    fatptr: ptr::NonNull<[u8]>,
    allocator: A,
}

/// # Safety
/// A mapping can move to a new thread, no problem. It's not Sync, though.
unsafe impl<A: Send + VirtualAllocator> Send for Mapping<A> {}

impl<A: VirtualAllocator> Mapping<A> {
    /// Pads `min_size` to the page size, and creates a new mapping of the
    /// padded size.
    pub fn new_in(min_size: usize, allocator: A) -> io::Result<Mapping<A>> {
        let page_size = page_size();
        match pad_to(min_size, page_size) {
            Some(size) if size <= isize::MAX as usize => {
                let fatptr = allocator.virtual_alloc(size)?;
                Ok(Mapping { fatptr, allocator })
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("requested virtual allocation of {min_size} bytes was too large (possibly after padding)"),
            )),
        }
    }

    #[inline]
    pub fn base_in_bounds_ptr(&self) -> InBoundsPtr {
        let base = self.fatptr;
        InBoundsPtr { base, offset: 0 }
    }

    #[inline]
    pub fn allocation_size(&self) -> usize {
        self.fatptr.len()
    }
}

impl<A: VirtualAllocator> Drop for Mapping<A> {
    fn drop(&mut self) {
        // SAFETY: passing fatptr` exactly as received from alloc.
        let _result = unsafe { self.allocator.virtual_free(self.fatptr) };

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
