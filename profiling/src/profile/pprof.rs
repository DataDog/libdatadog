// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::profile::EncodeError;
use derivative::Derivative;

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct Profile {
    #[prost(message, repeated, tag = "1")]
    pub sample_types: Vec<ValueType>,
    #[prost(message, repeated, tag = "2")]
    pub samples: Vec<Sample>,
    #[prost(message, repeated, tag = "3")]
    pub mappings: Vec<Mapping>,
    #[prost(message, repeated, tag = "4")]
    pub locations: Vec<Location>,
    #[prost(message, repeated, tag = "5")]
    pub functions: Vec<Function>,
    #[prost(string, repeated, tag = "6")]
    pub string_table: Vec<String>,
    #[prost(int64, tag = "7")]
    pub drop_frames: i64,
    #[prost(int64, tag = "8")]
    pub keep_frames: i64,
    #[prost(int64, tag = "9")]
    pub time_nanos: i64,
    #[prost(int64, tag = "10")]
    pub duration_nanos: i64,
    #[prost(message, optional, tag = "11")]
    pub period_type: Option<ValueType>,
    #[prost(int64, tag = "12")]
    pub period: i64,
    #[prost(int64, repeated, tag = "13")]
    pub comment: Vec<i64>,
    #[prost(int64, tag = "14")]
    pub default_sample_type: i64,
}

#[derive(Eq, Hash, PartialEq, ::prost::Message)]
pub struct ProfileSimpler {
    #[prost(string, repeated, tag = "6")]
    pub string_table: Vec<String>,
    #[prost(int64, tag = "7")]
    pub drop_frames: i64,
    #[prost(int64, tag = "8")]
    pub keep_frames: i64,
    #[prost(int64, tag = "9")]
    pub time_nanos: i64,
    #[prost(int64, tag = "10")]
    pub duration_nanos: i64,
    #[prost(message, optional, tag = "11")]
    pub period_type: Option<ValueType>,
    #[prost(int64, tag = "12")]
    pub period: i64,
    #[prost(int64, repeated, tag = "13")]
    pub comment: Vec<i64>,
    #[prost(int64, tag = "14")]
    pub default_sample_type: i64,
}


impl Profile {
    pub fn write_to_vec(&self, buffer: &mut Vec<u8>) -> Result<(), EncodeError> {
        use prost::Message;
        self.encode(buffer)
    }
}

#[derive(Clone, Eq, Hash, PartialEq, PartialOrd, Ord, ::prost::Message)]
pub struct Sample {
    /// The ids recorded here correspond to a Profile.location.id.
    /// The leaf is at location_id\[0\].
    #[prost(uint64, repeated, tag = "1")]
    pub location_ids: Vec<u64>,
    /// The type and unit of each value is defined by the corresponding
    /// entry in Profile.sample_type. All samples must have the same
    /// number of values, the same as the length of Profile.sample_type.
    /// When aggregating multiple samples into a single sample, the
    /// result has a list of values that is the elemntwise sum of the
    /// lists of the originals.
    #[prost(int64, repeated, tag = "2")]
    pub values: Vec<i64>,
    /// label includes additional context for this sample. It can include
    /// things like a thread id, allocation size, etc
    #[prost(message, repeated, tag = "3")]
    pub labels: Vec<Label>,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, ::prost::Message)]
pub struct ValueType {
    #[prost(int64, tag = "1")]
    pub r#type: i64, // Index into string table
    #[prost(int64, tag = "2")]
    pub unit: i64, // Index into string table
}

#[derive(Clone, Eq, PartialEq, Hash, PartialOrd, Ord, ::prost::Message)]
pub struct Label {
    #[prost(int64, tag = "1")]
    pub key: i64, // Index into string table
    #[prost(int64, tag = "2")]
    pub str: i64, // Index into string table
    #[prost(int64, tag = "3")]
    pub num: i64,
    #[prost(int64, tag = "4")]
    pub num_unit: i64,
}

impl Label {
    pub fn str(key: i64, str: i64) -> Self {
        Self {
            key,
            str,
            num: 0,
            num_unit: 0,
        }
    }
}

#[derive(Derivative)]
#[derivative(Eq, PartialEq, Hash)]
#[derive(Copy, Clone, ::prost::Message)]
pub struct Mapping {
    // Unique nonzero id for the mapping.
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(uint64, tag = "2")]
    pub memory_start: u64,
    #[prost(uint64, tag = "3")]
    pub memory_limit: u64,
    #[prost(uint64, tag = "4")]
    pub file_offset: u64,
    #[prost(int64, tag = "5")]
    pub filename: i64, // Index into string table
    #[prost(int64, tag = "6")]
    pub build_id: i64, // Index into string table
    #[prost(bool, tag = "7")]
    pub has_functions: bool,
    #[prost(bool, tag = "8")]
    pub has_filenames: bool,
    #[prost(bool, tag = "9")]
    pub has_line_numbers: bool,
    #[prost(bool, tag = "10")]
    pub has_inline_frames: bool,
}

#[derive(Derivative)]
#[derivative(Eq, PartialEq, Hash)]
#[derive(Clone, ::prost::Message)]
pub struct Location {
    /// Unique nonzero id for the location.
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(uint64, tag = "2")]
    pub mapping_id: u64,
    #[prost(uint64, tag = "3")]
    pub address: u64,
    #[prost(message, repeated, tag = "4")]
    pub lines: Vec<Line>,
    #[prost(bool, tag = "5")]
    pub is_folded: bool,
}

#[derive(Copy, Clone, Eq, PartialEq, Hash, ::prost::Message)]
pub struct Line {
    /// The id of the corresponding Function for this line.
    #[prost(uint64, tag = "1")]
    pub function_id: u64,
    /// Line number in source code.
    #[prost(int64, tag = "2")]
    pub line: i64,
}

#[derive(Derivative)]
#[derivative(Eq, PartialEq, Hash)]
#[derive(Copy, Clone, ::prost::Message)]
pub struct Function {
    /// Unique nonzero id for the function.
    #[derivative(PartialEq = "ignore", Hash = "ignore")]
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(int64, tag = "2")]
    pub name: i64, // Index into string table
    #[prost(int64, tag = "3")]
    pub system_name: i64, // Index into string table
    #[prost(int64, tag = "4")]
    pub filename: i64, // Index into string table
    #[prost(int64, tag = "5")]
    pub start_line: i64, // Index into string table
}

#[cfg(test)]
impl Profile {
    pub fn sorted_samples(&self) -> Vec<Sample> {
        let mut samples = self.samples.clone();
        samples.sort_unstable();
        samples
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use prost::Message;

    #[test]
    fn basic() {
        let mut strings: Vec<::prost::alloc::string::String> = Vec::with_capacity(8);
        strings.push("".into()); // 0
        strings.push("samples".into()); // 1
        strings.push("count".into()); // 2
        strings.push("php".into()); // 3
        strings.push("{main}".into()); // 4
        strings.push("index.php".into()); // 5
        strings.push("test".into()); // 6

        let php_mapping = Mapping {
            id: 1,
            filename: 3,
            ..Default::default()
        };

        let main_function = Function {
            id: 1,
            name: 4,
            system_name: 4,
            filename: 5,
            start_line: 0,
        };

        let test_function = Function {
            id: 2,
            name: 6,
            system_name: 6,
            filename: 5,
            start_line: 3,
        };

        let main_line = Line {
            function_id: main_function.id,
            line: 0,
        };

        let test_line = Line {
            function_id: test_function.id,
            line: 4,
        };

        let main_location = Location {
            id: 1,
            mapping_id: php_mapping.id,
            address: 0,
            lines: vec![main_line],
            is_folded: false,
        };

        let test_location = Location {
            id: 2,
            mapping_id: php_mapping.id,
            address: 0,
            lines: vec![test_line],
            is_folded: false,
        };

        let profiles = Profile {
            sample_types: vec![ValueType { r#type: 1, unit: 2 }],
            samples: vec![
                Sample {
                    location_ids: vec![main_location.id],
                    values: vec![1],
                    labels: vec![],
                },
                Sample {
                    location_ids: vec![test_location.id, main_location.id],
                    values: vec![1],
                    labels: vec![],
                },
            ],
            mappings: vec![php_mapping],
            locations: vec![main_location, test_location],
            functions: vec![main_function, test_function],
            string_table: strings,
            ..Default::default()
        };

        let mut buffer: Vec<u8> = Vec::new();
        profiles.encode(&mut buffer).expect("encoding to succeed");
        assert!(buffer.len() >= 72);
    }
}
