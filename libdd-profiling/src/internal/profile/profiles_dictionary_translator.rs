// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::{Dedup, FxIndexMap, StringId};
use crate::collections::string_table::StringTable;
use crate::internal::{Function, FunctionId, Mapping, MappingId};
use crate::profiles::collections::{SetId, StringRef};
use crate::profiles::datatypes::{
    self as dt, FunctionId2, MappingId2, ProfilesDictionary, StringId2,
};
use indexmap::map::Entry;

pub struct ProfilesDictionaryTranslator {
    pub profiles_dictionary: crate::profiles::collections::Arc<ProfilesDictionary>,
    pub mappings: FxIndexMap<SetId<dt::Mapping>, Option<MappingId>>,
    pub functions: FxIndexMap<SetId<dt::Function>, FunctionId>,
    pub strings: FxIndexMap<StringRef, StringId>,
}

// SAFETY: the profiles_dictionary keeps the storage for Ids alive.
unsafe impl Send for ProfilesDictionaryTranslator {}

impl ProfilesDictionaryTranslator {
    pub fn new(
        profiles_dictionary: crate::profiles::collections::Arc<ProfilesDictionary>,
    ) -> ProfilesDictionaryTranslator {
        ProfilesDictionaryTranslator {
            profiles_dictionary,
            mappings: Default::default(),
            functions: Default::default(),
            strings: Default::default(),
        }
    }

    pub fn translate_function(
        &mut self,
        functions: &mut impl Dedup<Function>,
        string_table: &mut StringTable,
        id2: FunctionId2,
    ) -> anyhow::Result<FunctionId> {
        let function2 = (unsafe { id2.read() }).unwrap_or_default();
        let set_id = unsafe { core::mem::transmute::<FunctionId2, SetId<dt::Function>>(id2) };

        if let Some(internal) = self.functions.get(&set_id) {
            return Ok(*internal);
        }

        let (name, system_name, filename) = (
            self.translate_string(string_table, function2.name)?,
            self.translate_string(string_table, function2.system_name)?,
            self.translate_string(string_table, function2.file_name)?,
        );
        let function = Function {
            name,
            system_name,
            filename,
        };
        let internal_id = functions.dedup(function);
        self.functions.try_reserve(1)?;
        self.functions.insert(set_id, internal_id);
        Ok(internal_id)
    }

    pub fn translate_mapping(
        &mut self,
        mappings: &mut impl Dedup<Mapping>,
        string_table: &mut StringTable,
        id2: MappingId2,
    ) -> anyhow::Result<Option<MappingId>> {
        let Some(mapping2) = (unsafe { id2.read() }) else {
            return Ok(None);
        };
        let set_id = unsafe { core::mem::transmute::<MappingId2, SetId<dt::Mapping>>(id2) };

        if let Some(internal) = self.mappings.get(&set_id) {
            return Ok(*internal);
        }

        let filename = self.translate_string(string_table, mapping2.filename)?;
        let build_id = self.translate_string(string_table, mapping2.build_id)?;
        let mapping = Mapping {
            memory_start: mapping2.memory_start,
            memory_limit: mapping2.memory_limit,
            file_offset: mapping2.file_offset,
            filename,
            build_id,
        };
        let internal_id = mappings.dedup(mapping);
        self.mappings.try_reserve(1)?;
        self.mappings.insert(set_id, Some(internal_id));
        Ok(Some(internal_id))
    }

    pub fn translate_string(
        &mut self,
        string_table: &mut StringTable,
        id2: StringId2,
    ) -> anyhow::Result<StringId> {
        if id2.is_empty() {
            return Ok(StringId::ZERO);
        }

        let string_ref = StringRef::from(id2);
        self.strings.try_reserve(1)?;
        match self.strings.entry(string_ref) {
            Entry::Occupied(o) => Ok(*o.get()),
            Entry::Vacant(v) => {
                let str = unsafe { self.profiles_dictionary.strings().get(string_ref) };
                // SAFETY: we're keeping these lifetimes in sync. I think.
                // TODO: BUT longer-term we want to avoid copying them
                //       entirely, so this should go away.
                let decouple_str = unsafe { core::mem::transmute::<&str, &str>(str) };
                let internal_id = string_table.try_intern(decouple_str)?;
                v.insert(internal_id);
                Ok(internal_id)
            }
        }
    }
}
