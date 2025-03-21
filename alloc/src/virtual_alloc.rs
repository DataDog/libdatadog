// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::AllocError;
use core::alloc::Layout;

/// Allocates entire pages of virtual memory for each allocation. This is
/// intended for large allocations only, such as working with other allocators
/// to provide a large chunk for them.
#[derive(Clone, Copy, Debug)]
pub struct VirtualAllocator {}

#[cfg_attr(debug_assertions, track_caller)]
#[inline]
fn layout_to_page_size(layout: Layout) -> Result<usize, AllocError> {
    if layout.size() == 0 {
        return Err(AllocError);
    }

    let page_size = os::page_size()?;
    let alignment = layout.align();
    if alignment > page_size {
        return Err(AllocError);
    }

    pad_to_pow2(layout.size(), page_size).ok_or(AllocError)
}

#[cfg_attr(debug_assertions, track_caller)]
#[inline]
fn pad_to_pow2(num: usize, pow2: usize) -> Option<usize> {
    debug_assert!(pow2.is_power_of_two());

    // Usually, if num is evenly divisible by the pow2, then use that without
    // bumping to the next size. However, we need to avoid zero.
    let bytes = num.max(pow2);

    // There's a bit-trick for powers of 2. This means they have 1 bit set:
    //     00001000     (decimal 8)
    // So by subtracting one, you get a pattern like:
    //     00000111     (decimal 7)
    // If we do num & (pow - 1), we get the same result as doing
    // num % pow, but is faster and easier to implement.
    //     11111101     (decimal 253)
    //   & 00000111     (decimal 7)
    //     --------
    //     00000101     (decimal 5)
    let remainder = bytes & (pow2 - 1);
    match remainder {
        0 => Some(bytes),

        // e.g. num=1024, pow=4096, remainder = 3072:
        // 1024 + (4096 - 3072) = 4096
        _ => bytes.checked_add(pow2 - remainder),
        // By definition, the remainder is less than the divisor, so this
        // pow - remainder cannot underflow.
    }
}

macro_rules! validate_page_size {
    ($x:expr) => {
        // On some platforms this may be unsigned or signed. We don't care if
        // this macro generates "dead code" for such things.
        #[allow(unused_comparisons)]
        if $x < 0 {
            Err(AllocError)
        } else {
            let size = $x as usize;
            if !size.is_power_of_two() {
                Err(AllocError)
            } else {
                Ok(size)
            }
        }
    };
}

#[cfg(unix)]
pub mod os {
    use super::VirtualAllocator;
    use allocator_api2::alloc::{AllocError, Allocator};
    use core::alloc::Layout;
    use core::ptr;

    pub fn page_size() -> Result<usize, AllocError> {
        // SAFETY: calling sysconf with correct arguments.
        let result = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        validate_page_size!(result)
    }

    unsafe impl Allocator for VirtualAllocator {
        fn allocate(&self, layout: Layout) -> Result<ptr::NonNull<[u8]>, AllocError> {
            self.allocate_zeroed(layout)
        }

        fn allocate_zeroed(&self, layout: Layout) -> Result<ptr::NonNull<[u8]>, AllocError> {
            if layout.size() == 0 {
                return Err(AllocError);
            }

            let size = super::layout_to_page_size(layout)?;

            let null = ptr::null_mut();
            let len = size as libc::size_t;
            let prot = libc::PROT_READ | libc::PROT_WRITE;
            let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
            // SAFETY: these args create a new mapping (no weird behavior),
            // akin to malloc.
            let result = unsafe { libc::mmap(null, len, prot, flags, -1, 0) };

            if result == libc::MAP_FAILED {
                return Err(AllocError);
            }

            // SAFETY: from my understanding of the spec, it's not possible to get
            // a mapping which starts at 0 (aka null) when MAP_FIXED wasn't given
            // and the specified address is 0.
            let addr = unsafe { ptr::NonNull::new_unchecked(result.cast()) };
            Ok(ptr::NonNull::slice_from_raw_parts(addr, size))
        }

        unsafe fn deallocate(&self, nonnull: ptr::NonNull<u8>, layout: Layout) {
            let ptr = nonnull.as_ptr();
            // SAFETY: this would have failed if it didn't fit, unless the
            // caller violated the preconditions.
            let size = super::layout_to_page_size(layout).unwrap_unchecked();

            // SAFETY: if the caller meets the safety conditions of this function,
            // then this is safe by extension.
            _ = libc::munmap(ptr.cast(), size);
        }
    }
}

#[cfg(windows)]
pub mod os {
    use super::VirtualAllocator;
    use allocator_api2::alloc::{AllocError, Allocator};
    use core::alloc::Layout;
    use core::{mem, ptr};
    use windows_sys::Win32::System::Memory;
    use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

    pub fn page_size() -> Result<usize, AllocError> {
        let mut system_info = mem::MaybeUninit::<SYSTEM_INFO>::uninit();
        // SAFETY: calling C function with correct uninit repr.
        unsafe { GetSystemInfo(system_info.as_mut_ptr()) };

        // SAFETY: GetSystemInfo is not documented to fail in any way, so it
        // should be safe to assume system_info was initialized.
        let system_info = unsafe { system_info.assume_init() };

        validate_page_size!(system_info.dwPageSize)
    }

    unsafe impl Allocator for VirtualAllocator {
        fn allocate(&self, layout: Layout) -> Result<ptr::NonNull<[u8]>, AllocError> {
            self.allocate_zeroed(layout)
        }

        fn allocate_zeroed(&self, layout: Layout) -> Result<ptr::NonNull<[u8]>, AllocError> {
            let size = super::layout_to_page_size(layout)?;

            let null = ptr::null_mut();
            let alloc_type = Memory::MEM_COMMIT | Memory::MEM_RESERVE;
            let protection = Memory::PAGE_READWRITE;
            // SAFETY: these args create a new allocation (no weird behavior),
            // akin to malloc.
            let result = unsafe { Memory::VirtualAlloc(null, size, alloc_type, protection) };

            match ptr::NonNull::new(result.cast::<u8>()) {
                Some(addr) => Ok(ptr::NonNull::slice_from_raw_parts(addr, size)),
                None => Err(AllocError),
            }
        }

        unsafe fn deallocate(&self, ptr: ptr::NonNull<u8>, _layout: Layout) {
            _ = Memory::VirtualFree(ptr.as_ptr() as *mut _, 0, Memory::MEM_RELEASE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::*;
    use allocator_api2::alloc::Allocator;
    use bolero::TypeGenerator;

    #[test]
    fn fuzz() {
        #[cfg(miri)]
        const MAX_SIZE: usize = 1_000_000;

        #[cfg(not(miri))]
        const MAX_SIZE: usize = isize::MAX as usize;

        let align_bits = 0..=32;
        let size = usize::produce();
        let idx = usize::produce();
        let val = u8::produce();
        let allocs = Vec::<(usize, u32, usize, u8)>::produce()
            .with()
            .values((size, align_bits, idx, val));
        bolero::check!()
            .with_generator(allocs)
            .for_each(|size_align_vec| {
                let allocator = VirtualAllocator {};

                for (size, align_bits, idx, val) in size_align_vec {
                    fuzzer_inner_loop(&allocator, *size, *align_bits, *idx, *val, MAX_SIZE)
                }
            })
    }

    #[test]
    fn test_zero_sized() {
        let alloc = VirtualAllocator {};
        assert_eq!(0, core::mem::size_of::<VirtualAllocator>());
        let zero_sized_layout = Layout::new::<VirtualAllocator>();
        _ = alloc.allocate(zero_sized_layout).unwrap_err();
    }

    #[test]
    fn test_too_large_alignment() {
        let page_size = os::page_size().unwrap();
        let too_large = (page_size + 1).next_power_of_two();
        let too_large_layout = Layout::from_size_align(1, too_large)
            .unwrap()
            .pad_to_align();
        let alloc = VirtualAllocator {};
        _ = alloc.allocate(too_large_layout).unwrap_err();
    }

    #[test]
    fn test_small_cases() {
        let page_size = os::page_size().unwrap();
        let alloc = VirtualAllocator {};

        // Allocations get rounded up to page size.
        let small_cases = [1, page_size - 1];
        for size in small_cases {
            let layout = Layout::from_size_align(size, 1).unwrap();
            let wide_ptr = alloc.allocate(layout).unwrap();
            assert_eq!(page_size, wide_ptr.len());
            unsafe { alloc.deallocate(wide_ptr.cast(), layout) };
        }

        // An even page size doesn't get rounded up.
        {
            let layout = Layout::from_size_align(page_size, page_size).unwrap();
            let wide_ptr = alloc.allocate(layout).unwrap();
            assert_eq!(page_size, wide_ptr.len());
            unsafe { alloc.deallocate(wide_ptr.cast(), layout) };
        }

        // page_size + 1 gets rounded up to the next page.
        {
            let layout = Layout::from_size_align(page_size + 1, page_size).unwrap();
            let wide_ptr = alloc.allocate(layout).unwrap();
            assert_eq!(2 * page_size, wide_ptr.len());
            unsafe { alloc.deallocate(wide_ptr.cast(), layout) };
        }
    }

    #[track_caller]
    fn realistic_size(size: usize) {
        let page_size = os::page_size().unwrap();
        let alloc = VirtualAllocator {};
        let layout = Layout::from_size_align(size, page_size).unwrap();
        let wide_ptr = alloc.allocate(layout).unwrap();
        let actual_size = wide_ptr.len();

        // Should be a multiple of page size.
        assert_eq!(0, actual_size % page_size);

        // Shouldn't ever be smaller than what was asked for.
        assert!(actual_size >= size);

        unsafe { alloc.deallocate(wide_ptr.cast(), layout) };
    }

    #[test]
    fn realistic_size_1mib() {
        realistic_size(1024 * 1024);
    }

    #[test]
    fn realistic_size_2mib() {
        realistic_size(2 * 1024 * 1024);
    }

    #[test]
    fn realistic_size_4mib() {
        realistic_size(4 * 1024 * 1024);
    }
}
