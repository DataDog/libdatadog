// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

mod id;
mod storable;

use ahash::RandomState;
use bumpalo::Bump;
use std::collections::HashSet;
use std::ops::Index;

pub use id::*;
pub use storable::*;

pub struct ProfTable<'arena, T: Storable> {
    arena: &'arena Bump,
    vec: Vec<&'arena T>,
    set: HashSet<&'arena T, RandomState>,
}

impl<'arena, T: Storable> ProfTable<'arena, T> {
    /// The current number of elements held in the table.
    #[allow(unused)]
    pub fn len(&self) -> usize {
        self.vec.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vec.is_empty()
    }

    /// # Safety
    /// The arena must not be reset while the ProfTable exists!
    pub unsafe fn new(arena: &'arena Bump) -> Self {
        let empty_item = &*arena.alloc(T::default());

        let mut vec = Vec::new();
        vec.push(empty_item);

        let mut set = HashSet::with_hasher(Default::default());
        set.insert(empty_item);

        Self { arena, vec, set }
    }

    pub fn insert_full(&mut self, value: &T) -> (&'arena T, bool) {
        match self.set.get(value) {
            None => {
                // Clone the value and update the id.
                let mut cloned = value.clone();
                let id: Id = self.vec.len().into();
                cloned.set_id(id);

                // Move it into the arena and insert its reference to the vec and set.
                let arena_ref = &*self.arena.alloc(cloned);
                self.vec.push(arena_ref);
                self.set.insert(arena_ref);
                (arena_ref, true)
            }
            Some(value) => (value, false),
        }
    }

    #[allow(unused)]
    pub fn insert(&mut self, value: &T) -> &'arena T {
        self.insert_full(value).0
    }

    #[allow(unused)]
    pub fn get(&self, id: Id) -> &'arena T {
        let index: Id = id.into();
        let offset: usize = index.into();
        let r = self.vec[offset];
        assert_eq!(r.get_id(), id);
        r
    }
}

impl<'arena, T: Storable, Idx> Index<Idx> for ProfTable<'arena, T>
where
    Idx: std::slice::SliceIndex<[&'arena T]>,
{
    type Output = Idx::Output;

    fn index(&self, index: Idx) -> &Self::Output {
        &self.vec[index]
    }
}

#[cfg(test)]
mod tests {
    use super::super::pprof::Mapping;
    use super::*;

    #[test]
    pub fn basic() {
        let arena = Bump::new();

        // Safety: the arena is left alone as required.
        let mut table: ProfTable<Mapping> = unsafe { ProfTable::new(&arena) };
        let id = table[0].id;
        assert_eq!(0, id);

        let mapping = Mapping::default();
        let id = table.insert(&mapping).id;
        assert_eq!(0, id);
    }
}
