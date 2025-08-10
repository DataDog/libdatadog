// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::ProfileId;
use datadog_profiling_protobuf::{Function, Line, Location, Mapping, StringOffset};
use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::mem::offset_of;
use std::ops::{Deref, Range};

/// A trait used to split a protobuf type into its id and the remainder of its
/// data.
///
/// # Safety
///
/// The default implementation must return 0 for the id field.
///
/// The implementing type must not have any padding bytes in the range given
/// by [`Self::RANGE_WITHOUT_ID`]. Leading or trailing padding bytes on the
/// struct are fine, they just can't exist in the given range.
///
/// ID_OFFSET should probably be 0, but as long as it's within the object and
/// the offset puts it at a properly aligned pointer for u64, it's fine.
pub unsafe trait Storable: core::fmt::Debug + Clone + Default + Sized {
    const ID_OFFSET: usize;

    const RANGE_WITHOUT_ID: Range<usize>;
}

/// # Safety
/// Mapping does not have any padding at all.
unsafe impl Storable for Mapping {
    const ID_OFFSET: usize = offset_of!(Mapping, id);

    const RANGE_WITHOUT_ID: Range<usize> = {
        let start: usize = offset_of!(Mapping, memory_start);
        let end: usize = offset_of!(Mapping, build_id) + size_of::<StringOffset>();
        Range { start, end }
    };
}

/// # Safety
/// Location does not have any padding at all.
unsafe impl Storable for Location {
    const ID_OFFSET: usize = offset_of!(Location, id);
    const RANGE_WITHOUT_ID: Range<usize> = {
        let start = offset_of!(Location, mapping_id);
        let end = offset_of!(Location, line) + size_of::<Line>();
        Range { start, end }
    };
}

/// # Safety
/// Function does not have any interior padding. It does have trailing padding,
/// which doesn't matter.
unsafe impl Storable for Function {
    const ID_OFFSET: usize = offset_of!(Function, id);
    const RANGE_WITHOUT_ID: Range<usize> = {
        let start = offset_of!(Function, name);
        let end = offset_of!(Function, filename) + size_of::<StringOffset>();
        Range { start, end }
    };
}

/// A wrapper around Storable data, which implements [`Eq`] and [`Hash`] based
/// on the byte range that excludes the id field
#[repr(transparent)]
#[derive(Debug)]
struct Stored<T: Storable>(T);

impl<T: Storable> Hash for Stored<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.borrow().hash(state)
    }
}

impl<T: Storable> PartialEq for Stored<T> {
    fn eq(&self, other: &Self) -> bool {
        self.borrow() == other.borrow()
    }
}

impl<T: Storable> Eq for Stored<T> {}

impl<T: Storable> Stored<T> {
    /// # Safety
    /// Any constraints on value ranges have been met already.
    unsafe fn new(data: T) -> Self {
        Stored(data)
    }

    fn borrow(&self) -> Borrowed<T> {
        Borrowed(&self.0)
    }

    fn id(&self) -> ProfileId {
        let raw_id = *self.borrow().id();
        // SAFETY: Guaranteed to fit, because it's already been stored.
        unsafe { ProfileId::new_unchecked(raw_id as u32) }
    }
}

/// A wrapper around Storable data, which implements [`Eq`] and [`Hash`] based
/// on the byte range that excludes the id field.
#[repr(transparent)]
#[derive(Debug)]
struct Borrowed<'a, T: Storable + 'a>(&'a T);

impl<'a, T: Storable + 'a> Borrowed<'a, T> {
    #[inline]
    fn addr(&self) -> *const u8 {
        (self.0 as *const T).cast()
    }

    #[inline]
    fn id(&self) -> &u64 {
        // SAFETY: safe as long as Storable's safety is adhered to.
        unsafe { &*self.addr().add(T::ID_OFFSET).cast::<u64>() }
    }
}

impl<T: Storable> Deref for Borrowed<'_, T> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        let range = T::RANGE_WITHOUT_ID;
        // SAFETY: safe as long as Storable's safety is adhered to.
        let ptr = unsafe { self.addr().add(range.start) };
        let len = range.end - range.start;
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

impl<'a, T: Storable + 'a> PartialEq for Borrowed<'a, T> {
    fn eq(&self, other: &Self) -> bool {
        self.deref() == other.deref()
    }
}

impl<'a, T: Storable + 'a> Hash for Borrowed<'a, T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.deref().hash(state)
    }
}

impl<'a, T: Storable + 'a> Eq for Borrowed<'a, T> {}

fn id_mut<T: Storable>(storable: &mut T) -> &mut u64 {
    let addr = storable as *mut T;
    unsafe { &mut *addr.add(T::ID_OFFSET).cast::<u64>() }
}

#[derive(Debug)]
pub struct Store<T: Storable> {
    map: HashSet<Stored<T>, BuildHasherDefault<rustc_hash::FxHasher>>,
}

// todo: remove this requirement somehow so we can avoid unwrap
impl<T: Storable> Default for Store<T> {
    #[allow(clippy::expect_used)]
    fn default() -> Self {
        Self::try_new().expect("unexpectedly failed to create store")
    }
}

impl<T: Storable> Store<T> {
    pub fn try_new() -> Result<Self, std::collections::TryReserveError> {
        let mut map = HashSet::with_hasher(BuildHasherDefault::default());
        map.try_reserve(1)?;
        // SAFETY: Storable guarantees that T::default() has an ID of 0.
        map.insert(unsafe { Stored::new(T::default()) });
        Ok(Store { map })
    }

    pub fn insert(&mut self, mut data: T) -> Result<ProfileId, T> {
        let stored = {
            // next_offset will not be zero, because the T::default() is
            // inserted into the Store at initialization.
            let next_offset = self.map.len() as u64;
            // OTEL requires them to fit in i32.
            if self.map.len() > i32::MAX as u32 as usize {
                return Err(data);
            }

            *id_mut(&mut data) = next_offset;
            // SAFETY: did the range checks above.
            unsafe { Stored::new(data) }
        };
        let id = stored.id();

        // If the item already exists, we return the existing id. It _could_
        // be that the id is 0, if someone re-inserts T::default().
        if let Some(prev) = self.map.get(&stored) {
            return Ok(prev.id());
        }

        if self.map.try_reserve(1).is_err() {
            return Err(stored.0);
        }

        self.map.insert(stored);
        Ok(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.map.iter().map(|s| &s.0)
    }

    pub fn iter_without_default(&self) -> impl Iterator<Item = &T> {
        self.map
            .iter()
            .filter(|s| s.id() != ProfileId::ZERO)
            .map(|s| &s.0)
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::{BuildHasher, RandomState};

    /// Property: if you change the ID of a Borrowed type, it still compares
    /// equal and has the same hash.
    fn prop_eq<T: Storable>(original: Borrowed<T>, distance: u64) {
        let mut modified = original.0.clone();
        let base = core::ptr::addr_of_mut!(modified);
        unsafe {
            let ptr = base.cast::<u8>().add(T::ID_OFFSET);
            ptr.cast::<u64>()
                .write(original.id().wrapping_add(distance));
        }

        let builder = RandomState::new();
        let modified = Borrowed(&modified);
        assert_eq!(original, modified);

        let hash1 = builder.hash_one(original);
        let hash2 = builder.hash_one(modified);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn prop_eq_mappings() {
        bolero::check!()
            .with_type::<(Mapping, u64)>()
            .for_each(|(data, distance)| prop_eq(Borrowed(data), *distance));
    }

    #[test]
    fn prop_eq_locations() {
        bolero::check!()
            .with_type::<(Location, u64)>()
            .for_each(|(data, distance)| prop_eq(Borrowed(data), *distance));
    }

    #[test]
    fn prop_eq_functions() {
        bolero::check!()
            .with_type::<(Function, u64)>()
            .for_each(|(data, distance)| prop_eq(Borrowed(data), *distance));
    }
}
