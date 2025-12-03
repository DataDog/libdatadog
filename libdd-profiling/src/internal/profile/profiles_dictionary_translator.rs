// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::collections::identifiable::{Dedup, FxIndexMap, StringId};
use crate::collections::string_table::StringTable;
use crate::internal::{Function, FunctionId, Mapping, MappingId};
use crate::profiles::collections::{SetId, StringRef};
use crate::profiles::datatypes::{self as dt, FunctionId2, MappingId2, ProfilesDictionary};
use anyhow::Context;
use indexmap::map::Entry;
use std::ptr::NonNull;

pub struct ProfilesDictionaryTranslator {
    pub profiles_dictionary: crate::profiles::collections::Arc<ProfilesDictionary>,
    pub mappings: FxIndexMap<SetId<dt::Mapping>, MappingId>,
    pub functions: FxIndexMap<Option<SetId<dt::Function>>, FunctionId>,
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
        let (set_id, function) = match NonNull::new(id2.0) {
            // Since the internal model treats functions as required, we
            // translate the null FunctionId2 to the default function.
            None => {
                let function = Function {
                    name: StringId::ZERO,
                    system_name: StringId::ZERO,
                    filename: StringId::ZERO,
                };
                (None, function)
            }
            Some(nn) => {
                let set_id = SetId(nn.cast::<dt::Function>());
                if let Some(internal) = self.functions.get(&Some(set_id)) {
                    return Ok(*internal);
                }

                // SAFETY: todo
                let function = unsafe { *self.profiles_dictionary.functions().get(set_id) };
                let function = Function {
                    name: self.translate_string(string_table, function.name)?,
                    system_name: self.translate_string(string_table, function.system_name)?,
                    filename: self.translate_string(string_table, function.file_name)?,
                };
                (Some(set_id), function)
            }
        };

        let internal_id = functions
            .try_dedup(function)
            .context("failed to deduplicate function in ProfilesDictionaryTranslator")?;
        self.functions.try_reserve(1).context(
            "failed to reserve memory for a new function in ProfilesDictionaryTranslator",
        )?;
        self.functions.insert(set_id, internal_id);
        Ok(internal_id)
    }

    pub fn translate_mapping(
        &mut self,
        mappings: &mut impl Dedup<Mapping>,
        string_table: &mut StringTable,
        id2: MappingId2,
    ) -> anyhow::Result<Option<MappingId>> {
        // Translate null MappingId2 to Ok(None). This is different from
        // functions because the internal module uses Option<MappingId>,
        // whereas it assumes functions are required.
        let Some(nn) = NonNull::new(id2.0) else {
            return Ok(None);
        };
        let set_id = SetId(nn.cast::<dt::Mapping>());
        if let Some(internal) = self.mappings.get(&set_id) {
            return Ok(Some(*internal));
        }

        // SAFETY: todo
        let mapping = unsafe { *self.profiles_dictionary.mappings().get(set_id) };
        let internal = Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename: self.translate_string(string_table, mapping.filename)?,
            build_id: self.translate_string(string_table, mapping.build_id)?,
        };
        let internal_id = mappings
            .try_dedup(internal)
            .context("failed to deduplicate mapping in ProfilesDictionaryTranslator")?;
        self.mappings.try_reserve(1).context(
            "failed to reserve memory for a new mapping in ProfilesDictionaryTranslator",
        )?;
        self.mappings.insert(set_id, internal_id);
        Ok(Some(internal_id))
    }

    pub fn translate_string(
        &mut self,
        string_table: &mut StringTable,
        str_ref: StringRef,
    ) -> anyhow::Result<StringId> {
        self.strings.try_reserve(1)?;
        match self.strings.entry(str_ref) {
            Entry::Occupied(o) => Ok(*o.get()),
            Entry::Vacant(v) => {
                let str = unsafe { self.profiles_dictionary.strings().get(str_ref) };
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
