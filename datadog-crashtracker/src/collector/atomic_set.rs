// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module introduces an `AtomicMultiset`, which is intended to allow lock free operation
//! including inside a crash signal handler.
//! This is useful to allow clients to register metadata about program execution, and then the
//! handler can emit that information into the crash-report.
//! If this is useful for other cases, we can consider moving it to ddcommon.

use portable_atomic::AtomicUsize;
use rand::Rng;
use std::fmt::Debug;
use std::io::Write;
use std::num::NonZeroU128;
use std::ptr::null_mut;
use std::sync::atomic::Ordering::SeqCst;

#[derive(Debug)]
pub enum AtomicSetError {
    NoSpace(String),
    IndexOutOfRange(usize),
    NoElementAtIndex(usize),
    IoError(std::io::Error),
}

impl std::fmt::Display for AtomicSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AtomicSetError::NoSpace(value) => write!(f, "No space to store value: {}", value),
            AtomicSetError::IndexOutOfRange(idx) => write!(f, "Index {} out of range", idx),
            AtomicSetError::NoElementAtIndex(idx) => {
                write!(f, "Expected an element at index {}", idx)
            }
            AtomicSetError::IoError(err) => write!(f, "IO error: {}", err),
        }
    }
}

impl std::error::Error for AtomicSetError {}

impl From<std::io::Error> for AtomicSetError {
    fn from(err: std::io::Error) -> Self {
        AtomicSetError::IoError(err)
    }
}

pub(crate) type AtomicSpanSet<const LEN: usize> = AtomicMultiset<AtomicSpan, LEN>;
pub(crate) type AtomicStringMultiset<const LEN: usize> = AtomicMultiset<AtomicString, LEN>;

pub trait Atomic {
    type Item: Ord + PartialEq + Debug + Clone;
    const NONE: Self;
    /// Returns whether there was a value before
    fn clear(&self) -> bool {
        self.take().is_some()
    }
    /// Returns whether there was anything to emit.
    fn consume_and_emit(
        &self,
        w: &mut impl Write,
        leak: bool,
        first: bool,
    ) -> Result<bool, AtomicSetError>;
    /// SAFETY: This is only safe to use in a single threaded context
    #[cfg(test)]
    unsafe fn load(&self) -> Option<Self::Item>;
    /// Swaps the value with the old, returning the old
    fn swap(&self, new: Option<Self::Item>) -> Option<Self::Item>;
    /// Takes the value, leaving EMPTY_INNER in its place.
    fn take(&self) -> Option<Self::Item> {
        self.swap(None)
    }
    /// Returns `None` if the insert succeeded.
    /// Returns `Some(val)` if the insert failed.
    fn try_insert(&self, val: Self::Item) -> Option<Self::Item>;
}

/// An atomic multiset, suitable for use in signal handler contexts.
pub struct AtomicMultiset<T, const LEN: usize> {
    used: AtomicUsize,
    set: [T; LEN],
}

impl<T, const LEN: usize> AtomicMultiset<T, LEN>
where
    T: Atomic,
    <T as Atomic>::Item: std::cmp::PartialEq + Debug + Ord,
{
    /// Atomicity: The individual operations of the clear are atomic, but the overall operation
    /// is not atomic.
    /// This should only be used in a context where no other threads are modifying the set.
    /// Performance: This operation is constant time.
    pub fn clear(&self) -> Result<(), AtomicSetError> {
        if !self.is_empty() {
            for v in self.set.iter() {
                if v.clear() {
                    self.used.sub(1, SeqCst)
                }
            }
        }
        Ok(())
    }

    /// Removes an element from the array at element idx.
    /// Returns:
    ///     Ok if the operation succeeds.
    ///     Err if the idx was out of bounds, or had no element to remove.
    /// Atomicity: This operation is atomic and lock-free.
    ///     Updates to values and `len()` are not transactional, but are eventually consistent in
    ///     that `len` will be correctly updated once this operation completes.
    ///     Until then, the invariant that `len`` >= actual number of elements in the array is
    ///     maintained
    /// Performance: This operation is constant time.
    pub fn remove(&self, idx: usize) -> Result<(), AtomicSetError> {
        if idx >= self.set.len() {
            return Err(AtomicSetError::IndexOutOfRange(idx));
        }
        if self.set[idx].clear() {
            self.used.sub(1, SeqCst)
        } else {
            return Err(AtomicSetError::NoElementAtIndex(idx));
        }
        Ok(())
    }

    pub const fn new() -> Self {
        Self {
            used: AtomicUsize::new(0),
            set: [T::NONE; LEN],
        }
    }

    /// Inserts an element into the array.
    /// Returns:
    ///     Ok(idx) if the operation succeeds.  `Idx` can later be used as an argument to `remove`.
    ///     Err if insert failed
    /// Atomicity: This operation is atomic and lock-free.
    ///     Updates to values and `len()` are not transactional, but are eventually consistent in
    ///     that `len` will be correctly updated once this operation completes.
    ///     Until then, the invariant that `len`` >= actual number of elements in the array is
    ///     maintained.
    /// Performance:
    ///     As long as the invariant is maintained that the array is <= 1/2 full, this is amortized
    ///     constant time.
    pub fn insert(&self, mut value: T::Item) -> Result<usize, AtomicSetError> {
        let used = self.used.fetch_add(1, SeqCst);
        if used >= self.set.len() / 2 {
            // We only fill to half full to get good amortized behaviour
            self.used.fetch_sub(1, SeqCst);
            return Err(AtomicSetError::NoSpace(format!("{:?}", &value)));
        }

        // Start at a random position.
        // Since the array is only at most half full, and since we start scanning at random
        // indicies, every slot should independently have <.5 probability of being occupied.
        // Long scans become exponentially unlikely, giving amortized constant time insertion.
        // Try 10 random locations, this should succeed 0.999 of the time.
        for _ in 0..10 {
            let idx: usize = rand::thread_rng().gen_range(0..self.set.len());
            if let Some(v) = self.set[idx].try_insert(value) {
                value = v;
            } else {
                return Ok(idx);
            }
        }

        // In the case where it doesn't succeed, do a linear probe to guarantee it lands somewhere.
        // Since we enforce that the array is only half full, this is guarantee to succeed.
        // We leave this to second to avoid the chains that can build up with linear probing.
        let shift: usize = rand::thread_rng().gen_range(0..self.set.len());
        for i in 0..self.set.len() {
            let idx = (i + shift) % self.set.len();

            if let Some(v) = self.set[idx].try_insert(value) {
                value = v;
            } else {
                return Ok(idx);
            }
        }
        Err(AtomicSetError::NoSpace("This should be unreachable: we ensure that there was at least one empty slot before entering the loop".to_string()))
    }

    /// Best effort check if the array is definitely empty.
    /// Returns:
    ///     If the array is definitely empty. Note that this may spuriously fail (see atomicity).
    /// Atomicity: This operation is atomic and lock-free.
    ///     Updates to values and `len()` are not transactional, but are eventually consistent in
    ///     that `len` will be correctly updated once this operations complete.
    ///     Until then, the invariant that `len`` >= actual number of elements in the array is
    ///     maintained.    
    /// Performance: Constant time
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Best effort length of the array.
    /// Returns:
    ///     A number that is at least as large as the number of elements in the set.
    /// Atomicity: This operation is atomic and lock-free.
    ///     Updates to values and `len()` are not transactional, but are eventually consistent in
    ///     that `len` will be correctly updated once this operations complete.
    ///     Until then, the invariant that `len`` >= actual number of elements in the array is
    ///     maintained.    
    /// Performance: Constant time
    pub fn len(&self) -> usize {
        self.used.load(SeqCst)
    }

    /// Emits the set to the given writer. This is useful to allow the crashtracker collector to
    /// transmit the set to the receiver.  As suggested in the name, this function consumes the
    /// elements of the set.
    /// The `leak` argument is useful inside a signal handler where calls to the allocator are
    /// prohibited.
    /// Returns:
    ///     If an error occurred during the operation.
    /// Atomicity:
    ///     This operation is atomic and lock-free.
    ///     It is not transactional: if elements are added during this operation, they may or may
    ///     not make into the emitted output.
    /// Performance: This does a linear scan over the entire array, and then emits any found items.
    /// It is therefore O(set.capacity()) + O(set.len()).
    pub fn consume_and_emit(&self, w: &mut impl Write, leak: bool) -> Result<(), AtomicSetError> {
        write!(w, "[")?;

        if self.used.load(SeqCst) > 0 {
            let mut first = true;
            for it in self.set.iter() {
                if it.consume_and_emit(w, leak, first)? {
                    first = false;
                }
            }
        }
        writeln!(w, "]")?;
        Ok(())
    }

    #[cfg(test)]
    /// This is unsafe when used in a concurrent context
    /// Putting it under cfg(test) to avoid its use in production.
    pub fn values(&self) -> Result<Vec<T::Item>, AtomicSetError> {
        let mut rval = Vec::with_capacity(self.used.load(SeqCst));
        if self.used.load(SeqCst) > 0 {
            for it in self.set.iter() {
                // SAFETY: only use this in test code, where we are guaranteed to be single threaded
                if let Some(v) = unsafe { it.load() } {
                    rval.push(v);
                }
            }
        }
        rval.sort();
        Ok(rval)
    }
}

pub(crate) struct AtomicString {
    inner: portable_atomic::AtomicPtr<String>,
}

impl AtomicString {
    fn ptr_from_inner(v: Option<String>) -> *mut String {
        v.map(|s| Box::into_raw(Box::new(s))).unwrap_or(null_mut())
    }

    // Safety: This should only be called on pointers that came from `ptr_from_inner`.
    unsafe fn inner_from_ptr(v: *mut String) -> Option<String> {
        if v.is_null() {
            None
        } else {
            Some(*Box::from_raw(v))
        }
    }
}

impl Atomic for AtomicString {
    type Item = String;
    // In this case, we actually WANT multiple copies of the interior mutable struct
    #[allow(clippy::declare_interior_mutable_const)]
    const NONE: Self = Self {
        inner: portable_atomic::AtomicPtr::new(null_mut()),
    };

    /// Returns whether there was anything to emit.
    fn consume_and_emit(
        &self,
        w: &mut impl Write,
        leak: bool,
        first: bool,
    ) -> Result<bool, AtomicSetError> {
        if let Some(s) = self.take() {
            if !first {
                write!(w, ", ")?;
            }
            write!(w, "\"{s}\"")?;

            if leak {
                String::leak(s);
            }

            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// SAFETY: This is only safe to use in a single threaded context
    #[cfg(test)]
    unsafe fn load(&self) -> Option<Self::Item> {
        let v = self.inner.load(SeqCst);
        if v.is_null() {
            None
        } else {
            // Safety: the pointer is non-null, and was created from a box by the insert functions.
            // We need to clone here since the set owns the original.
            Some((*v).clone())
        }
    }

    fn swap(&self, new: Option<Self::Item>) -> Option<Self::Item> {
        let old = self.inner.swap(Self::ptr_from_inner(new), SeqCst);
        // Safety: This pointer came from `ptr_from_inner` since that's the only way to set a value
        unsafe { Self::inner_from_ptr(old) }
    }

    /// Returns whether the insert succeeded.
    fn try_insert(&self, val: Self::Item) -> Option<Self::Item> {
        let ptr = Self::ptr_from_inner(Some(val));
        if self
            .inner
            .compare_exchange(null_mut(), ptr, SeqCst, SeqCst)
            .is_err()
        {
            // Safety: This pointer came from `ptr_from_inner`
            // The insert failed, so we own the only copy of it.
            unsafe { Self::inner_from_ptr(ptr) }
        } else {
            None
        }
    }
}

pub(crate) struct AtomicSpan {
    inner: portable_atomic::AtomicU128,
}

impl Atomic for AtomicSpan {
    type Item = NonZeroU128;
    // In this case, we actually WANT multiple copies of the interior mutable struct
    #[allow(clippy::declare_interior_mutable_const)]
    const NONE: Self = Self {
        inner: portable_atomic::AtomicU128::new(0),
    };

    /// Returns whether there was anything to emit.
    fn consume_and_emit(
        &self,
        w: &mut impl Write,
        _leak: bool,
        first: bool,
    ) -> Result<bool, AtomicSetError> {
        if let Some(v) = self.take() {
            if !first {
                write!(w, ", ")?;
            }
            write!(w, "{{\"id\": \"{v}\"}}")?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// SAFETY: This is only safe to use in a single threaded context
    #[cfg(test)]
    unsafe fn load(&self) -> Option<Self::Item> {
        NonZeroU128::new(self.inner.load(SeqCst))
    }

    fn swap(&self, new: Option<Self::Item>) -> Option<Self::Item> {
        let new = new.map(|x| x.get()).unwrap_or_default();
        NonZeroU128::new(self.inner.swap(new, SeqCst))
    }

    /// Returns whether the insert succeeded.
    fn try_insert(&self, val: Self::Item) -> Option<Self::Item> {
        if self
            .inner
            .compare_exchange(0, val.get(), SeqCst, SeqCst)
            .is_err()
        {
            Some(val)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_new() -> anyhow::Result<()> {
        let s: AtomicSpanSet<16> = AtomicSpanSet::new();
        assert_eq!(s.len(), 0);
        assert_eq!(&s.values()?, &[]);
        Ok(())
    }

    #[test]
    fn test_string_new() -> anyhow::Result<()> {
        let s: AtomicStringMultiset<16> = AtomicStringMultiset::new();
        assert_eq!(s.len(), 0);
        assert!(s.values()?.is_empty());
        Ok(())
    }

    #[test]
    fn test_string_ops() -> anyhow::Result<()> {
        let mut expected = std::collections::BTreeMap::<String, usize>::new();
        let s = AtomicStringMultiset::<8>::new();
        compare(&s, &expected);
        insert_and_compare(&s, &mut expected, "a".to_string());
        insert_and_compare(&s, &mut expected, "b".to_string());
        insert_and_compare(&s, &mut expected, "".to_string());
        insert_and_compare(&s, &mut expected, "c".to_string());
        insert(&s, &mut expected, "e".to_string()).expect_err("Should stop when half full");

        s.remove(200)
            .expect_err("Shouldn't let us go outside the range");

        remove_and_compare(&s, &mut expected, "a".to_string());
        insert_and_compare(&s, &mut expected, "d".to_string());
        remove_and_compare(&s, &mut expected, "c".to_string());

        s.clear()?;
        expected.clear();
        compare(&s, &expected);
        insert_and_compare(&s, &mut expected, "z".to_string());
        // Prevent memory leaks
        s.clear()?;
        Ok(())
    }

    #[test]
    fn test_span_ops() -> anyhow::Result<()> {
        let mut expected = std::collections::BTreeMap::<NonZeroU128, usize>::new();
        let s: AtomicSpanSet<8> = AtomicSpanSet::<8>::new();
        compare(&s, &expected);
        insert_and_compare(&s, &mut expected, nz(42));
        insert_and_compare(&s, &mut expected, nz(21));
        insert_and_compare(&s, &mut expected, nz(19));
        insert_and_compare(&s, &mut expected, nz(3));
        insert(&s, &mut expected, nz(8)).expect_err("Should stop when half full");

        s.remove(200)
            .expect_err("Shouldn't let us go outside the range");

        remove_and_compare(&s, &mut expected, nz(42));
        insert_and_compare(&s, &mut expected, nz(12));
        remove_and_compare(&s, &mut expected, nz(19));

        s.clear()?;
        expected.clear();
        compare(&s, &expected);
        insert_and_compare(&s, &mut expected, nz(12));

        Ok(())
    }

    fn nz(v: u128) -> NonZeroU128 {
        NonZeroU128::new(v).unwrap()
    }

    #[test]
    fn test_span_emit() {
        let s: AtomicSpanSet<8> = AtomicSpanSet::new();
        s.insert(nz(42)).unwrap();
        s.insert(nz(21)).unwrap();
        let mut buf = Vec::new();
        s.consume_and_emit(&mut buf, false).unwrap();
        let actual = String::from_utf8(buf).unwrap();
        assert!(
            actual == "[{\"id\": \"42\"}, {\"id\": \"21\"}]\n"
                || actual == "[{\"id\": \"21\"}, {\"id\": \"42\"}]\n"
        );
    }

    fn bs(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn test_string_emit() {
        let s: AtomicStringMultiset<8> = AtomicStringMultiset::new();
        s.insert(bs("hello")).unwrap();
        s.insert(bs("world")).unwrap();
        let mut buf = Vec::new();
        s.consume_and_emit(&mut buf, false).unwrap();
        let actual = String::from_utf8(buf).unwrap();
        assert!(
            actual == "[\"hello\", \"world\"]\n" || actual == "[\"world\", \"hello\"]\n",
            "actual was {actual}"
        );
    }

    fn remove_and_compare<T: Atomic>(
        s: &AtomicMultiset<T, 8>,
        expected: &mut std::collections::BTreeMap<T::Item, usize>,
        v: T::Item,
    ) {
        remove(s, expected, v).unwrap();
        compare(s, expected);
    }

    fn remove<T: Atomic>(
        s: &AtomicMultiset<T, 8>,
        expected: &mut std::collections::BTreeMap<T::Item, usize>,
        v: T::Item,
    ) -> anyhow::Result<()> {
        let idx = expected.get(&v).unwrap();
        s.remove(*idx).unwrap();
        expected.remove(&v);
        Ok(())
    }

    fn compare<T: Atomic>(
        s: &AtomicMultiset<T, 8>,
        expected: &std::collections::BTreeMap<T::Item, usize>,
    ) {
        let actual = s.values().unwrap();
        let golden: Vec<_> = expected.keys().cloned().collect();
        assert_eq!(actual, golden);
        assert_eq!(expected.len(), s.len());
    }

    fn insert<T: Atomic>(
        s: &AtomicMultiset<T, 8>,
        expected: &mut std::collections::BTreeMap<T::Item, usize>,
        v: T::Item,
    ) -> anyhow::Result<()> {
        expected.insert(v.clone(), s.insert(v)?);
        Ok(())
    }

    fn insert_and_compare<T: Atomic>(
        s: &AtomicMultiset<T, 8>,
        expected: &mut std::collections::BTreeMap<T::Item, usize>,
        v: T::Item,
    ) {
        insert(s, expected, v).unwrap();
        compare(s, expected);
    }
}
