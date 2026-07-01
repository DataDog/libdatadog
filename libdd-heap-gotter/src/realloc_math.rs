// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Pure helpers for sampled `realloc` layout arithmetic.
//!
//! Split out from `hooks::gotter_realloc` so the offset/size arithmetic
//! can be unit-tested on any host, without pulling in the Linux-only
//! GOT-hook module.

/// Size to pass to the underlying `realloc` for a sampled old block.
///
/// The MVP sampled-realloc path returns an *unsampled* pointer at
/// `new_raw` (offset 0), so the underlying block must be large enough
/// to hold both:
///
///   1. the old header + slack in `[0, old_offset)` (untouched by libc's realloc copy),
///   2. `size` bytes of user data at `[old_offset, old_offset + size)`, which we later `memmove`
///      down to `[0, size)`.
///
/// Hence `size + old_offset`. Returns `None` on overflow so the caller
/// can fail the realloc rather than truncate.
pub(crate) fn sampled_realloc_raw_size(size: usize, old_offset: usize) -> Option<usize> {
    size.checked_add(old_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampled_realloc_raw_size_reserves_old_offset_for_unsampled_result() {
        assert_eq!(sampled_realloc_raw_size(64, 16), Some(80));
        assert_eq!(sampled_realloc_raw_size(64, 64), Some(128));
        assert_eq!(sampled_realloc_raw_size(usize::MAX, 16), None);
    }
}
