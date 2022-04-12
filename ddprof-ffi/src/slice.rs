use std::borrow::Cow;
use std::fmt::{Debug, Formatter};
use std::marker::PhantomData;
use std::os::raw::c_char;
use std::str::Utf8Error;

/// Remember, the data inside of each member is potentially coming from FFI,
/// so every operation on it is unsafe!
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Slice<'a, T: 'a> {
    ptr: *const T,
    len: usize,
    marker: PhantomData<&'a [T]>,
}

impl<'a, T: Debug> Debug for Slice<'a, T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        unsafe { f.debug_list().entries(self.as_slice().iter()).finish() }
    }
}

impl<'a, T: Eq> PartialEq<Self> for Slice<'a, T> {
    fn eq(&self, other: &Self) -> bool {
        unsafe {
            return self.as_slice() == other.as_slice();
        }
    }
}

impl<'a, T: Eq> Eq for Slice<'a, T> {}

// Use to represent strings -- should be valid UTF-8.
pub type CharSlice<'a> = crate::Slice<'a, c_char>;

/// Use to represent bytes -- does not need to be valid UTF-8.
pub type ByteSlice<'a> = crate::Slice<'a, u8>;

/// This exists as an intrinsic, but it is private.
pub fn is_aligned_and_not_null<T>(ptr: *const T) -> bool {
    !ptr.is_null() && ptr as usize % std::mem::align_of::<T>() == 0
}

pub trait AsBytes<'a> {
    /// # Safety
    /// Each implementor must document their safety requirements, but this is expected to be
    /// unsafe as this is for FFI types.
    unsafe fn as_bytes(&'a self) -> &'a [u8];

    /// # Safety
    /// This function has the same safety requirements as `as_bytes`.
    unsafe fn try_to_utf8(&'a self) -> Result<&'a str, Utf8Error> {
        std::str::from_utf8(self.as_bytes())
    }

    /// # Safety
    /// This function has the same safety requirements as `as_bytes`
    unsafe fn to_utf8_lossy(&'a self) -> Cow<'a, str> {
        String::from_utf8_lossy(self.as_bytes())
    }
}

impl<'a> AsBytes<'a> for Slice<'a, u8> {
    /// # Safety
    /// Slice needs to satisfy most of the requirements of std::slice::from_raw_parts except the
    /// aligned and non-null requirements, as this function will detect these conditions and
    /// return an empty slice instead.
    unsafe fn as_bytes(&'a self) -> &'a [u8] {
        if is_aligned_and_not_null(self.ptr) {
            std::slice::from_raw_parts(self.ptr, self.len)
        } else {
            &[]
        }
    }
}

impl<'a> AsBytes<'a> for Slice<'a, i8> {
    /// # Safety
    /// Slice needs to satisfy most of the requirements of std::slice::from_raw_parts except the
    /// aligned and non-null requirements, as this function will detect these conditions and
    /// return an empty slice instead.
    unsafe fn as_bytes(&'a self) -> &'a [u8] {
        if is_aligned_and_not_null(self.ptr) {
            std::slice::from_raw_parts(self.ptr as *const u8, self.len)
        } else {
            &[]
        }
    }
}

impl<'a, T: 'a> Slice<'a, T> {
    /// # Safety
    /// This function mostly has the same safety requirements as `std::str::from_raw_parts`, but
    /// it can tolerate mis-aligned and null pointers.
    pub unsafe fn new(ptr: *const T, len: usize) -> Self {
        if is_aligned_and_not_null(ptr) {
            Self {
                ptr,
                len,
                ..Default::default()
            }
        } else {
            Slice::default()
        }
    }

    /// # Safety
    /// This function mostly has the same safety requirements as `std::str::from_raw_parts`, but
    /// it can tolerate mis-aligned and null pointers.
    pub unsafe fn as_slice(&self) -> &'a [T] {
        if is_aligned_and_not_null(self.ptr) {
            std::slice::from_raw_parts(self.ptr, self.len)
        } else {
            &[]
        }
    }

    /// # Safety
    /// This function mostly has the same safety requirements as `std::str::from_raw_parts`, but
    /// it can tolerate mis-aligned and null pointers.
    pub unsafe fn into_slice(self) -> &'a [T] {
        if is_aligned_and_not_null(self.ptr) {
            std::slice::from_raw_parts(self.ptr, self.len)
        } else {
            &[]
        }
    }

    pub fn len(&self) -> usize {
        if is_aligned_and_not_null(self.ptr) {
            self.len
        } else {
            0
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<'a, T> Default for Slice<'a, T> {
    fn default() -> Self {
        /* The docs on std::slice::from_raw_parts indicate the pointer should be
         * non-null and suitably aligned for T even for zero-length slices.
         * Using a few tests, I wasn't actually able to create any harm with a
         * null pointer; after all it shouldn't get de-referenced and such, but
         * nonetheless we follow the documentation and use NonNull::dangling(),
         * which it suggests.
         * Since Slice's can be made from C, check for null and unaligned
         * pointers in associated functions to defend against this.
         */
        Self {
            ptr: std::ptr::NonNull::dangling().as_ptr(),
            len: 0,
            marker: Default::default(),
        }
    }
}

impl<'a, T: 'a> From<&'a [T]> for Slice<'a, T> {
    fn from(s: &'a [T]) -> Self {
        // SAFETY: Rust slices meet all the invariants required for Slice::new.
        unsafe { Slice::new(s.as_ptr() as *const T, s.len()) }
    }
}

impl<'a, T> From<&'a Vec<T>> for Slice<'a, T> {
    fn from(value: &'a Vec<T>) -> Self {
        Slice::from(value.as_slice())
    }
}

impl<'a> From<&'a str> for Slice<'a, c_char> {
    fn from(s: &'a str) -> Self {
        // SAFETY: Rust strings meet all the invariants required for Slice::new.
        unsafe { Slice::new(s.as_ptr() as *const c_char, s.len()) }
    }
}

#[cfg(test)]
mod test {
    use std::os::raw::c_char;

    use crate::*;

    #[test]
    fn slice_from_into_slice() {
        let raw: &[u8] = b"_ZN9wikipedia7article6formatE";
        let slice = Slice::from(raw);

        let converted: &[u8] = unsafe { slice.into_slice() };
        assert_eq!(converted, raw);
    }

    #[test]
    fn string_try_to_utf8() {
        let raw: &[u8] = b"_ZN9wikipedia7article6formatE";
        let slice = Slice::from(raw);

        let result = unsafe { slice.try_to_utf8() };
        assert!(result.is_ok());

        let expected = "_ZN9wikipedia7article6formatE";
        assert_eq!(expected, result.unwrap())
    }

    #[test]
    fn string_from_c_char() {
        let raw: &[u8] = b"_ZN9wikipedia7article6formatE";
        let slice = unsafe { Slice::new(raw.as_ptr() as *const c_char, raw.len()) };

        let result = unsafe { slice.try_to_utf8() };
        assert!(result.is_ok());

        let expected = "_ZN9wikipedia7article6formatE";
        assert_eq!(expected, result.unwrap())
    }

    #[derive(Debug, Eq, PartialEq)]
    struct Foo(i64);

    #[test]
    fn slice_from_foo() {
        let raw = Foo(42);
        let ptr = &raw as *const Foo;
        let slice = unsafe { Slice::new(ptr, 1) };

        let expected: &[Foo] = &[raw];
        let actual: &[Foo] = unsafe { slice.as_slice() };

        assert_eq!(expected, actual)
    }

    #[test]
    fn slice_from_null() {
        let ptr: *const usize = std::ptr::null();
        let expected: &[usize] = &[];
        let actual: &[usize] = unsafe { Slice::new(ptr, 0).as_slice() };
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_iterator() {
        let slice: &[i32] = &[1, 2, 3];
        let slice = Slice::from(slice);

        let mut iter = unsafe { slice.into_slice() }.iter();

        assert_eq!(Some(&1), iter.next());
        assert_eq!(Some(&2), iter.next());
        assert_eq!(Some(&3), iter.next());
    }
}
