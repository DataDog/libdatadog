// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_alloc::{AllocError, Allocator};
use std::alloc::Layout;
use std::borrow::Borrow;
use std::ffi::c_void;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::ptr::NonNull;
use std::{fmt, hash, ptr};

const USIZE_WIDTH: usize = core::mem::size_of::<usize>();

/// A struct which acts like a thin slice reference. It does this by storing
// the length of the slice just before the elements of the slice.
#[derive(Copy, Clone)]
#[repr(C)]
pub struct ThinSlice<'a, T: Copy> {
    thin_ptr: ThinPtr<T>,

    /// Since [`ThinSlice`] doesn't hold a reference but acts like one,
    // indicate this to the compiler with phantom data.
    // This takes up no space.
    _marker: PhantomData<&'a [T]>,
}

impl<T: Copy + fmt::Debug> fmt::Debug for ThinSlice<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

/// A struct which acts like a thin &str. It does this by storing the size
/// of the string just before the bytes of the string.
#[derive(Copy, Clone)]
#[repr(transparent)]
pub struct ThinStr<'a> {
    inner: ThinSlice<'a, u8>,
}

impl ThinStr<'_> {
    pub fn into_raw(self) -> NonNull<c_void> {
        self.inner.thin_ptr.size_ptr.cast()
    }

    /// Re-creates a [`ThinStr`] created by [`ThinStr::into_raw`].
    ///
    /// # Safety
    ///
    /// `this` needs to be created from [``ThinStr::into_raw`] and the storage
    /// it belongs to should still be alive.
    pub unsafe fn from_raw(this: NonNull<c_void>) -> Self {
        // SAFETY: `this` must have been produced by `ThinStr::into_raw` for
        // a compatible `ThinStr` allocation. After calling this function, the
        // original raw pointer must not be used again.
        let thin_ptr = ThinPtr {
            size_ptr: this.cast(),
            _marker: PhantomData,
        };
        Self {
            inner: ThinSlice {
                thin_ptr,
                _marker: PhantomData,
            },
        }
    }
}

impl fmt::Debug for ThinStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(f)
    }
}

// SAFETY: ThinStr is safe to send between threads as long as the underlying
// arena/storage remains alive. The caller must ensure the arena outlives all
// ThinStr references. This is the design trade-off for better performance
// than individual reference counting.
unsafe impl<T: Copy> Send for ThinPtr<T> {}
unsafe impl<T: Copy> Sync for ThinPtr<T> {}

unsafe impl<T: Copy> Send for ThinSlice<'_, T> {}
unsafe impl<T: Copy> Sync for ThinSlice<'_, T> {}

unsafe impl Send for ThinStr<'_> {}
unsafe impl Sync for ThinStr<'_> {}

impl ThinStr<'static> {
    pub const fn new() -> ThinStr<'static> {
        ThinStr {
            inner: ThinSlice {
                thin_ptr: EMPTY_INLINE_STRING.as_thin_ptr(),
                _marker: PhantomData,
            },
        }
    }

    pub const fn end_timestamp_ns() -> ThinStr<'static> {
        ThinStr {
            inner: ThinSlice {
                thin_ptr: END_TIMESTAMP_NS.as_thin_ptr(),
                _marker: PhantomData,
            },
        }
    }

    pub const fn local_root_span_id() -> ThinStr<'static> {
        ThinStr {
            inner: ThinSlice {
                thin_ptr: LOCAL_ROOT_SPAN_ID.as_thin_ptr(),
                _marker: PhantomData,
            },
        }
    }

    pub const fn trace_endpoint() -> ThinStr<'static> {
        ThinStr {
            inner: ThinSlice {
                thin_ptr: TRACE_ENDPOINT.as_thin_ptr(),
                _marker: PhantomData,
            },
        }
    }

    pub const fn span_id() -> ThinStr<'static> {
        ThinStr {
            inner: ThinSlice {
                thin_ptr: SPAN_ID.as_thin_ptr(),
                _marker: PhantomData,
            },
        }
    }
}

impl Default for ThinStr<'static> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Borrow<InlineString> for ConstString<N> {
    fn borrow(&self) -> &InlineString {
        let thin_ptr = ThinPtr {
            size_ptr: NonNull::from(self).cast::<u8>(),
            _marker: PhantomData,
        };
        // SAFETY: the object is layout compatible and lifetime is safe, and
        // inline strings are valid UTF-8.
        unsafe { &*thin_ptr.inline_string_ptr().as_ptr() }
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
struct ThinPtr<T: Copy> {
    /// Points to the beginning of an inline slice of T.
    size_ptr: NonNull<u8>,
    _marker: PhantomData<T>,
}

#[repr(C)]
pub struct InlineSlice<T: Copy> {
    /// Stores the len of `data` in native endian.
    size: [u8; core::mem::size_of::<usize>()],
    data: [T],
}

impl<T: Copy> Deref for InlineSlice<T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

#[repr(C)]
pub struct InlineString {
    /// Stores the len of `data` in native endian.
    size: [u8; core::mem::size_of::<usize>()],
    data: str,
}

impl Deref for InlineString {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<T: Copy> ThinPtr<T> {
    /// Reads the size prefix to get the length of the slice.
    const fn len(self) -> usize {
        // SAFETY: ThinPtr points to the size prefix of the slice.
        let size = unsafe { self.size_ptr.cast::<[u8; USIZE_WIDTH]>().as_ptr().read() };
        usize::from_ne_bytes(size)
    }

    /// Returns a wide pointer to an inline slice. The pointer is mut but you
    /// most likely shouldn't modify it.
    const fn inline_slice_ptr(self) -> NonNull<InlineSlice<T>> {
        let len = self.len();
        let slice = ptr::slice_from_raw_parts_mut(self.size_ptr.as_ptr(), len);
        // SAFETY: derived from a non-null pointer self.size_ptr.
        unsafe { NonNull::new_unchecked(slice as *mut [()] as *mut InlineSlice<T>) }
    }
}

impl ThinPtr<u8> {
    /// Returns a wide pointer to an inline string. The pointer is mut but you
    /// most likely shouldn't modify it.
    ///
    /// # Safety
    /// The bytes must be valid UTF-8 and originate from a valid `InlineString`
    /// layout created by this module.
    const unsafe fn inline_string_ptr(self) -> NonNull<InlineString> {
        let len = self.len();
        let slice = ptr::slice_from_raw_parts_mut(self.size_ptr.as_ptr(), len);
        // SAFETY: derived from a non-null pointer self.size_ptr.
        unsafe { NonNull::new_unchecked(slice as *mut [()] as *mut InlineString) }
    }
}

// Generic ThinSlice implementation
impl<'a, T: Copy> ThinSlice<'a, T> {
    /// Returns the length of the slice.
    pub fn len(&self) -> usize {
        self.thin_ptr.len()
    }

    /// Returns true if the slice is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the slice as a `&[T]`.
    pub fn as_slice(&self) -> &[T] {
        // SAFETY: ThinSlice is layout compatible with InlineSlice, and the
        // lifetime is correct.
        let inline_slice = unsafe { self.thin_ptr.inline_slice_ptr().as_ref() };
        &inline_slice.data
    }

    /// Computes the layout for a slice of the given length.
    pub fn layout_for(slice: &[T]) -> Result<Layout, AllocError> {
        let len = slice.len();
        let element_size = core::mem::size_of::<T>();
        let data_size = len.checked_mul(element_size).ok_or(AllocError)?;
        let total_size = USIZE_WIDTH.checked_add(data_size).ok_or(AllocError)?;
        Layout::from_size_align(total_size, 1).map_err(|_| AllocError)
    }

    /// Allocates memory for a slice and returns a pointer to uninitialized memory.
    pub fn try_allocate_for<A: Allocator>(
        slice: &[T],
        alloc: &A,
    ) -> Result<NonNull<[MaybeUninit<u8>]>, AllocError> {
        let layout = Self::layout_for(slice)?;
        let obj = alloc.allocate(layout)?;
        let ptr = obj.cast::<MaybeUninit<u8>>();
        Ok(NonNull::slice_from_raw_parts(ptr, obj.len()))
    }

    /// Tries to create a [`ThinSlice`] in the uninitialized space.
    ///
    /// # Errors
    ///
    /// Returns an error if the spare capacity is not large enough.
    pub fn try_from_slice_in(
        slice: &[T],
        spare_capacity: &'a mut [MaybeUninit<u8>],
    ) -> Result<Self, AllocError> {
        let layout = Self::layout_for(slice)?;
        if spare_capacity.len() < layout.size() {
            return Err(AllocError);
        }

        let allocation = spare_capacity.as_mut_ptr().cast::<u8>();

        // Write the size prefix
        let size_bytes = slice.len().to_ne_bytes();
        // SAFETY: we've verified the allocation is big enough and aligned.
        unsafe { core::ptr::copy_nonoverlapping(size_bytes.as_ptr(), allocation, USIZE_WIDTH) };

        // Write the data
        let data = unsafe { allocation.add(USIZE_WIDTH).cast::<T>() };
        // SAFETY: the allocation is big enough, locations are distinct, and
        // the memory is safe for writing.
        unsafe { core::ptr::copy_nonoverlapping(slice.as_ptr(), data, slice.len()) };

        let size_ptr = unsafe { NonNull::new_unchecked(allocation) };
        let thin_ptr = ThinPtr {
            size_ptr,
            _marker: PhantomData,
        };
        let _marker = PhantomData;
        Ok(ThinSlice { thin_ptr, _marker })
    }

    /// Creates a [`ThinSlice`] in the uninitialized space without checking capacity.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `spare_capacity` has enough space for the slice
    /// as determined by [`Self::layout_for`].
    pub unsafe fn from_slice_in_unchecked(
        slice: &[T],
        spare_capacity: &'a mut [MaybeUninit<u8>],
    ) -> Self {
        let allocation = spare_capacity.as_mut_ptr().cast::<u8>();

        // Write the size prefix
        let size_bytes = slice.len().to_ne_bytes();
        core::ptr::copy_nonoverlapping(size_bytes.as_ptr(), allocation, USIZE_WIDTH);

        // Write the data
        let data = unsafe { allocation.add(USIZE_WIDTH).cast::<T>() };
        core::ptr::copy_nonoverlapping(slice.as_ptr(), data, slice.len());

        let size_ptr = NonNull::new_unchecked(allocation);
        let thin_ptr = ThinPtr {
            size_ptr,
            _marker: PhantomData,
        };
        let _marker = PhantomData;
        ThinSlice { thin_ptr, _marker }
    }

    /// Returns the memory layout of this slice.
    pub fn layout(&self) -> Layout {
        // layout_for only fails on overflow or invalid align; for valid T and lengths
        // produced by this type, it should always succeed. In case of error, fall back to
        // a conservative layout that matches the actual allocation.
        Self::layout_for(self.as_slice()).unwrap_or_else(|_| unsafe {
            // Size = prefix + data length, with alignment of 1
            Layout::from_size_align_unchecked(USIZE_WIDTH + self.len(), 1)
        })
    }
}

impl<T: Copy> Deref for ThinSlice<'_, T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: Copy> PartialEq for ThinSlice<'_, T>
where
    T: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Copy> Eq for ThinSlice<'_, T> where T: Eq {}

impl<T: Copy> hash::Hash for ThinSlice<'_, T>
where
    T: hash::Hash,
{
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state)
    }
}

impl<T: Copy> Borrow<[T]> for ThinSlice<'_, T> {
    fn borrow(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Copy> Borrow<InlineSlice<T>> for ThinSlice<'_, T> {
    fn borrow(&self) -> &InlineSlice<T> {
        // SAFETY: ThinSlice is layout compatible with InlineSlice, and the
        // lifetime is correct.
        unsafe { self.thin_ptr.inline_slice_ptr().as_ref() }
    }
}

// String-specific ThinStr implementations that delegate to ThinSlice
impl<'a> ThinStr<'a> {
    // Note: len(), is_empty(), and as_bytes() are available through Deref<Target = str>

    /// Computes the layout for a string of the given length.
    pub fn layout_for(str: &str) -> Result<Layout, AllocError> {
        ThinSlice::layout_for(str.as_bytes())
    }

    /// Allocates memory for a string and returns a pointer to uninitialized memory.
    pub fn try_allocate_for<A: Allocator>(
        str: &str,
        alloc: &A,
    ) -> Result<NonNull<[MaybeUninit<u8>]>, AllocError> {
        ThinSlice::try_allocate_for(str.as_bytes(), alloc)
    }

    /// Tries to create a [`ThinStr`] in the uninitialized space.
    ///
    /// # Errors
    ///
    /// Returns an error if the spare capacity is not large enough.
    pub fn try_from_str_in(
        str: &str,
        spare_capacity: &'a mut [MaybeUninit<u8>],
    ) -> Result<Self, AllocError> {
        let inner = ThinSlice::try_from_slice_in(str.as_bytes(), spare_capacity)?;
        Ok(ThinStr { inner })
    }

    /// Creates a [`ThinStr`] in the uninitialized space without checking capacity.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `spare_capacity` has enough space for the string
    /// as determined by [`Self::layout_for`].
    pub unsafe fn from_str_in_unchecked(
        str: &str,
        spare_capacity: &'a mut [MaybeUninit<u8>],
    ) -> Self {
        let inner = ThinSlice::from_slice_in_unchecked(str.as_bytes(), spare_capacity);
        ThinStr { inner }
    }

    /// Returns the memory layout of this string.
    pub fn layout(&self) -> Layout {
        self.inner.layout()
    }
}

impl Deref for ThinStr<'_> {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        let inline_string: &InlineString = self.borrow();
        &inline_string.data
    }
}

impl Borrow<str> for ThinStr<'_> {
    fn borrow(&self) -> &str {
        self.deref()
    }
}

impl Borrow<InlineString> for ThinStr<'_> {
    fn borrow(&self) -> &InlineString {
        // SAFETY: as long as the lifetime is correct, then this is also safe.
        // If the caller is lying about the lifetime (e.g. dynamic lifetimes)
        // then the caller needs to be cautious about borrowing this, and
        // ThinStr only stores valid UTF-8 strings.
        unsafe { self.inner.thin_ptr.inline_string_ptr().as_ref() }
    }
}

impl PartialEq for ThinStr<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.deref() == other.deref()
    }
}

impl Eq for ThinStr<'_> {}

impl hash::Hash for ThinStr<'_> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        // Hash as a string to maintain consistency with &str
        self.deref().hash(state)
    }
}

impl<'a> From<ThinSlice<'a, u8>> for ThinStr<'a> {
    fn from(inner: ThinSlice<'a, u8>) -> Self {
        ThinStr { inner }
    }
}

impl<'a> From<ThinStr<'a>> for ThinSlice<'a, u8> {
    fn from(thin_str: ThinStr<'a>) -> Self {
        thin_str.inner
    }
}

/// [`ConstString`] is used to create the storage needed for static strings
/// that back [`ThinStr`]s.
#[repr(C)]
pub struct ConstString<const N: usize> {
    /// Stores the len of `data`.
    size: [u8; core::mem::size_of::<usize>()],
    data: [u8; N],
}

impl<const N: usize> ConstString<N> {
    const fn new(str: &str) -> Self {
        if str.len() != N {
            panic!("string length and storage mismatch for ConstString")
        }
        ConstString::<N> {
            size: N.to_ne_bytes(),
            data: {
                let src = str.as_bytes();
                let mut dst = [0u8; N];
                let mut i = 0usize;
                while i < N {
                    dst[i] = src[i];
                    i += 1;
                }
                dst
            },
        }
    }
    const fn as_thin_ptr(&self) -> ThinPtr<u8> {
        let ptr = core::ptr::addr_of!(self.size).cast::<u8>();
        // SAFETY: derived from static address, and ThinStr does not allow
        // modifications, so the mut-cast is also fine.
        let size_ptr = unsafe { NonNull::new_unchecked(ptr.cast_mut()) };
        ThinPtr {
            size_ptr,
            _marker: PhantomData,
        }
    }
}

static EMPTY_INLINE_STRING: ConstString<0> = ConstString::new("");
static END_TIMESTAMP_NS: ConstString<16> = ConstString::new("end_timestamp_ns");
static LOCAL_ROOT_SPAN_ID: ConstString<18> = ConstString::new("local root span id");
static TRACE_ENDPOINT: ConstString<14> = ConstString::new("trace endpoint");
static SPAN_ID: ConstString<7> = ConstString::new("span id");

#[no_mangle]
pub static DDOG_PROF_WELL_KNOWN_STRINGS: [ThinStr; 5] = [
    ThinStr::new(),
    ThinStr::end_timestamp_ns(),
    ThinStr::local_root_span_id(),
    ThinStr::trace_endpoint(),
    ThinStr::span_id(),
];

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_alloc::Global;

    const TEST_STRINGS: [&str; 5] = [
        "datadog",
        "MyNamespace.MyClass.MyMethod(Int32 id, String name)",
        "/var/run/datadog/apm.socket",
        "[truncated]",
        "Sidekiq::❨╯°□°❩╯︵┻━┻",
    ];

    #[test]
    fn test_allocation_and_deallocation() {
        let alloc = &Global;

        let mut thin_strs: Vec<ThinStr> = TEST_STRINGS
            .iter()
            .copied()
            .map(|str| {
                let obj = ThinStr::try_allocate_for(str, alloc).unwrap();
                // SAFETY: just allocated the bytes, no other references exist,
                // so we can safely turn it into `&mut [MaybeUninit<u8>]`.
                let uninit = unsafe { &mut *obj.as_ptr() };
                let thin_str = ThinStr::try_from_str_in(str, uninit).unwrap();
                let actual = thin_str.deref();
                assert_eq!(str, actual);
                thin_str
            })
            .collect();

        // This could detect out-of-bounds reads.
        for (thin_str, str) in thin_strs.iter().zip(TEST_STRINGS) {
            let actual = thin_str.deref();
            assert_eq!(str, actual);
        }

        for thin_str in thin_strs.drain(..) {
            unsafe { alloc.deallocate(thin_str.inner.thin_ptr.size_ptr, thin_str.layout()) };
        }
    }

    #[test]
    fn test_empty_string() {
        let alloc = &Global;

        let obj = ThinStr::try_allocate_for("", alloc).unwrap();
        let uninit = unsafe { &mut *obj.as_ptr() };
        let thin_str = ThinStr::try_from_str_in("", uninit).unwrap();

        assert_eq!(thin_str.deref(), "");
        assert_eq!(thin_str.deref().len(), 0);

        unsafe { alloc.deallocate(thin_str.inner.thin_ptr.size_ptr, thin_str.layout()) };
    }

    #[test]
    fn test_single_byte_strings() {
        let alloc = &Global;
        let single_bytes = ["a", "z", "0", "9", "!", "~"];

        for &s in &single_bytes {
            let obj = ThinStr::try_allocate_for(s, alloc).unwrap();
            let uninit = unsafe { &mut *obj.as_ptr() };
            let thin_str = ThinStr::try_from_str_in(s, uninit).unwrap();

            assert_eq!(thin_str.deref(), s);
            assert_eq!(thin_str.deref().len(), 1);

            unsafe { alloc.deallocate(thin_str.inner.thin_ptr.size_ptr, thin_str.layout()) };
        }
    }

    #[test]
    fn test_boundary_lengths() {
        let alloc = &Global;

        // Test strings around common boundary sizes
        let test_cases = [
            ("", 0),
            ("a", 1),
            ("ab", 2),
            ("abc", 3),
            ("abcd", 4),
            ("abcdefg", 7),
            ("abcdefgh", 8),
            ("abcdefghijklmno", 15),
            ("abcdefghijklmnop", 16),
            ("abcdefghijklmnopqrstuvwxyz123456", 32),
            ("abcdefghijklmnopqrstuvwxyz1234567", 33),
        ];

        for (s, expected_len) in test_cases {
            assert_eq!(s.len(), expected_len);

            let obj = ThinStr::try_allocate_for(s, alloc).unwrap();
            let uninit = unsafe { &mut *obj.as_ptr() };
            let thin_str = ThinStr::try_from_str_in(s, uninit).unwrap();

            assert_eq!(thin_str.deref(), s);
            assert_eq!(thin_str.deref().len(), expected_len);

            unsafe { alloc.deallocate(thin_str.inner.thin_ptr.size_ptr, thin_str.layout()) };
        }
    }

    #[test]
    fn test_unicode_edge_cases() {
        let alloc = &Global;

        let unicode_cases = [
            "é",                  // 2-byte UTF-8
            "€",                  // 3-byte UTF-8
            "🦀",                 // 4-byte UTF-8
            "\u{0000}",           // Null character
            "\u{FFFD}",           // Replacement character
            "a\u{0000}b",         // Embedded null
            "\n\r\t",             // Control characters
            "\u{1F600}\u{1F601}", // Multiple emoji
        ];

        for s in unicode_cases {
            let obj = ThinStr::try_allocate_for(s, alloc).unwrap();
            let uninit = unsafe { &mut *obj.as_ptr() };
            let thin_str = ThinStr::try_from_str_in(s, uninit).unwrap();

            assert_eq!(thin_str.deref(), s);
            assert_eq!(thin_str.deref().len(), s.len());

            unsafe { alloc.deallocate(thin_str.inner.thin_ptr.size_ptr, thin_str.layout()) };
        }
    }

    #[test]
    fn test_capacity() {
        // Test that try_from_str_in fails when there's not enough space
        let test_string = "hello world";
        let mut small_buffer = [std::mem::MaybeUninit::uninit(); 5]; // Too small

        let result = ThinStr::try_from_str_in(test_string, &mut small_buffer);
        assert!(result.is_err());

        // Test with exactly the right amount of space
        let required_size = test_string.len() + core::mem::size_of::<usize>();
        let mut buffer = vec![std::mem::MaybeUninit::uninit(); required_size];

        let thin_str = ThinStr::try_from_str_in(test_string, &mut buffer).unwrap();
        assert_eq!(thin_str.deref(), test_string);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            // Reduce test cases under miri for faster execution
            cases: if cfg!(miri) { 16 } else { 256 },
            ..proptest::prelude::ProptestConfig::default()
        })]

        #[test]
        fn test_thin_str_properties(test_string in ".*") {
            use std::borrow::Borrow;
            use std::hash::{Hash, Hasher};
            use std::collections::hash_map::DefaultHasher;

            let alloc = &Global;

            // Test layout calculation property
            let layout = ThinStr::layout_for(&test_string).unwrap();
            let min_size = test_string.len() + core::mem::size_of::<usize>();
            assert!(layout.size() >= min_size);
            assert!(layout.align() >= 1);
            assert!(layout.align().is_power_of_two());

            // Create ThinStr
            let obj = ThinStr::try_allocate_for(&test_string, alloc).unwrap();
            let uninit = unsafe { &mut *obj.as_ptr() };
            let thin_str = ThinStr::try_from_str_in(&test_string, uninit).unwrap();

            // Test borrowing properties
            let borrowed_str: &str = thin_str.borrow();
            assert_eq!(borrowed_str, test_string);

            let borrowed_inline: &InlineString = thin_str.borrow();
            assert_eq!(borrowed_inline.deref(), test_string);

            // Test deref consistency
            assert_eq!(thin_str.deref(), test_string);
            assert_eq!(thin_str.deref().len(), test_string.len());

            // Test hash consistency property
            let mut hasher1 = DefaultHasher::new();
            thin_str.hash(&mut hasher1);
            let hash1 = hasher1.finish();

            let mut hasher2 = DefaultHasher::new();
            test_string.hash(&mut hasher2);
            let hash2 = hasher2.finish();

            assert_eq!(hash1, hash2);

            // Test equality property - create another ThinStr with same content
            let obj2 = ThinStr::try_allocate_for(&test_string, alloc).unwrap();
            let uninit2 = unsafe { &mut *obj2.as_ptr() };
            let thin_str2 = ThinStr::try_from_str_in(&test_string, uninit2).unwrap();

            // Should be equal even though they're different allocations
            assert_eq!(thin_str, thin_str2);

            // Cleanup
            unsafe {
                alloc.deallocate(thin_str.inner.thin_ptr.size_ptr, thin_str.layout());
                alloc.deallocate(thin_str2.inner.thin_ptr.size_ptr, thin_str2.layout());
            }
        }
    }

    #[test]
    fn test_large_string() {
        let alloc = &Global;

        // Test a reasonably large string
        let large_string = "x".repeat(10000);
        let obj = ThinStr::try_allocate_for(&large_string, alloc).unwrap();
        let uninit = unsafe { &mut *obj.as_ptr() };
        let thin_str = ThinStr::try_from_str_in(&large_string, uninit).unwrap();

        assert_eq!(thin_str.deref(), large_string);
        assert_eq!(thin_str.deref().len(), 10000);

        unsafe { alloc.deallocate(thin_str.inner.thin_ptr.size_ptr, thin_str.layout()) };
    }
}
