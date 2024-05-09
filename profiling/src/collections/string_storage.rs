use std::cell::Cell;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;
use std::hash::Hash;
use std::ptr;
use std::rc::Rc;

use super::identifiable::FxIndexSet;
use super::identifiable::Id;
use super::identifiable::StringId;

pub trait StringStorage {
    /// Interns the `str` as a string, returning the id in the string table.
    /// The empty string is guaranteed to have an id of [StringId::ZERO].
    fn intern(&mut self, item: Rc<str>) -> StringId;
    fn get_string(&self, id: StringId) -> Rc<str>;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn into_iter(self: Box<Self>) -> Box<dyn Iterator<Item = Rc<str>>>;
    fn clone_empty(&self) -> Box<dyn StringStorage>;
}

pub struct SimpleStringStorage {
    set: FxIndexSet<Rc<str>>,
}

impl SimpleStringStorage {
    pub fn new() -> Self {
        SimpleStringStorage {
            set: Default::default(),
        }
    }
}

impl Default for SimpleStringStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl StringStorage for SimpleStringStorage {
    fn intern(&mut self, item: Rc<str>) -> StringId {
        // For performance, delay converting the [&str] to a [String] until
        // after it has been determined to not exist in the set. This avoids
        // temporary allocations.
        let index = match self.set.get_index_of(&item) {
            Some(index) => index,
            None => {
                let (index, _inserted) = self.set.insert_full(item.clone());
                // This wouldn't make any sense; the item couldn't be found so
                // we try to insert it, but suddenly it exists now?
                debug_assert!(_inserted);
                index
            }
        };
        StringId::from_offset(index)
    }

    fn get_string(&self, id: StringId) -> Rc<str> {
        self.set
            .get_index(id.to_offset())
            .expect("StringId to have a valid interned index")
            .clone()
    }

    fn len(&self) -> usize {
        self.set.len()
    }

    fn into_iter(self: Box<Self>) -> Box<dyn Iterator<Item = Rc<str>>> {
        Box::new(self.set.into_iter())
    }

    fn clone_empty(&self) -> Box<dyn StringStorage> {
        Box::new(SimpleStringStorage::new())
    }
}

#[derive(PartialEq, Debug)]
struct ManagedStringData {
    str: Rc<str>,
    last_usage_gen: Cell<Option<u32>>,
    seq_num: Cell<Option<StringId>>,
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
            /*let used_in_this_gen = data
                .last_usage_gen
                .get()
                .map_or(false, |last_usage_gen| last_usage_gen == self.current_gen);
            if used_in_this_gen || *id == 0 {
                // Empty string (id = 0) or anything that was used in the gen
                // we are now closing, is kept alive
                return true;
            }*/
            if data.usage_count.get() > 0 {
                return true;
            }

            self.str_to_id.remove_entry(&data.str);
            false
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
                    last_usage_gen: Cell::new(None),
                    seq_num: Cell::new(None),
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

    pub fn unintern(&self, id: u32) {
        let (data, _) = self.get_data(id);
        let usage_count = &data.usage_count;
        usage_count.set(usage_count.get() - 1);
    }

    pub fn get_seq_num(&self, id: u32, profile_strings: &mut dyn StringStorage) -> StringId {
        let (data, already_accessed_in_this_gen) = self.get_data(id);

        let seq_num = if already_accessed_in_this_gen {
            data.seq_num.get()
        } else {
            None
        };

        match seq_num {
            Some(v) => v,
            None => {
                let seq_num = profile_strings.intern(data.str.clone());
                data.seq_num.set(Some(seq_num));
                seq_num
            }
        }
    }

    pub fn get_string(&self, id: u32) -> Rc<str> {
        let (data, _) = self.get_data(id);

        data.str.clone()
    }

    fn get_data(&self, id: u32) -> (&ManagedStringData, bool) {
        let data = match self.id_to_data.get(&id) {
            Some(v) => v,
            None => {
                println!("{:?} {:?}", id, self.id_to_data);
                panic!("StringId to have a valid interned id");
            }
        };
        let existing_gen = data.last_usage_gen.replace(Some(self.current_gen));
        let already_accessed_in_this_gen = if let Some(v) = existing_gen {
            v == self.current_gen
        } else {
            false
        };
        (data, already_accessed_in_this_gen)
    }
}

impl Default for ManagedStringStorage {
    fn default() -> Self {
        Self::new()
    }
}
