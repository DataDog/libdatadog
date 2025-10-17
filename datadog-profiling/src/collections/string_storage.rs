// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
use std::cell::Cell;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::num::NonZeroU32;
use std::rc::Rc;

use super::identifiable::StringId;
use super::string_table::StringTable;

#[derive(PartialEq, Debug)]
struct ManagedStringData {
    str: Rc<str>,
    cached_seq_num: Cell<Option<(InternalCachedProfileId, StringId)>>,
    usage_count: Cell<u32>,
}

pub struct ManagedStringStorage {
    next_id: u32,
    id_to_data: HashMap<u32, ManagedStringData, BuildHasherDefault<rustc_hash::FxHasher>>,
    str_to_id: HashMap<Rc<str>, u32, BuildHasherDefault<rustc_hash::FxHasher>>,
    current_gen: u32,
    next_cached_profile_id: InternalCachedProfileId,
}

#[derive(PartialEq, Debug)]
// The `ManagedStringStorage::get_seq_num` operation is used to map a `ManagedStorageId` into a
// `StringId` for a given `StringTable`. As an optimization, we store a one-element `cached_seq_num`
//  inline cache with each `ManagedStringData` entry, so that repeatedly calling
// `get_seq_num` with the same id provides faster lookups.
//
// Because:
// 1. Multiple profiles can be using the same `ManagedStringTable`
// 2. The same profile resets its `StringTable` on serialization and starts anew
// ...we need a way to identify when the cache can and cannot be reused.
//
// This is where the `CachedProfileId` comes in. A given `CachedProfileId` should be considered
// as representing a unique `StringTable`. Different `StringTable`s should have different
// `CachedProfileId`s, and when a `StringTable` gets flushed and starts anew, it should also have a
// different `CachedProfileId`.
//
// **This struct is on purpose not Copy and not Clone to try to make it really hard to accidentally
// reuse** when a profile gets reset.
pub struct CachedProfileId {
    id: u32,
}

#[derive(PartialEq, Debug, Copy, Clone)]
struct InternalCachedProfileId {
    id: u32,
}

// Enable Mutex<ManagedStringStorage> to be Send
//
// SAFETY: ManagedStringStorage **must always** be wrapped with a Mutex -- you can't pass one in to
// a Profile without it. This is because it is not, by itself, thread-safe, and its real-world use
// cases are expected to include concurrency.
unsafe impl Send for ManagedStringStorage {}

impl From<&CachedProfileId> for InternalCachedProfileId {
    fn from(cached: &CachedProfileId) -> Self {
        InternalCachedProfileId { id: cached.id }
    }
}

impl ManagedStringStorage {
    pub fn new() -> Self {
        let mut storage = ManagedStringStorage {
            next_id: 0,
            id_to_data: Default::default(),
            str_to_id: Default::default(),
            current_gen: 0,
            next_cached_profile_id: InternalCachedProfileId { id: 0 },
        };
        // Ensure empty string gets id 0 and always has usage > 0 so it's always retained
        // Safety: On an empty managed string table intern should never fail.
        #[allow(clippy::expect_used)]
        storage.intern_new("").expect("Initialization to succeed");
        storage
    }

    pub fn next_cached_profile_id(&mut self) -> anyhow::Result<CachedProfileId> {
        let next_id = self.next_cached_profile_id.id;
        self.next_cached_profile_id = InternalCachedProfileId {
            id: next_id
                .checked_add(1)
                .context("Ran out of cached_profile_ids!")?,
        };
        Ok(CachedProfileId { id: next_id })
    }

    pub fn advance_gen(&mut self) {
        self.id_to_data.retain(|_, data| {
            let retain = data.usage_count.get() > 0;
            if !retain {
                self.str_to_id.remove_entry(&data.str);
            }
            retain
        });
        self.current_gen += 1;
    }

    pub fn intern(&mut self, item: &str) -> anyhow::Result<u32> {
        if item.is_empty() {
            // We don't increase ref-counts on the empty string
            return Ok(0);
        }

        let entry = self.str_to_id.get_key_value(item);
        match entry {
            Some((_, id)) => {
                let usage_count = &self
                    .id_to_data
                    .get(id)
                    .context("BUG: id_to_data and str_to_id should be in sync")?
                    .usage_count;
                usage_count.set(
                    usage_count
                        .get()
                        .checked_add(1)
                        .context("Usage_count overflow")?,
                );
                Ok(*id)
            }
            None => self.intern_new(item),
        }
    }

    fn intern_new(&mut self, item: &str) -> anyhow::Result<u32> {
        let id = self.next_id;
        let str: Rc<str> = item.into();
        let data = ManagedStringData {
            str: str.clone(),
            cached_seq_num: Cell::new(None),
            usage_count: Cell::new(1),
        };
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("Ran out of string ids!")?;
        let old_value = self.str_to_id.insert(str.clone(), id);
        debug_assert_eq!(old_value, None);
        let old_value = self.id_to_data.insert(id, data);
        debug_assert_eq!(old_value, None);
        Ok(id)
    }

    // Here id is a NonZeroU32 because an id of 0 is the empty string and that can never be
    // uninterned (and it should be skipped instead in the caller)
    pub fn unintern(&mut self, id: NonZeroU32) -> anyhow::Result<()> {
        let data = self.get_data(id.into())?;
        let usage_count = &data.usage_count;
        usage_count.set(
            usage_count
                .get()
                .checked_sub(1)
                .context("Usage_count underflow")?,
        );
        Ok(())
    }

    // Here id is a NonZeroU32 because an id of 0 which StringTable always maps to 0 as well so this
    // entire call can be skipped
    // See comment on `struct CachedProfileId` for details on how to use it.
    pub fn get_seq_num(
        &mut self,
        id: NonZeroU32,
        profile_strings: &mut StringTable,
        cached_profile_id: &CachedProfileId,
    ) -> anyhow::Result<StringId> {
        let data = self.get_data(id.into())?;

        match data.cached_seq_num.get() {
            Some((profile_id, seq_num)) if profile_id.id == cached_profile_id.id => Ok(seq_num),
            _ => {
                let seq_num = profile_strings.try_intern(data.str.as_ref())?;
                data.cached_seq_num
                    .set(Some((cached_profile_id.into(), seq_num)));
                Ok(seq_num)
            }
        }
    }

    pub fn get_string(&self, id: u32) -> anyhow::Result<Rc<str>> {
        let data = self.get_data(id)?;

        Ok(data.str.clone())
    }

    fn get_data(&self, id: u32) -> anyhow::Result<&ManagedStringData> {
        match self.id_to_data.get(&id) {
            Some(v) => {
                if v.usage_count.get() > 0 {
                    Ok(v)
                } else {
                    Err(anyhow::anyhow!(
                        "Tried to read data for id {} ('{}') but usage count was zero",
                        id,
                        v.str
                    ))
                }
            }
            None => Err(anyhow::anyhow!("ManagedStringId {} is not valid", id)),
        }
    }
}

impl Default for ManagedStringStorage {
    fn default() -> Self {
        Self::new()
    }
}
