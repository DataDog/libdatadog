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

/// Translates IDs from a [`ProfilesDictionary`] into the IDs used by the
/// current Profile's internal collections.
///
/// # Safety
///
/// All IDs passed to the translate methods (translate_function,
/// translate_mapping, translate_string) MUST have been created by the same
/// ProfilesDictionary that this translator wraps.
pub struct ProfilesDictionaryTranslator {
    pub profiles_dictionary: crate::profiles::collections::Arc<ProfilesDictionary>,
    pub mappings: FxIndexMap<SetId<dt::Mapping>, MappingId>,
    pub functions: FxIndexMap<Option<SetId<dt::Function>>, FunctionId>,
    pub strings: FxIndexMap<StringRef, StringId>,
}

// SAFETY: ProfilesDictionaryTranslator is Send because:
// 1. The profiles_dictionary Arc ensures the underlying storage remains alive and valid for the
//    lifetime of this translator, and Arc<T> is Send when T is Send + Sync. ProfilesDictionary is
//    Send + Sync.
// 2. SetId<T> and StringRef are non-owning handles (thin pointers) to immutable data in the
//    ProfilesDictionary's concurrent collections, which use arena allocation with stable addresses.
//    The Arc protects this data, making the pointers safe to send across threads.
// 3. FxIndexMap<K, V> is Send when K and V are Send. The keys (SetId, StringRef) and values
//    (MappingId, FunctionId, StringId) are all Copy types that are Send.
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

    /// Translates a FunctionId2 from the ProfilesDictionary into a FunctionId
    /// for this profile's StringTable.
    ///
    /// # Safety
    ///
    /// The `id2` must have been created by `self.profiles_dictionary`, and
    /// the strings must also live in the same dictionary.
    pub unsafe fn translate_function(
        &mut self,
        functions: &mut impl Dedup<Function>,
        string_table: &mut StringTable,
        id2: FunctionId2,
    ) -> anyhow::Result<FunctionId> {
        let (set_id, function) = match NonNull::new(id2.0) {
            // Since the internal model treats functions as required, we
            // translate the null FunctionId2 to the default function.
            None => (None, Function::default()),
            Some(nn) => {
                let set_id = SetId(nn.cast::<dt::Function>());
                if let Some(internal) = self.functions.get(&Some(set_id)) {
                    return Ok(*internal);
                }

                // SAFETY: This is safe if `id2` (the FunctionId2) was created by
                // `self.profiles_dictionary`, which is a precondition of calling
                // this method.
                let function = unsafe { *self.profiles_dictionary.functions().get(set_id) };
                // SAFETY: safe if the strings were made by
                // `self.profiles_dictionary`, which is a precondition of
                // calling this method.
                let function = unsafe {
                    Function {
                        name: self.translate_string(string_table, function.name)?,
                        system_name: self.translate_string(string_table, function.system_name)?,
                        filename: self.translate_string(string_table, function.file_name)?,
                    }
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

    /// Translates a MappingId2 from the ProfilesDictionary into a MappingId
    /// for this profile's internal collections.
    ///
    /// # Safety
    ///
    /// The `id2` must have been created by `self.profiles_dictionary`, and
    /// the strings must also live in the same dictionary.
    pub unsafe fn translate_mapping(
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

        // SAFETY: This is safe if `id2` (the MappingId2) was created by
        // `self.profiles_dictionary`, which is a precondition of calling
        // this method.
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

    /// Translates a StringRef from the ProfilesDictionary into a StringId
    /// for this profile's internal string table.
    ///
    /// # Safety
    ///
    /// The `str_ref` must have been created by `self.profiles_dictionary`.
    /// Violating this precondition results in undefined behavior.
    pub unsafe fn translate_string(
        &mut self,
        string_table: &mut StringTable,
        str_ref: StringRef,
    ) -> anyhow::Result<StringId> {
        self.strings.try_reserve(1)?;
        match self.strings.entry(str_ref) {
            Entry::Occupied(o) => Ok(*o.get()),
            Entry::Vacant(v) => {
                // SAFETY: This is safe if `str_ref` was created by
                // `self.profiles_dictionary`, which is a precondition of calling
                // this method.
                let str = unsafe { self.profiles_dictionary.strings().get(str_ref) };
                let internal_id = string_table.try_intern(str)?;
                v.insert(internal_id);
                Ok(internal_id)
            }
        }
    }
}
