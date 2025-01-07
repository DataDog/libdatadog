// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::cell::Cell;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::num::NonZeroU32;
use std::ptr;
use std::rc::Rc;

use super::identifiable::StringId;
use super::string_table::StringTable;

#[derive(PartialEq, Debug)]
struct ManagedStringData {
    str: Rc<str>,
    cached_seq_num: Cell<Option<(*const StringTable, StringId)>>,
    usage_count: Cell<u32>,
}

pub struct ManagedStringStorage {
    next_id: u32,
    id_to_data: HashMap<u32, ManagedStringData, BuildHasherDefault<rustc_hash::FxHasher>>,
    str_to_id: HashMap<Rc<str>, u32, BuildHasherDefault<rustc_hash::FxHasher>>,
    current_gen: u32,
}

impl ManagedStringStorage {
    pub fn new() -> Self {
        let mut storage = ManagedStringStorage {
            next_id: 0,
            id_to_data: Default::default(),
            str_to_id: Default::default(),
            current_gen: 0,
        };
        // Ensure empty string gets id 0 and always has usage > 0 so it's always retained
        storage.intern("");
        storage
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

    pub fn intern(&mut self, item: &str) -> u32 {
        let entry = self.str_to_id.get_key_value(item);
        match entry {
            Some((_, id)) => {
                let usage_count = &self
                    .id_to_data
                    .get(id)
                    .expect("id_to_data and str_to_id should be in sync")
                    .usage_count;
                usage_count.set(usage_count.get() + 1);
                *id
            }
            None => {
                let id = self.next_id;
                let str: Rc<str> = item.into();
                let data = ManagedStringData {
                    str: str.clone(),
                    cached_seq_num: Cell::new(None),
                    usage_count: Cell::new(1),
                };
                self.next_id = self.next_id.checked_add(1).expect("Ran out of string ids!");
                let old_value = self.str_to_id.insert(str.clone(), id);
                debug_assert_eq!(old_value, None);
                let old_value = self.id_to_data.insert(id, data);
                debug_assert_eq!(old_value, None);
                id
            }
        }
    }

    // Here id is a NonZeroU32 because an id of 0 is the empty string and that can never be
    // uninterned (and it should be skipped instead in the caller)
    pub fn unintern(&self, id: NonZeroU32) {
        let data = self.get_data(id.into());
        let usage_count = &data.usage_count;
        usage_count.set(usage_count.get() - 1);
    }

    // Here id is a NonZeroU32 because an id of 0 which StringTable always maps to 0 as well so this
    // entire call can be skipped
    pub fn get_seq_num(&self, id: NonZeroU32, profile_strings: &mut StringTable) -> StringId {
        let data = self.get_data(id.into());

        let profile_strings_pointer = ptr::addr_of!(*profile_strings);

        match data.cached_seq_num.get() {
            Some((pointer, seq_num)) if pointer == profile_strings_pointer => seq_num,
            _ => {
                let seq_num = profile_strings.intern(data.str.as_ref());
                data.cached_seq_num
                    .set(Some((profile_strings_pointer, seq_num)));
                seq_num
            }
        }
    }

    pub fn get_string(&self, id: u32) -> Rc<str> {
        let data = self.get_data(id);

        data.str.clone()
    }

    fn get_data(&self, id: u32) -> &ManagedStringData {
        let data = match self.id_to_data.get(&id) {
            Some(v) => v,
            None => {
                println!("{:?} {:?}", id, self.id_to_data);
                panic!("StringId to have a valid interned id");
            }
        };
        data
    }
}

impl Default for ManagedStringStorage {
    fn default() -> Self {
        Self::new()
    }
}
