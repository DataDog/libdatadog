// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelSet, ParallelStringSet};
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
}
