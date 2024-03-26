// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

mod arena;
pub mod r#virtual;

pub use arena::*;
use core::ptr;
use std::sync::Once;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AllocError;

impl std::error::Error for AllocError {}

impl std::fmt::Display for AllocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("memory allocation failed")
    }
}

impl From<std::collections::TryReserveError> for AllocError {
    fn from(_value: std::collections::TryReserveError) -> Self {
        AllocError
    }
}

mod os {
    use std::io;

    #[cfg(unix)]
    pub fn page_size() -> io::Result<usize> {
        let result = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if result == -1 {
            Err(io::Error::last_os_error())
        } else if result < 0 {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("page size was negative: {result}"),
            ))
        } else {
            let size = result as usize;
            if !size.is_power_of_two() {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("page size was not a power of two: {size}"),
                ))
            } else {
                Ok(size)
            }
        }
    }

    #[cfg(windows)]
    pub fn page_size() -> io::Result<usize> {
        use core::mem;
        use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

        let mut system_info = mem::MaybeUninit::<SYSTEM_INFO>::uninit();
        // SAFETY: todo
        unsafe { GetSystemInfo(system_info.as_mut_ptr()) };

        // SAFETY: todo
        let system_info = unsafe { system_info.assume_init() };

        let size = system_info.dwPageSize;
        if !size.is_power_of_two() {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("page size was not a power of two: {size}"),
            ))
        } else {
            Ok(size as usize)
        }
    }
}

/// Gets the system's page size, which will be a power of two. Can panic if
/// the OS returns an unusual value such as a negative page size or one that
/// isn't a power of two.
#[inline]
pub fn page_size() -> usize {
    // todo: use OnceCell once we are on Rust 1.70+ to avoid unsafe.
    static INIT: Once = Once::new();
    static mut PAGE_SIZE: usize = 0;

    unsafe {
        INIT.call_once(|| {
            PAGE_SIZE = os::page_size().unwrap();
        });
        PAGE_SIZE
    }
}

/// Pads `bytes` to the `page_size`, which must be a power of two, and this
/// is not checked in release builds at all.
pub fn pad_to(bytes: usize, page_size: usize) -> Option<usize> {
    debug_assert!(page_size.is_power_of_two());

    // Usually, if bytes is evenly divisible by the page size, then use that
    // without bumping to the next size. However, we need to avoid zero.
    let bytes = bytes.max(page_size);

    // There's a bit-trick here to improve performance, because it's known
    // that page sizes are powers of 2. This means they have 1 bit set:
    //     00001000     (decimal 8)
    // So by subtracting one, you get a pattern like:
    //     00000111     (decimal 7)
    // If we do bytes & (page_size - 1), we get the same result as doing
    // bytes % page_size, but is faster and easier to implement.
    //     11111101     (decimal 253)
    //   & 00000111     (decimal 7)
    //     --------
    //     00000101     (decimal 5)
    let remainder = bytes & (page_size - 1);
    match remainder {
        0 => Some(bytes),

        // e.g. bytes=1024, page_size=4096, rem = 3072:
        // 1024 + (4096 - 3072) = 4096
        _ => bytes.checked_add(page_size - remainder),
        // By definition, the remainder is less than the divisor, so this
        // page_size - remainder cannot underflow.
    }
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct InBoundsPtr {
    /// The starting address of the allocation.
    base: ptr::NonNull<u8>,
    /// The offset from the base allocation. Never larger than [isize::MAX],
    /// nor the allocation's size.
    offset: usize,
    /// The length of the allocation. Note it's never larger than [isize::MAX].
    size: usize,
}

impl InBoundsPtr {
    pub const fn add(&self, offset: usize) -> Result<InBoundsPtr, AllocError> {
        match self.offset.checked_add(offset) {
            Some(new_offset) if new_offset <= self.size => Ok(InBoundsPtr {
                offset: new_offset,
                ..*self
            }),
            _ => Err(AllocError),
        }
    }

    pub fn align_to(&self, align: usize) -> Result<InBoundsPtr, AllocError> {
        let off = self.as_ptr().align_offset(align);
        self.add(off)
    }

    pub fn slice(&self, len: usize) -> Result<ptr::NonNull<[u8]>, AllocError> {
        match self.offset.checked_add(len) {
            Some(end) => {
                if end > self.size {
                    Err(AllocError)
                } else {
                    let slice = ptr::slice_from_raw_parts_mut(self.as_ptr(), len);
                    // SAFETY: cannot be null (derived from an allocation).
                    Ok(unsafe { ptr::NonNull::new_unchecked(slice) })
                }
            }
            None => Err(AllocError),
        }
    }

    pub fn as_ptr(&self) -> *mut u8 {
        // SAFETY: in-bounds (the whole point of the type).
        unsafe { self.base.as_ptr().add(self.offset) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Default page size for Linux.
    const LINUX: usize = 4096;

    // Default page size for Mac M1.
    const MAC_M1: usize = 16384;

    #[test]
    fn test_pad_to() {
        test_padding_ranges(LINUX);
        test_padding_ranges(MAC_M1);
    }

    fn test_padding_ranges(page: usize) {
        let two_pages = 2 * page;
        let three_pages = 3 * page;
        let four_pages = 4 * page;

        let cases = [
            (0..=page, page),
            ((page + 1)..=two_pages, two_pages),
            ((two_pages + 1)..=three_pages, three_pages),
            ((three_pages + 1)..=four_pages, four_pages),
        ];

        for (range, expected_pages) in cases {
            for value in range {
                assert_eq!(pad_to(value, page).unwrap(), expected_pages);
            }
        }
    }

    #[test]
    fn test_overflow() {
        let max = usize::MAX;
        assert_eq!(pad_to(max, LINUX), None);
        assert_eq!(pad_to(max, MAC_M1), None);
    }
}
