// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// https://doc.rust-lang.org/beta/std/primitive.pointer.html#method.is_aligned_to
/// Convenience function until the std lib standardizes this.
/// Currently only used in test code, so doing the power of two bit mask stuff the stdlib does
/// would be overkill.
#[cfg(test)]
pub(crate) fn is_aligned_to<T: ?Sized>(p: *const T, align: usize) -> bool {
    (p as *const u8 as usize) % align == 0
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
        assert!(is_aligned_to(ptr.as_ptr(), align));
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
    use crate::utils::is_aligned_to;

    #[test]
    fn test_is_aligned_to() {
        assert!(is_aligned_to(12 as *const u8, 1));
        assert!(is_aligned_to(12 as *const u8, 2));
        assert!(is_aligned_to(12 as *const u8, 3));
        assert!(is_aligned_to(12 as *const u8, 4));
        assert!(!is_aligned_to(12 as *const u8, 5));
        assert!(is_aligned_to(12 as *const u8, 6));
        assert!(!is_aligned_to(12 as *const u8, 7));
        assert!(!is_aligned_to(12 as *const u8, 8));
    }
}
