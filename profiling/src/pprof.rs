// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

include!(concat!(env!("OUT_DIR"), "/pprof.rs"));

#[cfg(test)]
mod test {
    use crate::pprof::{Function, Line, Location, Mapping, Profile, Sample, ValueType};
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
            line: vec![main_line],
            is_folded: false,
        };

        let test_location = Location {
            id: 2,
            mapping_id: php_mapping.id,
            address: 0,
            line: vec![test_line],
            is_folded: false,
        };

        let profiles = Profile {
            sample_type: vec![ValueType { r#type: 1, unit: 2 }],
            sample: vec![
                Sample {
                    location_id: vec![main_location.id],
                    value: vec![1],
                    label: vec![],
                },
                Sample {
                    location_id: vec![test_location.id, main_location.id],
                    value: vec![1],
                    label: vec![],
                },
            ],
            mapping: vec![php_mapping],
            location: vec![main_location, test_location],
            function: vec![main_function, test_function],
            string_table: strings,
            ..Default::default()
        };

        let mut buffer: Vec<u8> = Vec::new();
        profiles.encode(&mut buffer).expect("encoding to succeed");
        assert!(buffer.len() >= 72);
    }
}
