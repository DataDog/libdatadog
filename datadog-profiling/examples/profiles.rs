// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use arrayvec::ArrayVec;
use datadog_profiling::profiles;
use datadog_profiling::profiles::datatypes::{
    AnyValue, Function, KeyValue, Line, Location, Profile, Sample, ScratchPad, ValueType,
};
use datadog_profiling::profiles::string_writer::FallibleStringWriter;

// Keep this in-sync with profiles.c
fn main() {
    let dictionary = profiles::datatypes::ProfilesDictionary::try_new().unwrap();
    let strings = dictionary.strings();
    let functions = dictionary.functions();

    // Example doesn't use mappings, but this is how you get them.
    let _mappings = dictionary.mappings();

    let scratchpad = ScratchPad::try_new().unwrap();
    let locations = scratchpad.locations();
    let stacks = scratchpad.stacks();
    let attributes = scratchpad.attributes();

    let wall_time_id = strings.try_insert("wall-time").unwrap();
    let nanoseconds_id = strings.try_insert("nanoseconds").unwrap();
    let samples_id = strings.try_insert("samples").unwrap();
    let count_id = strings.try_insert("count").unwrap();

    let wall_time_vt = ValueType::new(wall_time_id, nanoseconds_id);
    let sample_types = [ValueType::new(samples_id, count_id), wall_time_vt];

    let mut profile = Profile::default();
    for vt in sample_types {
        profile.add_sample_type(vt).unwrap();
    }

    profile.add_period(10000, wall_time_vt);

    let phpinfo_id = functions
        .try_insert(Function {
            name: strings.try_insert("phpinfo").unwrap(),
            file_name: strings.try_insert("/srv/public/index.php").unwrap(),
            ..Function::default()
        })
        .unwrap();

    let main_id = functions
        .try_insert(Function {
            name: strings.try_insert("{main}").unwrap(),
            file_name: strings.try_insert("/srv/public/index.php").unwrap(),
            ..Function::default()
        })
        .unwrap();

    let location_1 = locations
        .try_insert(Location {
            address: 0,
            mapping_id: None,
            line: Line {
                line_number: 3,
                function_id: Some(phpinfo_id),
            },
        })
        .unwrap();
    let location_2 = locations
        .try_insert(Location {
            address: 0,
            mapping_id: None,
            line: Line {
                line_number: 10,
                function_id: Some(main_id),
            },
        })
        .unwrap();

    let stack_id = stacks.try_insert(&[location_1, location_2]).unwrap();

    // Build attributes: process_id (string) and runtime-id (UUIDv4 as string)
    // Process ID as string (u32 decimal fits within 10 chars)
    let pid = std::process::id();
    let pid_str = FallibleStringWriter::try_format_with_size_hint(&pid, 10).unwrap();
    let process_id_attr = KeyValue {
        key: "process_id".into(),
        value: AnyValue::String(pid_str),
    };
    let process_id_attr_id = attributes.try_insert(process_id_attr).unwrap();

    // UUID textual form is 36 bytes (32 hex + 4 hyphens). Reserve exactly that.
    let runtime_id = uuid::Uuid::new_v4();
    let runtime_id_str = FallibleStringWriter::try_format_with_size_hint(&runtime_id, 36).unwrap();
    let runtime_id_attr = KeyValue {
        key: "runtime-id".into(),
        value: AnyValue::String(runtime_id_str),
    };
    let runtime_id_attr_id = attributes.try_insert(runtime_id_attr).unwrap();

    profile
        .add_sample(Sample {
            stack_id,
            values: ArrayVec::from([1, 10000]),
            attributes: vec![process_id_attr_id, runtime_id_attr_id],
            link_id: None,
            timestamp_nanos: 0,
        })
        .unwrap();

    // todo: convert the in-memory profile into an uncompressed pprof with a new type PprofBuilder.
    // todo: compress the pprof using profiles::Compressor.
    // todo: write the compressed pprof to stdout.
}
