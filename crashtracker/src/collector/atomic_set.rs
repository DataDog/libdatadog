// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use portable_atomic::AtomicUsize;
use rand::Rng;
use std::fmt::Debug;
use std::io::Write;
use std::num::NonZeroU128;
use std::ptr::null_mut;
use std::sync::atomic::Ordering::SeqCst;

pub(crate) type AtomicSpanSet<const LEN: usize> = AtomicSet<AtomicSpan, LEN>;
pub(crate) type AtomicStringSet<const LEN: usize> = AtomicSet<AtomicString, LEN>;

pub trait Atomic {
    type Item: Ord + PartialEq + Debug + Clone;
    const NONE: Self;
    /// Returns whether there was a value before
    fn clear(&self) -> bool {
        self.take().is_some()
    }
    /// Returns whether there was anything to emit.
    fn consume_and_emit(&self, w: &mut impl Write, leak: bool, first: bool)
        -> anyhow::Result<bool>;
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

pub struct AtomicSet<T, const LEN: usize> {
    used: AtomicUsize,
    set: [T; LEN],
}

impl<T, const LEN: usize> AtomicSet<T, LEN>
where
    T: Atomic,
    <T as Atomic>::Item: std::cmp::PartialEq + Debug + Ord,
{
    /// Atomicity: This is NOT ATOMIC.  If other code modifies the set while this is happening,
    /// badness will occur.
    pub fn clear(&self) -> anyhow::Result<()> {
        if !self.is_empty() {
            for v in self.set.iter() {
                if v.clear() {
                    self.used.sub(1, SeqCst)
                }
            }
        }
        Ok(())
    }

    pub fn remove(&self, idx: usize) -> anyhow::Result<()> {
        anyhow::ensure!(idx < self.set.len(), "Idx {idx} out of range");
        if self.set[idx].clear() {
            self.used.sub(1, SeqCst)
        }
        Ok(())
    }

    pub const fn new() -> Self {
        Self {
            used: AtomicUsize::new(0),
            set: [T::NONE; LEN],
        }
    }

    pub fn insert(&self, mut value: T::Item) -> anyhow::Result<usize> {
        let used = self.used.fetch_add(1, SeqCst);
        if used >= self.set.len() / 2 {
            // We only fill to half full to get good amortized behaviour
            self.used.fetch_sub(1, SeqCst);
            anyhow::bail!("Crashtracker Atomic Set: No space to store {:?}", &value);
        }

        // Start at a random position.
        // Since the array is only at most half full, and since we start scanning at random
        // indicies, every slot should independently have <.5 probability of being occupied.
        // Long scans become exponentially unlikely, giving amortized constant time insertion.
        let shift: usize = rand::thread_rng().gen_range(0..self.set.len());
        for i in 0..self.set.len() {
            let idx = (i + shift) % self.set.len();

            if let Some(v) = self.set[idx].try_insert(value) {
                value = v;
            } else {
                return Ok(idx);
            }
        }
        anyhow::bail!("This should be unreachable: we ensure that there was at least one empty slot before entering the loop")
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        self.used.load(SeqCst)
    }

    pub fn consume_and_emit(&self, w: &mut impl Write, leak: bool) -> anyhow::Result<()> {
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
    pub fn values(&self) -> anyhow::Result<Vec<T::Item>> {
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
    ) -> anyhow::Result<bool> {
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
    ) -> anyhow::Result<bool> {
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
        let s: AtomicStringSet<16> = AtomicStringSet::new();
        assert_eq!(s.len(), 0);
        assert!(s.values()?.is_empty());
        Ok(())
    }

    #[test]
    fn test_string_ops() -> anyhow::Result<()> {
        let mut expected = std::collections::BTreeMap::<String, usize>::new();
        let s = AtomicStringSet::<8>::new();
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
        let s: AtomicStringSet<8> = AtomicStringSet::new();
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
        s: &AtomicSet<T, 8>,
        expected: &mut std::collections::BTreeMap<T::Item, usize>,
        v: T::Item,
    ) {
        remove(s, expected, v).unwrap();
        compare(s, expected);
    }

    fn remove<T: Atomic>(
        s: &AtomicSet<T, 8>,
        expected: &mut std::collections::BTreeMap<T::Item, usize>,
        v: T::Item,
    ) -> anyhow::Result<()> {
        let idx = expected.get(&v).unwrap();
        s.remove(*idx).unwrap();
        expected.remove(&v);
        Ok(())
    }

    fn compare<T: Atomic>(
        s: &AtomicSet<T, 8>,
        expected: &std::collections::BTreeMap<T::Item, usize>,
    ) {
        let actual = s.values().unwrap();
        let golden: Vec<_> = expected.keys().cloned().collect();
        assert_eq!(actual, golden);
        assert_eq!(expected.len(), s.len());
    }

    fn insert<T: Atomic>(
        s: &AtomicSet<T, 8>,
        expected: &mut std::collections::BTreeMap<T::Item, usize>,
        v: T::Item,
    ) -> anyhow::Result<()> {
        expected.insert(v.clone(), s.insert(v)?);
        Ok(())
    }

    fn insert_and_compare<T: Atomic>(
        s: &AtomicSet<T, 8>,
        expected: &mut std::collections::BTreeMap<T::Item, usize>,
        v: T::Item,
    ) {
        insert(s, expected, v).unwrap();
        compare(s, expected);
    }
}
