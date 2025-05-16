// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling_core::prost_impls::{
    Function, Location, Mapping, Profile, Sample, ValueType,
};

pub use test_helpers::*;

mod test_helpers {
    use super::*;

    pub fn sorted_samples(profile: &Profile) -> Vec<Sample> {
        let mut samples = profile.samples.clone();
        samples.sort_unstable();
        samples
    }

    pub fn string_table_fetch(profile: &Profile, id: i64) -> &String {
        profile
            .string_table
            .get(id as usize)
            .unwrap_or_else(|| panic!("String {id} not found"))
    }

    pub fn string_table_fetch_owned(profile: &Profile, id: i64) -> Box<str> {
        string_table_fetch(profile, id).clone().into_boxed_str()
    }
}

mod tests {
    use super::*;
    use datadog_profiling_core::prost_impls::Line;
    use prost::Message;

    #[test]
    fn basic() {
        let mut strings: Vec<String> = Vec::with_capacity(8);
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
        };

        let test_function = Function {
            id: 2,
            name: 6,
            system_name: 6,
            filename: 5,
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
