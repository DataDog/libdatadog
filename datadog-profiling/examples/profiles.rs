// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling::profiles::datatypes::{
    AnyValue, Function, KeyValue, Line, Location, Profile, ProfilesDictionary, SampleBuilder,
    ScratchPad, ValueType,
};
use datadog_profiling::profiles::{Compressor, FallibleStringWriter, PprofBuilder};
use std::io;
use std::ptr::null_mut;

// Keep this roughly in-sync with profiles.c
fn main() {
    let dictionary = ProfilesDictionary::try_new().unwrap();
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
        profile.try_add_sample(vt).unwrap();
    }

    profile.add_period(10000, wall_time_vt);

    let phpinfo_id = functions
        .try_insert(Function {
            name: strings.try_insert("phpinfo").unwrap(),
            file_name: strings.try_insert("/srv/public/index.php").unwrap(),
            ..Function::default()
        })
        .unwrap()
        .into_raw();

    let main_id = functions
        .try_insert(Function {
            name: strings.try_insert("{main}").unwrap(),
            file_name: strings.try_insert("/srv/public/index.php").unwrap(),
            ..Function::default()
        })
        .unwrap()
        .into_raw();

    let location_1 = locations
        .try_insert(Location {
            address: 0,
            mapping_id: null_mut(),
            line: Line {
                line_number: 3,
                function_id: phpinfo_id.as_ptr(),
            },
        })
        .unwrap();
    let location_2 = locations
        .try_insert(Location {
            address: 0,
            mapping_id: null_mut(),
            line: Line {
                line_number: 10,
                function_id: main_id.as_ptr(),
            },
        })
        .unwrap();

    let stack_id = stacks.try_insert(&[location_1, location_2]).unwrap();

    // Build attributes
    let process_id_attr = {
        let pid = std::process::id();
        let key_id = strings.try_insert("process_id").unwrap();
        KeyValue {
            key: key_id,
            value: AnyValue::Integer(pid as i64),
        }
    };

    let runtime_id_attr = {
        use core::fmt::Write;
        let runtime_id = uuid::Uuid::new_v4();
        let mut rid_w = FallibleStringWriter::new();
        // UUID textual form is 36 bytes (32 hex + 4 hyphens).
        rid_w.try_reserve(36).unwrap();
        write!(&mut rid_w, "{}", runtime_id).unwrap();
        let key_id = strings.try_insert("runtime-id").unwrap();
        KeyValue {
            key: key_id,
            value: AnyValue::String(rid_w.into()),
        }
    };

    let mut sb = SampleBuilder::new(
        attributes.try_clone().unwrap(),
        scratchpad.links().try_clone().unwrap(),
    );
    sb.set_stack_id(stack_id);
    sb.push_value(1).unwrap();
    sb.push_value(10000).unwrap();
    sb.push_attribute(process_id_attr).unwrap();
    sb.push_attribute(runtime_id_attr).unwrap();
    sb.set_timestamp(std::time::SystemTime::UNIX_EPOCH);
    let sample = sb.build().unwrap();

    profile.add_sample(sample).unwrap();

    let mut compressor = Compressor::with_max_capacity(50 * 1024 * 1024);

    // Convert the in-memory profile into pprof using PprofBuilder and
    // stream to an LZ4 compressor, then write to stdout.
    let mut builder = PprofBuilder::new(&dictionary, &scratchpad);
    builder
        .try_add_profile(&profile, std::iter::empty())
        .unwrap();

    builder.build(&mut compressor).unwrap();
    let compressed = compressor.finish().unwrap();
    {
        use io::Write;
        io::stdout().write_all(&compressed).unwrap();
    }
}
