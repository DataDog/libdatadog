// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// https://doc.rust-lang.org/beta/std/primitive.pointer.html#method.is_aligned_to
/// Convenience function until the std lib standardizes this.
#[cfg(test)]
#[track_caller]
pub(crate) fn is_aligned_to<T>(p: *const T, align: usize) -> bool {
    p.align_offset(align) == 0
}

#[cfg(test)]
pub(crate) fn fuzzer_inner_loop<A: crate::Allocator>(
    allocator: &A,
    size: usize,
    align_bits: u32,
    idx: usize,
    val: u8,
    max_size: usize,
) {
    use core::alloc::Layout;
    let idx = if size > 0 { idx % size } else { 0 };
    let align = 1usize << align_bits;
    let Ok(layout) = Layout::from_size_align(size, align) else {
        return;
    };

    if layout.pad_to_align().size() > max_size {
        return;
    };

    if let Ok(mut ptr) = allocator.allocate(layout) {
        assert!(is_aligned_to(ptr.cast::<u8>().as_ptr(), align));
        let obj = unsafe { ptr.as_mut() };
        // The object is guaranteed to be at least size, but can be larger
        assert!(obj.len() >= size);

        // Test that writing and reading a random index in the object works.
        obj[idx] = val;
        assert_eq!(obj[idx], val);

        // deallocate doesn't return memory to the allocator, but it shouldn't
        // panic, as that prevents its use in containers like Vec.
        unsafe { allocator.deallocate(ptr.cast(), layout) };
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::is_aligned_to;

    #[test]
    fn test_is_aligned_to() {
        #[repr(C, align(16))]
        struct Wide {
            data: [u8; 16],
        }

        static WIDE: Wide = Wide { data: [0; 16] };

        let wide = core::ptr::addr_of!(WIDE);
        assert!(is_aligned_to(wide, 1 << 0));
        assert!(is_aligned_to(wide, 1 << 1));
        assert!(is_aligned_to(wide, 1 << 2));
        assert!(is_aligned_to(wide, 1 << 3));
        assert!(is_aligned_to(wide, 1 << 4));

        let twelve = core::ptr::addr_of!(WIDE.data[12]);
        assert!(is_aligned_to(twelve, 1 << 0));
        assert!(is_aligned_to(twelve, 1 << 1));
        assert!(is_aligned_to(twelve, 1 << 2));
        assert!(!is_aligned_to(twelve, 1 << 3));
        assert!(!is_aligned_to(twelve, 1 << 4));
    }
}
