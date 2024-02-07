// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::{pad_to, page_size};
use core::{mem, ptr, slice};

#[repr(C)]
pub struct Mapping {
    base: ptr::NonNull<()>,
    size: usize,
}

impl Mapping {
    pub fn base_non_null_ptr<T>(&self) -> ptr::NonNull<T> {
        self.base.cast()
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

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io;

    impl Drop for Mapping {
        fn drop(&mut self) {
            // If this fails, there's not much we can do about it. We could panic
            // but panic in drops are generally frowned on. We compromise: in
            // debug builds, we panic if it's a -1 but in release builds we just
            // move on.
            let _result =
                unsafe { libc::munmap(self.base.as_ptr().cast(), self.size as libc::size_t) };

            #[cfg(debug_assertions)]
            if _result == -1 {
                panic!("failed to drop mapping: {}", io::Error::last_os_error());
            }
        }
    }

    pub fn alloc(min_size: usize) -> io::Result<Mapping> {
        #[cfg(debug_assertions)]
        if !min_size.is_power_of_two() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("requested virtual allocation was not a power of two: {min_size}"),
            ));
        }

        let page_size = page_size();
        match pad_to(min_size, page_size) {
            None =>
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("requested virtual allocation {min_size} could not be padded to the page size {page_size} "),
                )),
            Some(size) => {
                let result = unsafe { libc::mmap(
                    ptr::null_mut(),
                    size as libc::size_t,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0
                )};

                if result == libc::MAP_FAILED {
                    return Err(io::Error::last_os_error());
                }

                if result.is_null() {
                    unsafe { libc::munmap(result, size as libc::size_t) };
                    Err(io::Error::new(io::ErrorKind::Other, "mmap returned a null pointer"))
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
