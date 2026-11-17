// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::pprof::test_utils::{deserialize_compressed_pprof, string_table_fetch};

fn create_test_profile() -> Box<Profile> {
    let wall_time = ffi::SampleType::WallTime;
    let period = ffi::Period {
        value_type: wall_time,
        value: 60,
    };
    Profile::create(vec![ffi::SampleType::WallTime], &period).unwrap()
}

fn serialize_test_profile_to_vec(profile: &mut Profile) -> Vec<u8> {
    profile.serialize().unwrap().bytes().to_vec()
}

fn create_test_dictionary() -> Box<ProfileDictionary> {
    ProfileDictionary::create().unwrap()
}

fn create_test_profile_with_dictionary(dictionary: &ProfileDictionary) -> Box<Profile> {
    let wall_time = ffi::SampleType::WallTime;
    let period = ffi::Period {
        value_type: wall_time,
        value: 60,
    };
    Profile::create_with_dictionary(vec![ffi::SampleType::WallTime], &period, dictionary).unwrap()
}

fn intern_test_string(dictionary: &ProfileDictionary, value: &str) -> ffi::DictionaryStringId {
    let mut id = ffi::DictionaryStringId::default();
    assert!(dictionary.intern_string(value, &mut id));
    id
}

fn intern_test_mapping(
    dictionary: &ProfileDictionary,
    mapping: &ffi::DictionaryMapping,
) -> ffi::DictionaryMappingId {
    let mut id = ffi::DictionaryMappingId::default();
    // SAFETY: Test helpers only pass ids created by this dictionary.
    assert!(unsafe { dictionary.intern_mapping(mapping, &mut id) });
    id
}

fn intern_test_function(
    dictionary: &ProfileDictionary,
    function: &ffi::DictionaryFunction,
) -> ffi::DictionaryFunctionId {
    let mut id = ffi::DictionaryFunctionId::default();
    // SAFETY: Test helpers only pass ids created by this dictionary.
    assert!(unsafe { dictionary.intern_function(function, &mut id) });
    id
}

fn create_test_dictionary_sample_parts(
    dictionary: &ProfileDictionary,
) -> (
    ffi::DictionaryLocation,
    Vec<i64>,
    Vec<ffi::DictionaryLabel<'static>>,
) {
    let filename_mapping = intern_test_string(dictionary, "/usr/lib/libtest.so");
    let filename_function = intern_test_string(dictionary, "/src/test.cpp");
    let build_id = intern_test_string(dictionary, "abc123");
    let name = intern_test_string(dictionary, "test_function");
    let system_name = intern_test_string(dictionary, "_Z13test_functionv");
    let label_key = intern_test_string(dictionary, "pid");
    let mapping = intern_test_mapping(
        dictionary,
        &ffi::DictionaryMapping {
            memory_start: 0x10000000,
            memory_limit: 0x20000000,
            file_offset: 0,
            filename: filename_mapping,
            build_id,
        },
    );
    let function = intern_test_function(
        dictionary,
        &ffi::DictionaryFunction {
            name,
            system_name,
            filename: filename_function,
        },
    );

    (
        ffi::DictionaryLocation {
            mapping,
            function,
            address: 0x10003000,
            line: 100,
        },
        vec![1000000],
        vec![ffi::DictionaryLabel {
            key: label_key,
            str_bytes: b"",
            num: 101,
            num_unit: b"",
        }],
    )
}

fn create_test_location(address: u64, line: i64) -> ffi::Location<'static> {
    ffi::Location {
        mapping: ffi::Mapping {
            memory_start: address & 0xFFFF0000,
            memory_limit: (address & 0xFFFF0000) + 0x10000000,
            file_offset: 0,
            filename: "/usr/lib/libtest.so",
            build_id: "abc123",
        },
        function: ffi::Function {
            name: "test_function",
            system_name: "_Z13test_functionv",
            filename: "/src/test.cpp",
        },
        address,
        line,
    }
}

fn create_test_sample() -> ffi::Sample<'static> {
    ffi::Sample {
        locations: Box::leak(vec![create_test_location(0x10003000, 100)].into_boxed_slice()),
        values: Box::leak(vec![1000000].into_boxed_slice()),
        labels: &[],
    }
}

#[test]
fn test_profile_timestamped_sample_serializes_end_timestamp_label() {
    let mut profile = create_test_profile();
    let sample = create_test_sample();

    profile.add_sample_with_timestamp(&sample, 42);

    let serialized = serialize_test_profile_to_vec(&mut profile);
    let pprof = deserialize_compressed_pprof(&serialized).unwrap();
    let timestamp_labels = pprof
        .samples
        .iter()
        .flat_map(|sample| sample.labels.iter())
        .filter(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns")
        .collect::<Vec<_>>();

    assert_eq!(timestamp_labels.len(), 1);
    let timestamp_label = timestamp_labels[0];
    assert_eq!(timestamp_label.num, 42);
    assert_eq!(string_table_fetch(&pprof, timestamp_label.str), "");
    assert_eq!(string_table_fetch(&pprof, timestamp_label.num_unit), "");
}

#[test]
fn test_profile_timestamped_identical_samples_remain_distinct() {
    let mut profile = create_test_profile();
    let sample = create_test_sample();

    profile.add_sample_with_timestamp(&sample, 42);
    profile.add_sample_with_timestamp(&sample, 43);

    let serialized = serialize_test_profile_to_vec(&mut profile);
    let pprof = deserialize_compressed_pprof(&serialized).unwrap();
    let mut timestamps = pprof
        .samples
        .iter()
        .flat_map(|sample| sample.labels.iter())
        .filter(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns")
        .map(|label| label.num)
        .collect::<Vec<_>>();

    timestamps.sort_unstable();

    assert_eq!(timestamps, vec![42, 43]);
}

#[test]
fn test_profiles_dictionary_operations() {
    let dictionary = create_test_dictionary();
    let filename = intern_test_string(&dictionary, "example.py");
    let build_id = intern_test_string(&dictionary, "build-id");
    let function_name = intern_test_string(&dictionary, "function");
    let system_name = intern_test_string(&dictionary, "system_function");
    let function_file_name = intern_test_string(&dictionary, "example.py");

    let mapping = intern_test_mapping(
        &dictionary,
        &ffi::DictionaryMapping {
            memory_start: 1,
            memory_limit: 2,
            file_offset: 3,
            filename,
            build_id,
        },
    );
    let function = intern_test_function(
        &dictionary,
        &ffi::DictionaryFunction {
            name: function_name,
            system_name,
            filename: function_file_name,
        },
    );

    assert!(!mapping.is_null());
    assert!(!function.is_null());
}

#[test]
fn test_profiles_dictionary_status_operations() {
    let dictionary = create_test_dictionary();

    assert!(ffi::DictionaryStringId::default().is_null());
    assert!(ffi::DictionaryFunctionId::default().is_null());
    assert!(ffi::DictionaryMappingId::default().is_null());

    let mut filename = ffi::DictionaryStringId::default();
    assert!(dictionary.intern_string("example.py", &mut filename));
    assert!(!filename.is_null());

    let mut build_id = ffi::DictionaryStringId::default();
    assert!(dictionary.intern_string("build-id", &mut build_id));
    assert!(!build_id.is_null());

    let mut mapping = ffi::DictionaryMappingId::default();
    // SAFETY: filename and build_id were interned by this dictionary.
    assert!(unsafe {
        dictionary.intern_mapping(
            &ffi::DictionaryMapping {
                memory_start: 1,
                memory_limit: 2,
                file_offset: 3,
                filename,
                build_id,
            },
            &mut mapping,
        )
    });
    assert!(!mapping.is_null());

    let mut function_name = ffi::DictionaryStringId::default();
    assert!(dictionary.intern_string("function", &mut function_name));
    let mut function_file_name = ffi::DictionaryStringId::default();
    assert!(dictionary.intern_string("example.py", &mut function_file_name));
    let mut function = ffi::DictionaryFunctionId::default();
    // SAFETY: function_name and function_file_name were interned by this
    // dictionary; system_name is the null/default id.
    assert!(unsafe {
        dictionary.intern_function(
            &ffi::DictionaryFunction {
                name: function_name,
                system_name: ffi::DictionaryStringId {
                    handle: std::ptr::null_mut(),
                },
                filename: function_file_name,
            },
            &mut function,
        )
    });
    assert!(!function.is_null());
}

#[test]
fn test_profile_add_dictionary_sample_serializes() {
    let dictionary = create_test_dictionary();
    let mut profile = create_test_profile_with_dictionary(&dictionary);
    let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
    let locations = vec![location];
    let sample = ffi::DictionarySample {
        locations: &locations,
        values: &values,
        labels: &labels,
    };

    // SAFETY: sample ids were produced by the dictionary used to create profile.
    unsafe { profile.add_dictionary_sample_with_timestamp(&sample, 42) };

    let serialized = serialize_test_profile_to_vec(&mut profile);
    assert!(
        serialized.len() > 100,
        "Serialized dictionary-backed profile should be non-trivial"
    );
}

#[test]
fn test_profile_add_dictionary_sample_profile_holds_dictionary_alive() {
    let dictionary = create_test_dictionary();
    let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
    let mut profile = create_test_profile_with_dictionary(&dictionary);
    drop(dictionary);

    let locations = vec![location];
    let sample = ffi::DictionarySample {
        locations: &locations,
        values: &values,
        labels: &labels,
    };

    // SAFETY: sample ids were produced by the dictionary used to create profile.
    unsafe { profile.add_dictionary_sample_with_timestamp(&sample, 42) };

    let serialized = serialize_test_profile_to_vec(&mut profile);
    assert!(serialized.len() > 100);
}

#[test]
fn test_profile_add_dictionary_sample_serializes_dictionary_label_and_timestamp() {
    let dictionary = create_test_dictionary();
    let mut profile = create_test_profile_with_dictionary(&dictionary);
    let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
    let locations = vec![location];
    let sample = ffi::DictionarySample {
        locations: &locations,
        values: &values,
        labels: &labels,
    };

    // SAFETY: sample ids were produced by the dictionary used to create profile.
    unsafe { profile.add_dictionary_sample_with_timestamp(&sample, 42) };

    let serialized = serialize_test_profile_to_vec(&mut profile);
    let pprof = deserialize_compressed_pprof(&serialized).unwrap();

    let sample = pprof.samples.first().expect("serialized sample");
    let pid_label = sample
        .labels
        .iter()
        .find(|label| string_table_fetch(&pprof, label.key) == "pid")
        .expect("pid label");
    assert_eq!(pid_label.num, 101);

    let timestamp_label = sample
        .labels
        .iter()
        .find(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns")
        .expect("end_timestamp_ns label");
    assert_eq!(timestamp_label.num, 42);
}

#[test]
fn test_profile_add_dictionary_sample_identical_timestamped_samples_remain_distinct() {
    let dictionary = create_test_dictionary();
    let mut profile = create_test_profile_with_dictionary(&dictionary);
    let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
    let locations = vec![location];
    let sample = ffi::DictionarySample {
        locations: &locations,
        values: &values,
        labels: &labels,
    };

    // SAFETY: sample ids were produced by the dictionary used to create profile.
    unsafe { profile.add_dictionary_sample_with_timestamp(&sample, 42) };
    // SAFETY: sample ids were produced by the dictionary used to create profile.
    unsafe { profile.add_dictionary_sample_with_timestamp(&sample, 43) };

    let serialized = serialize_test_profile_to_vec(&mut profile);
    let pprof = deserialize_compressed_pprof(&serialized).unwrap();
    let mut timestamps = pprof
        .samples
        .iter()
        .flat_map(|sample| sample.labels.iter())
        .filter(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns")
        .map(|label| label.num)
        .collect::<Vec<_>>();

    timestamps.sort_unstable();

    assert_eq!(timestamps, vec![42, 43]);
}

#[test]
fn test_profile_add_dictionary_sample_without_timestamp() {
    let dictionary = create_test_dictionary();
    let mut profile = create_test_profile_with_dictionary(&dictionary);
    let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
    let locations = vec![location];
    let sample = ffi::DictionarySample {
        locations: &locations,
        values: &values,
        labels: &labels,
    };

    // SAFETY: sample ids were produced by the dictionary used to create profile.
    unsafe { profile.add_dictionary_sample(&sample) };
    assert_eq!(profile.inner.only_for_testing_num_aggregated_samples(), 1);
    assert_eq!(profile.inner.only_for_testing_num_timestamped_samples(), 0);

    let serialized = serialize_test_profile_to_vec(&mut profile);
    let pprof = deserialize_compressed_pprof(&serialized).unwrap();
    let has_timestamp_label = pprof
        .samples
        .iter()
        .flat_map(|sample| sample.labels.iter())
        .any(|label| string_table_fetch(&pprof, label.key) == "end_timestamp_ns");

    assert!(!has_timestamp_label);
}

#[test]
fn test_profile_serialize_returns_encoded_profile_and_resets() {
    let mut profile = create_test_profile();
    profile.add_sample(&create_test_sample());

    let encoded = profile.serialize().unwrap();

    assert!(
        encoded.bytes().len() > 100,
        "Encoded profile should contain non-trivial compressed bytes"
    );
    assert_eq!(
        profile.inner.only_for_testing_num_aggregated_samples(),
        0,
        "Profile should be empty after serialize"
    );
}

#[test]
fn test_endpoint_operations() {
    let mut profile = create_test_profile();

    profile.add_endpoint(12345, b"/api/test");
    profile.add_endpoint_count(b"/api/test", 100);
}

#[test]
fn test_intern_string_lossy_valid_utf8() {
    let dictionary = create_test_dictionary();
    let mut id = ffi::DictionaryStringId::default();
    assert!(dictionary.intern_string_lossy(b"hello", &mut id));
    assert!(!id.is_null());

    // Same bytes interned via intern_string should produce the same id
    let mut id2 = ffi::DictionaryStringId::default();
    assert!(dictionary.intern_string("hello", &mut id2));
    assert_eq!(id.handle, id2.handle);
}

#[test]
fn test_intern_string_lossy_invalid_utf8() {
    let dictionary = create_test_dictionary();
    let mut id = ffi::DictionaryStringId::default();
    // 0xFF is not valid UTF-8 — should be replaced with U+FFFD
    assert!(dictionary.intern_string_lossy(b"\xFF", &mut id));
    assert!(!id.is_null());

    // Interning the same invalid bytes again should return the same id
    let mut id2 = ffi::DictionaryStringId::default();
    assert!(dictionary.intern_string_lossy(b"\xFF", &mut id2));
    assert_eq!(id.handle, id2.handle);
}
