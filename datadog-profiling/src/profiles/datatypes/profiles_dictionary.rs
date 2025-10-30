// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::api2::{Function2, FunctionId2, Mapping2, MappingId2, StringId2};
use crate::profiles::collections::{ParallelSet, ParallelStringSet, SetId};
use crate::profiles::datatypes::{Function, FunctionSet, Mapping, MappingSet};
use crate::profiles::ProfileError;

pub struct ProfilesDictionary {
    functions: FunctionSet,
    mappings: MappingSet,
    strings: ParallelStringSet,
}

impl ProfilesDictionary {
    pub fn try_new() -> Result<ProfilesDictionary, ProfileError> {
        let dictionary = ProfilesDictionary {
            functions: ParallelSet::try_new()?,
            mappings: ParallelSet::try_new()?,
            strings: ParallelStringSet::try_new()?,
        };
        dictionary.mappings.try_insert(Mapping::default())?;
        dictionary.functions.try_insert(Function::default())?;
        Ok(dictionary)
    }

    pub fn functions(&self) -> &FunctionSet {
        &self.functions
    }

    pub fn mappings(&self) -> &MappingSet {
        &self.mappings
    }

    pub fn strings(&self) -> &ParallelStringSet {
        &self.strings
    }

    pub fn try_insert_function2(&self, function: Function2) -> Result<FunctionId2, ProfileError> {
        let function = Function {
            name: function.name.into(),
            system_name: function.system_name.into(),
            file_name: function.file_name.into(),
        };
        let set_id = self.functions.try_insert(function)?;
        let function_id = unsafe { core::mem::transmute::<SetId<Function>, FunctionId2>(set_id) };
        Ok(function_id)
    }

    pub fn try_insert_mapping2(&self, mapping: Mapping2) -> Result<MappingId2, ProfileError> {
        let mapping = Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename: mapping.filename.into(),
            build_id: mapping.build_id.into(),
        };
        let set_id = self.mappings.try_insert(mapping)?;
        let mapping_id = unsafe { core::mem::transmute::<SetId<Mapping>, MappingId2>(set_id) };
        Ok(mapping_id)
    }

    pub fn try_insert_str2(&self, str: &str) -> Result<StringId2, ProfileError> {
        let string_ref = self.strings.try_insert(str)?;
        Ok(string_ref.into())
    }
}
