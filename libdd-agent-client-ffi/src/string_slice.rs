// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! `DdogStringSlice` — a flat C-friendly view of a list of UTF-8 strings.
//!
//! Used by [`crate::agent_info`] to surface `AgentInfo::endpoints` (which
//! is a `Vec<String>` on the Rust side). The slice elements are
//! [`libdd_common_ffi::CharSlice`]s borrowing from the underlying
//! `AgentInfo` handle and remain valid for the handle's lifetime.

use libdd_common_ffi::CharSlice;

/// A list of UTF-8 strings exposed across the FFI boundary.
///
/// The `ptr` array is `len` `CharSlice` entries long. Each entry borrows
/// its bytes from the producing handle (e.g. an `AgentInfo`) and is
/// only valid while that handle is live. The slice itself (the `ptr`
/// allocation) is owned by the caller and must be released via the
/// matching `*_drop` function.
#[repr(C)]
pub struct DdogStringSlice<'a> {
    /// Pointer to an array of `len` borrowed [`CharSlice`] values.
    /// May be null if `len == 0`.
    pub ptr: *const CharSlice<'a>,
    /// Number of `CharSlice` elements at `.ptr`.
    pub len: usize,
}

impl<'a> DdogStringSlice<'a> {
    /// Build an owned `DdogStringSlice` from a borrowed slice of
    /// strings. The returned `Box<[CharSlice]>` is leaked and must be
    /// reclaimed by the caller via the matching free function.
    pub(crate) fn from_strings(strings: &'a [String]) -> Self {
        let entries: Vec<CharSlice<'a>> = strings
            .iter()
            .map(|s| CharSlice::from(s.as_str()))
            .collect();
        let mut boxed = entries.into_boxed_slice();
        let len = boxed.len();
        let ptr = boxed.as_mut_ptr();
        std::mem::forget(boxed);
        Self { ptr, len }
    }

    /// Empty slice (used when the underlying source has no entries).
    pub(crate) fn empty() -> Self {
        Self {
            ptr: std::ptr::null(),
            len: 0,
        }
    }
}

/// Free a [`DdogStringSlice`] previously produced by this crate's API
/// (for example by [`crate::ddog_agent_info_endpoints`]).
///
/// Frees only the outer array of `CharSlice` entries — the bytes the
/// entries point to are owned by the producing handle (e.g. an
/// `AgentInfo`) and remain alive until that handle is dropped.
///
/// # Safety
/// `slice` must have been produced by this crate, paired with its
/// original `len`. It is safe to call this with a default-zeroed
/// `DdogStringSlice` (null `ptr`, `len = 0`).
#[no_mangle]
pub unsafe extern "C" fn ddog_string_slice_drop(slice: DdogStringSlice<'_>) {
    if slice.ptr.is_null() || slice.len == 0 {
        return;
    }
    let s = std::slice::from_raw_parts_mut(slice.ptr as *mut CharSlice<'_>, slice.len);
    drop(Box::from_raw(s as *mut [CharSlice<'_>]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_strings_round_trip() {
        let owned = vec!["a".to_owned(), "bb".to_owned(), "ccc".to_owned()];
        let slice = DdogStringSlice::from_strings(&owned);
        assert_eq!(slice.len, 3);
        unsafe {
            let entries = std::slice::from_raw_parts(slice.ptr, slice.len);
            assert_eq!(entries[0].len(), 1);
            assert_eq!(entries[1].len(), 2);
            assert_eq!(entries[2].len(), 3);
            ddog_string_slice_drop(slice);
        }
    }

    #[test]
    fn empty_round_trip() {
        let slice = DdogStringSlice::empty();
        unsafe { ddog_string_slice_drop(slice) };
    }
}
