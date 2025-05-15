// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

use std::{
    borrow, cmp, fmt, hash,
    ops::{self, RangeBounds},
    ptr::NonNull,
    sync::Arc,
};

#[cfg(feature = "serde")]
use serde::Serialize;

/// Immutable bytes type with zero copy cloning and slicing.
#[derive(Clone)]
pub struct Bytes {
    slice: &'static [u8],
    // The `bytes`` field is used to ensure that the underlying bytes are freed when there are no
    // more references to the `Bytes` object. For static buffers the field is `None`.
    bytes: Option<RefCountedBytes>,
}

/// The underlying bytes that the `Bytes` object references.
pub trait UnderlyingBytes: AsRef<[u8]> + Send + Sync + 'static {}

/// Since the Bytes type is immutable, and UnderlyingBytes is `Send + Sync``, it is safe to share
/// `Bytes` across threads.
unsafe impl Send for Bytes {}
unsafe impl Sync for Bytes {}

impl Bytes {
    /// Creates empty `Bytes`.
    #[inline]
    pub const fn empty() -> Self {
        Self::from_static(b"")
    }

    /// Creates `Bytes` from a static slice.
    #[inline]
    pub const fn from_static(value: &'static [u8]) -> Self {
        let slice: &[u8] = value;
        Self { slice, bytes: None }
    }

    /// Creates `Bytes` from a slice, by copying.
    pub fn copy_from_slice(data: &[u8]) -> Self {
        Self::from_underlying(data.to_vec())
    }

    /// Returns the length of the `Bytes`.
    #[inline]
    pub const fn len(&self) -> usize {
        self.slice.len()
    }

    /// Returns `true` if the `Bytes` is empty.
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.slice.is_empty()
    }

    /// Returns a slice of self for the provided range.
    ///
    /// This will return a new `Bytes` handle set to the slice, and will not copy the underlying
    /// data.
    ///
    /// This operation is `O(1)`.
    ///
    /// # Panics
    ///
    /// Slicing will panic if the range does not conform to  `start <= end` and `end <= self.len()`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tinybytes::Bytes;
    ///
    /// let bytes = Bytes::copy_from_slice(b"hello world");
    /// let slice = bytes.slice(0..5);
    /// assert_eq!(slice.as_ref(), b"hello");
    ///
    /// let slice = bytes.slice(6..11);
    /// assert_eq!(slice.as_ref(), b"world");
    /// ```
    pub fn slice(&self, range: impl RangeBounds<usize>) -> Self {
        use std::ops::Bound;

        let len = self.len();

        #[allow(clippy::expect_used)]
        let start = match range.start_bound() {
            Bound::Included(&n) => n,
            Bound::Excluded(&n) => n.checked_add(1).expect("range start overflow"),
            Bound::Unbounded => 0,
        };

        #[allow(clippy::expect_used)]
        let end = match range.end_bound() {
            Bound::Included(&n) => n.checked_add(1).expect("range end overflow"),
            Bound::Excluded(&n) => n,
            Bound::Unbounded => len,
        };

        assert!(
            start <= end,
            "range start must not be greater than end: {:?} > {:?}",
            start,
            end,
        );
        assert!(
            end <= len,
            "range end must not be greater than length: {:?} > {:?}",
            end,
            len,
        );

        if end == start {
            Bytes::empty()
        } else {
            self.safe_slice_ref(start, end)
        }
    }

    /// Returns a slice of self that is equivalent to the given `subset`, if it is a subset.
    ///
    /// When processing a `Bytes` buffer with other tools, one often gets a
    /// `&[u8]` which is in fact a slice of the `Bytes`, i.e. a subset of it.
    /// This function turns that `&[u8]` into another `Bytes`, as if one had
    /// called `self.slice()` with the range that corresponds to `subset`.
    ///
    /// This operation is `O(1)`.
    ///
    /// # Examples
    ///
    /// ```
    /// use tinybytes::Bytes;
    ///
    /// let bytes = Bytes::copy_from_slice(b"hello world");
    /// let subset = &bytes.as_ref()[0..5];
    /// let slice = bytes.slice_ref(subset).unwrap();
    /// assert_eq!(slice.as_ref(), b"hello");
    ///
    /// let subset = &bytes.as_ref()[6..11];
    /// let slice = bytes.slice_ref(subset).unwrap();
    /// assert_eq!(slice.as_ref(), b"world");
    ///
    /// let invalid_subset = b"invalid";
    /// assert!(bytes.slice_ref(invalid_subset).is_none());
    /// ```
    pub fn slice_ref(&self, subset: &[u8]) -> Option<Bytes> {
        // An empty slice can be a subset of any slice.
        if subset.is_empty() {
            return Some(Bytes::empty());
        }

        let subset_start = subset.as_ptr() as usize;
        let subset_end = subset_start + subset.len();
        let self_start = self.slice.as_ptr() as usize;
        let self_end = self_start + self.slice.len();
        if subset_start >= self_start && subset_end <= self_end {
            Some(self.safe_slice_ref(subset_start - self_start, subset_end - self_start))
        } else {
            None
        }
    }

    /// Returns a mutable reference to the slice of self.
    /// Allows for fast unchecked shrinking of the slice.
    ///
    /// # Safety
    ///
    /// Callers of that function must make sure that they only put subslices of the slice into the
    /// returned reference.
    /// They also need to make sure to not persist the slice reference for longer than the struct
    /// lives.
    #[inline]
    pub unsafe fn as_mut_slice(&mut self) -> &mut &'static [u8] {
        &mut self.slice
    }

    // private

    fn from_underlying(value: impl UnderlyingBytes) -> Self {
        Self {
            slice: unsafe { std::mem::transmute::<&'_ [u8], &'static [u8]>(value.as_ref()) },
            bytes: Some(arc_refcounted_bytes(Arc::new(value))),
        }
    }

    #[inline]
    fn safe_slice_ref(&self, start: usize, end: usize) -> Self {
        Self {
            slice: &self.slice[start..end],
            bytes: self.bytes.clone(),
        }
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        self.slice
    }
}

// Implementations of `UnderlyingBytes` for common types.
impl UnderlyingBytes for Vec<u8> {}
impl UnderlyingBytes for Box<[u8]> {}
impl UnderlyingBytes for String {}

// Implementations of common traits for `Bytes`.
impl Default for Bytes {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T: UnderlyingBytes> From<T> for Bytes {
    fn from(value: T) -> Self {
        Self::from_underlying(value)
    }
}

impl AsRef<[u8]> for Bytes {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl borrow::Borrow<[u8]> for Bytes {
    #[inline]
    fn borrow(&self) -> &[u8] {
        self.as_slice()
    }
}

impl ops::Deref for Bytes {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl<T: AsRef<[u8]>> PartialEq<T> for Bytes {
    #[inline]
    fn eq(&self, other: &T) -> bool {
        self.as_slice() == other.as_ref()
    }
}

impl Eq for Bytes {}

impl<T: AsRef<[u8]>> PartialOrd<T> for Bytes {
    fn partial_cmp(&self, other: &T) -> Option<cmp::Ordering> {
        self.as_slice().partial_cmp(other.as_ref())
    }
}

impl Ord for Bytes {
    fn cmp(&self, other: &Bytes) -> cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl hash::Hash for Bytes {
    // TODO should we cache the hash since we know the bytes are immutable?
    #[inline]
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.as_slice().hash(state);
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self.as_slice(), f)
    }
}

#[cfg(feature = "serde")]
impl Serialize for Bytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_bytes(self.as_slice())
    }
}

fn arc_refcounted_bytes<T>(data: Arc<T>) -> RefCountedBytes {
    unsafe fn arc_clone<T>(data: *const ()) -> RawRefCountedBytes {
        Arc::increment_strong_count(data as *const T);
        RawRefCountedBytes {
            data: unsafe { NonNull::new_unchecked(data as *mut ()) },
            vtable: RefCountedBytesVTable {
                clone: arc_clone::<T>,
                drop: arc_drop::<T>,
            },
        }
    }
    unsafe fn arc_drop<T>(data: *const ()) {
        Arc::from_raw(data as *const T);
    }
    RefCountedBytes {
        raw: RawRefCountedBytes {
            data: unsafe { NonNull::new_unchecked(Arc::into_raw(data) as *const () as *mut ()) },
            vtable: RefCountedBytesVTable {
                clone: arc_clone::<T>,
                drop: arc_drop::<T>,
            },
        },
    }
}

struct RefCountedBytes {
    raw: RawRefCountedBytes,
}

impl Clone for RefCountedBytes {
    fn clone(&self) -> Self {
        RefCountedBytes {
            raw: unsafe { (self.raw.vtable.clone)(self.raw.data.as_ptr().cast_const()) },
        }
    }
}

impl Drop for RefCountedBytes {
    fn drop(&mut self) {
        unsafe { (self.raw.vtable.drop)(self.raw.data.as_ptr().cast_const()) }
    }
}

struct RawRefCountedBytes {
    data: NonNull<()>,
    vtable: RefCountedBytesVTable,
}

struct RefCountedBytesVTable {
    clone: unsafe fn(*const ()) -> RawRefCountedBytes,
    drop: unsafe fn(*const ()),
}

#[cfg(feature = "bytes_string")]
mod bytes_string;
#[cfg(feature = "bytes_string")]
pub use bytes_string::BytesString;

#[cfg(test)]
mod test;

#[cfg(test)]
mod tests {
    use std::{ptr::NonNull, sync::atomic::AtomicUsize};

    use crate::{RawRefCountedBytes, RefCountedBytes, RefCountedBytesVTable};

    struct CustomArc<T> {
        rc: AtomicUsize,
        #[allow(unused)]
        data: T,
    }

    fn custom_arc_refcounted<T>(data: T) -> RefCountedBytes {
        unsafe fn custom_arc_clone<T>(data: *const ()) -> RawRefCountedBytes {
            let custom_arc = data as *const CustomArc<T>;
            let rc = unsafe {
                std::ptr::addr_of!((*custom_arc).rc)
                    .as_ref()
                    .unwrap_unchecked()
            };
            rc.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            RawRefCountedBytes {
                data: NonNull::new_unchecked(data as *mut ()),
                vtable: RefCountedBytesVTable {
                    clone: custom_arc_clone::<T>,
                    drop: custom_arc_drop::<T>,
                },
            }
        }

        unsafe fn custom_arc_drop<T>(data: *const ()) {
            let custom_arc = data as *const CustomArc<T>;
            let rc: &AtomicUsize = unsafe {
                std::ptr::addr_of!((*custom_arc).rc)
                    .as_ref()
                    .unwrap_unchecked()
            };
            if rc.fetch_sub(1, std::sync::atomic::Ordering::Release) == 1 {
                {
                    let custom_arc = (custom_arc as *mut CustomArc<T>)
                        .as_mut()
                        .unwrap_unchecked();
                    std::ptr::drop_in_place(custom_arc);
                }
                std::alloc::dealloc(
                    data as *mut () as *mut u8,
                    std::alloc::Layout::new::<CustomArc<T>>(),
                );
            }
        }

        let rc = Box::leak(Box::new(CustomArc {
            rc: AtomicUsize::new(1),
            data,
        })) as *mut _ as *const ();
        RefCountedBytes {
            raw: RawRefCountedBytes {
                data: unsafe { NonNull::new_unchecked(rc as *mut ()) },
                vtable: RefCountedBytesVTable {
                    clone: custom_arc_clone::<T>,
                    drop: custom_arc_drop::<T>,
                },
            },
        }
    }

    #[test]
    /// Run with miri to check that this works
    fn test_custom_arc_refcounted() {
        let data = vec![1, 2, 3];
        let refcounted = custom_arc_refcounted(data);
        let refcounted_clone = refcounted.clone();

        drop(refcounted);
        drop(refcounted_clone);
    }
}
