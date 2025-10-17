// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelSet, ParallelStringSet};
use crate::profiles::datatypes::{Function2, FunctionSet, Mapping2, MappingSet};
use crate::profiles::ProfileError;

pub struct ProfilesDictionary2 {
    functions: FunctionSet,
    mappings: MappingSet,
    strings: ParallelStringSet,
}

impl ProfilesDictionary2 {
    pub fn try_new() -> Result<ProfilesDictionary2, ProfileError> {
        let dictionary = ProfilesDictionary2 {
            functions: ParallelSet::try_new()?,
            mappings: ParallelSet::try_new()?,
            strings: ParallelStringSet::try_new()?,
        };
        dictionary.mappings.try_insert(Mapping2::default())?;
        dictionary.functions.try_insert(Function2::default())?;
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
}
