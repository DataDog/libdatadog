// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

mod arena;
mod r#virtual;

pub use arena::*;
use core::ptr::{self, NonNull};
use std::sync::Once;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AllocError;

impl std::error::Error for AllocError {}

impl std::fmt::Display for AllocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("memory allocation failed")
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
}

/// Gets the system's page size, which will be a power of two. Can panic if
/// the OS returns an unusual value such as a negative page size or one that
/// isn't a power of two.
#[inline]
pub fn page_size() -> usize {
    // todo: use OnceCell once we are on Rust 1.70+.
    static INIT: Once = Once::new();
    static mut PAGE_SIZE: usize = 0;

    unsafe {
        INIT.call_once(|| {
            PAGE_SIZE = os::page_size().unwrap();
        });
        PAGE_SIZE
    }
}

fn pad_to(bytes: usize, page_size: usize) -> Option<usize> {
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

// Keep this as a private trait.
trait AsNonNull<T> {
    unsafe fn as_non_null(&self) -> NonNull<T>;
    unsafe fn as_non_null_slice(&self) -> NonNull<[T]>;
}

impl AsNonNull<u8> for &[u8] {
    unsafe fn as_non_null(&self) -> NonNull<u8> {
        // SAFETY: slice pointers are always non-null, though they may dangle.
        NonNull::new_unchecked(self.as_ptr() as *mut u8)
    }

    unsafe fn as_non_null_slice(&self) -> NonNull<[u8]> {
        let slice = ptr::slice_from_raw_parts_mut(self.as_non_null().as_ptr(), self.len());
        NonNull::new_unchecked(slice)
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
