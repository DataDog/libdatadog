// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelSet, ParallelStringSet, SetError, SetId};
use crate::profiles::datatypes::{
    Function, Function2, FunctionId2, Mapping, Mapping2, MappingId2, StringId2,
};

pub type FunctionSet = ParallelSet<Function, 4>;
pub type MappingSet = ParallelSet<Mapping, 2>;

/// `ProfilesDictionary` contains data which are common to multiple profiles,
/// whether that's multiple profiles simultaneously or multiple profiles
/// through time.
///
/// The current implementation is thread-safe, though there has been some
/// discussion about making that optional, as some libraries will call these
/// APIs in places where a mutex is already employed.
pub struct ProfilesDictionary {
    functions: FunctionSet,
    mappings: MappingSet,
    strings: ParallelStringSet,
}

impl ProfilesDictionary {
    /// Creates a new dictionary, returning an error if it cannot allocate
    /// memory for one of its member sets.
    pub fn try_new() -> Result<ProfilesDictionary, SetError> {
        let dictionary = ProfilesDictionary {
            functions: ParallelSet::try_new()?,
            mappings: ParallelSet::try_new()?,
            strings: ParallelStringSet::try_new()?,
        };
        dictionary.mappings.try_insert(Mapping::default())?;
        dictionary.functions.try_insert(Function::default())?;
        Ok(dictionary)
    }

    /// Adds the function to the function set in the dictionary, and returns a
    /// `FunctionId2` which represents it. Returns an error if it cannot
    /// allocate memory.
    pub fn try_insert_function2(&self, function: Function2) -> Result<FunctionId2, SetError> {
        let function = Function {
            name: function.name.into(),
            system_name: function.system_name.into(),
            file_name: function.file_name.into(),
        };
        let set_id = self.functions.try_insert(function)?;
        Ok(FunctionId2::from(set_id))
    }

    /// Adds the mapping to the mapping set in the dictionary, and returns a
    /// `MappingId2` which represents it. Returns an error if it cannot
    /// allocate memory.
    pub fn try_insert_mapping2(&self, mapping: Mapping2) -> Result<MappingId2, SetError> {
        let mapping = Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename: mapping.filename.into(),
            build_id: mapping.build_id.into(),
        };
        let set_id = self.mappings.try_insert(mapping)?;
        Ok(MappingId2::from(set_id))
    }

    /// Adds the string to the string set in the dictionary, and returns a
    /// `StringId2` which represents it. Returns an error if it cannot
    /// allocate memory.
    pub fn try_insert_str2(&self, str: &str) -> Result<StringId2, SetError> {
        // Choosing not to add a fast-path for the empty string. It falls out
        // naturally, and if the caller feels it's useful for their use-case,
        // then they can always wrap this to do it.
        let string_ref = self.strings.try_insert(str)?;
        Ok(string_ref.into())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::collections::StringRef;
    use proptest::prelude::*;

    #[track_caller]
    fn assert_string_id_eq(a: StringId2, b: StringId2) {
        assert_eq!(StringRef::from(a), StringRef::from(b));
    }

    fn assert_string_value(dict: &ProfilesDictionary, id: StringId2, expected: &str) {
        unsafe {
            assert_eq!(dict.strings().get(id.into()), expected);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: if cfg!(miri) { 8 } else { 64 },
            ..ProptestConfig::default()
        })]

        #[test]
        fn proptest_function_round_trip_and_deduplication(
            name in ".*",
            system_name in ".*",
            file_name in ".*",
        ) {
            let dict = ProfilesDictionary::try_new().unwrap();

            // Insert strings
            let name_id = dict.try_insert_str2(&name).unwrap();
            let system_name_id = dict.try_insert_str2(&system_name).unwrap();
            let file_name_id = dict.try_insert_str2(&file_name).unwrap();

            let function2 = Function2 { name: name_id, system_name: system_name_id, file_name: file_name_id };

            // Test insert and read back (exercises unsafe transmute)
            let id1 = dict.try_insert_function2(function2).unwrap();
            prop_assert!(!id1.is_empty());

            let read1 = unsafe { id1.read() }.unwrap();
            assert_string_id_eq(read1.name, name_id);
            assert_string_id_eq(read1.system_name, system_name_id);
            assert_string_id_eq(read1.file_name, file_name_id);

            // Test deduplication
            let id2 = dict.try_insert_function2(function2).unwrap();
            prop_assert_eq!(id1.0, id2.0);

            // Test round-trip conversion
            let function: Function = read1.into();
            assert_string_value(&dict, function.name.into(), &name);
            assert_string_value(&dict, function.system_name.into(), &system_name);
            assert_string_value(&dict, function.file_name.into(), &file_name);
        }

        #[test]
        fn proptest_mapping_round_trip_and_deduplication(
            memory_start in 0..u64::MAX,
            memory_limit in 0..u64::MAX,
            file_offset in 0..u64::MAX,
            filename in ".*",
            build_id in ".*",
        ) {
            let dict = ProfilesDictionary::try_new().unwrap();

            let filename_id = dict.try_insert_str2(&filename).unwrap();
            let build_id_id = dict.try_insert_str2(&build_id).unwrap();

            let mapping2 = Mapping2 {
                memory_start,
                memory_limit,
                file_offset,
                filename: filename_id,
                build_id: build_id_id,
            };

            // Test insert and read back (exercises unsafe transmute)
            let id1 = dict.try_insert_mapping2(mapping2).unwrap();
            prop_assert!(!id1.is_empty());

            let read1 = unsafe { id1.read() }.unwrap();
            prop_assert_eq!(read1.memory_start, memory_start);
            prop_assert_eq!(read1.memory_limit, memory_limit);
            prop_assert_eq!(read1.file_offset, file_offset);
            assert_string_id_eq(read1.filename, filename_id);
            assert_string_id_eq(read1.build_id, build_id_id);

            // Test deduplication
            let id2 = dict.try_insert_mapping2(mapping2).unwrap();
            prop_assert_eq!(id1.0, id2.0);

            // Verify string values
            assert_string_value(&dict, read1.filename, &filename);
            assert_string_value(&dict, read1.build_id, &build_id);
        }
    }
}
