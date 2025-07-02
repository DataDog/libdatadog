// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::ProfileError;
use hashbrown::HashTable;
use rustc_hash::FxHasher;
use std::hash::{Hash, Hasher};

/// A half-open range, similar to the std Range except it's Copy and not an
/// iterator.
///
/// No modifying start/end!
#[derive(Copy, Clone, Debug)]
pub struct Range {
    pub(crate) start: u32,
    pub(crate) end: u32,
}

impl From<Range> for core::ops::Range<usize> {
    fn from(range: Range) -> Self {
        Self::from(&range)
    }
}

impl From<&Range> for core::ops::Range<usize> {
    fn from(range: &Range) -> Self {
        let start = range.start as usize;
        let end = range.end as usize;
        Self { start, end }
    }
}

#[derive(Default)]
pub struct SliceSet<T> {
    map: HashTable<Range>,
    arena: Vec<T>,
}

impl<T> SliceSet<T> {
    pub const fn new() -> Self {
        let map = HashTable::new();
        let arena = Vec::new();
        Self { map, arena }
    }

    pub fn lookup(&self, range: Range) -> Option<&[T]> {
        let range = core::ops::Range::<usize>::from(range);
        // Don't use get_unchecked here; methods like clear exist, so it could
        // be out-of-bounds even in safe Rust.
        self.arena.get(range)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.arena.clear();
    }
}

impl<T: Clone + Eq + Hash> SliceSet<T> {
    pub fn insert(&mut self, data: &[T]) -> Result<Range, ProfileError> {
        let Self { map, arena } = self;
        let hash = Self::hash(data);

        let eq = |range: &Range| -> bool {
            // SAFETY: when finding, the ranges come from internal values.
            unsafe { Self::project(arena, range) == data }
        };
        if let Some(range) = map.find(hash, eq) {
            return Ok(*range);
        }

        // Didn't find it, so we need to insert into the arena and map.
        // First, make sure the arena's len won't overflow u32.

        let Some(new_len) = arena.len().checked_add(data.len()) else {
            return Err(ProfileError::StorageFull);
        };
        if u32::try_from(new_len).is_err() {
            return Err(ProfileError::StorageFull);
        }

        // Second, reserve memory from both structs before adding to either.
        arena.try_reserve(data.len())?;
        map.try_reserve(1, |x| Self::rehash(arena, x))?;

        // Third, add data to both structures.
        let start = arena.len() as u32;
        arena.extend_from_slice(data);
        let end = arena.len() as u32;
        let range = Range { start, end };
        map.insert_unique(hash, range, |x| Self::rehash(arena, x));
        Ok(range)
    }

    /// # Safety
    /// The range must be in-bounds of the arena!
    #[inline]
    unsafe fn project<'a>(arena: &'a [T], range: &Range) -> &'a [T] {
        let range = core::ops::Range::<usize>::from(range);
        // SAFETY: required by project's safety conditions.
        arena.get_unchecked(range)
    }

    fn rehash(arena: &[T], range: &Range) -> u64 {
        // SAFETY: when rehashing, the ranges come from internal values.
        let data = unsafe { Self::project(arena, range) };
        Self::hash(data)
    }

    fn hash(data: &[T]) -> u64 {
        let mut hasher = FxHasher::default();
        data.hash(&mut hasher);
        hasher.finish()
    }
}

/// An opaque handle to a slice in a SliceSet.
/// cbindgen:field-names=[opaque]
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SliceId(u64);

impl From<Range> for SliceId {
    fn from(range: Range) -> Self {
        let lower = range.start as u64;
        let upper = (range.end as u64) << 32;
        Self(lower | upper)
    }
}

impl From<SliceId> for Range {
    fn from(id: SliceId) -> Self {
        let start = id.0 as u32;
        let end = (id.0 >> 32) as u32;
        Range { start, end }
    }
}

/// A result from calling a SliceSet routine such as
/// `ddog_prof_LabelsSet_insert`.
#[repr(C)]
pub enum SliceSetInsertResult {
    Ok(SliceId),
    Err(ProfileError),
}

impl From<Result<Range, ProfileError>> for SliceSetInsertResult {
    fn from(result: Result<Range, ProfileError>) -> Self {
        match result {
            Ok(ok) => SliceSetInsertResult::Ok(ok.into()),
            Err(err) => SliceSetInsertResult::Err(err),
        }
    }
}

impl From<SliceSetInsertResult> for Result<Range, ProfileError> {
    fn from(result: SliceSetInsertResult) -> Self {
        match result {
            SliceSetInsertResult::Ok(id) => Ok(id.into()),
            SliceSetInsertResult::Err(err) => Err(err),
        }
    }
}
