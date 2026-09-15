// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::api;
use crate::exporter;
use crate::pprof::test_utils::{deserialize_compressed_pprof, string_table_fetch};
use libdd_common::test_utils::{
    create_temp_file_path, parse_http_request_sync, HttpRequest, TempFileGuard,
};
use serde_json::json;

const TEST_LIB_NAME: &str = "dd-trace-test";
const TEST_LIB_VERSION: &str = "1.0.0";
const TEST_FAMILY: &str = "test";

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
            str: "",
            num: 101,
            num_unit: "",
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

fn create_test_exporter() -> Box<ProfileExporter> {
    ProfileExporter::create_agent_exporter(
        TEST_LIB_NAME,
        TEST_LIB_VERSION,
        TEST_FAMILY,
        vec![ffi::Tag {
            key: "env",
            value: "test",
        }],
        "http://localhost:1", // Port 1 unlikely to have server
        100,
        false,
    )
    .unwrap()
}

fn create_test_file_exporter(test_name: &str) -> (Box<ProfileExporter>, TempFileGuard) {
    let file_path = create_temp_file_path(test_name, "http");
    let exporter = ProfileExporter::create_file_exporter(
        TEST_LIB_NAME,
        TEST_LIB_VERSION,
        TEST_FAMILY,
        vec![ffi::Tag {
            key: "env",
            value: "test",
        }],
        file_path.to_string_lossy().as_ref(),
    )
    .unwrap();

    (exporter, file_path)
}

fn read_dumped_request_and_event(file_path: &std::path::Path) -> (HttpRequest, serde_json::Value) {
    let request_bytes = std::fs::read(file_path).expect("read dumped request");
    let request = parse_http_request_sync(&request_bytes).expect("parse dumped request");
    let event_part = request
        .multipart_parts
        .iter()
        .find(|part| part.filename.as_deref() == Some("event.json"))
        .expect("event.json multipart part");
    let event_json = serde_json::from_slice(&event_part.content).expect("parse event.json");

    (request, event_json)
}

#[test]
fn test_profile_operations() {
    let mut profile = create_test_profile();

    // Verify profile starts empty
    assert_eq!(
        profile.inner.only_for_testing_num_aggregated_samples(),
        0,
        "Profile should start with no samples"
    );

    // Add samples and verify they're tracked
    let sample = create_test_sample();
    profile.add_sample(&sample);
    assert_eq!(
        profile.inner.only_for_testing_num_aggregated_samples(),
        1,
        "Profile should have 1 sample after adding"
    );

    // Add another sample with different address
    let sample2_locations = vec![create_test_location(0x20003000, 200)];
    let sample2_values = vec![2000000];
    let sample2 = ffi::Sample {
        locations: &sample2_locations,
        values: &sample2_values,
        labels: &[],
    };
    profile.add_sample(&sample2);
    assert_eq!(
        profile.inner.only_for_testing_num_aggregated_samples(),
        2,
        "Profile should have 2 samples"
    );

    // Test endpoints
    profile.add_endpoint(12345, "/api/test");
    profile.add_endpoint(67890, "/api/other");
    profile.add_endpoint_count("/api/test", 100);

    // Test upscaling rules (verify they don't error)
    assert!(profile.add_upscaling_rule_poisson(&[0], "thread_id", "0", 0, 0, 1000000));
    assert!(profile.add_upscaling_rule_proportional(&[0], "thread_id", "1", 100.0));
    assert!(profile.add_upscaling_rule_poisson_non_sample_type_count(
        &[0],
        "thread_id",
        "2",
        0,
        50,
        1000000,
    ));

    // Serialize and verify output
    let serialized = serialize_test_profile_to_vec(&mut profile);
    assert!(
        serialized.len() > 100,
        "Serialized profile should be non-trivial"
    );

    // Verify it's a valid pprof by checking for gzip/zstd magic bytes
    assert!(
        serialized.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) || // zstd magic
        serialized.starts_with(&[0x1f, 0x8b]), // gzip magic
        "Serialized profile should be compressed"
    );

    // After serialization (which resets), profile should be empty.
    assert_eq!(
        profile.inner.only_for_testing_num_aggregated_samples(),
        0,
        "Profile should be empty after serialize"
    );
}

#[test]
fn test_profile_timestamped_sample_operations() {
    let mut profile = create_test_profile();
    let sample = create_test_sample();

    profile.add_sample_with_timestamp(&sample, 42);

    assert_eq!(
        profile.inner.only_for_testing_num_aggregated_samples(),
        0,
        "Timestamped samples should not be aggregated into the non-timestamped bucket"
    );
    assert_eq!(
        profile.inner.only_for_testing_num_timestamped_samples(),
        1,
        "Profile should have 1 timestamped sample after adding"
    );

    let serialized = serialize_test_profile_to_vec(&mut profile);
    assert!(
        serialized.len() > 100,
        "Serialized timestamped profile should be non-trivial"
    );
    assert!(
        serialized.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) || serialized.starts_with(&[0x1f, 0x8b]),
        "Serialized timestamped profile should be compressed"
    );
}

#[test]
fn test_status_and_result_accessors() {
    let status = ffi::Status::err("Test::operation", "boom");
    assert!(!status.ok());
    assert_eq!(status.operation(), "Test::operation");
    assert_eq!(status.message(), "boom");

    let ok_status = ffi::Status::ok_for("Test::ok");
    assert!(ok_status.ok());
    assert!(ok_status.check_and_print());

    let result = ProfileExporter::create_agent_exporter(
        TEST_LIB_NAME,
        TEST_LIB_VERSION,
        TEST_FAMILY,
        vec![],
        "not a url",
        0,
        false,
    );
    assert!(!result.ok());
    assert!(!result.message().is_empty());
}

#[test]
fn test_profile_timestamped_sample_rejects_zero_timestamp() {
    let mut profile = create_test_profile();
    let sample = create_test_sample();

    profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
    assert!(!profile.add_sample_with_timestamp(&sample, 0));
    let errors = profile.take_errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("endtime_ns must be non-zero"));
}

#[test]
fn test_profile_error_storage_modes() {
    let mut profile = create_test_profile();
    let bad_sample = ffi::Sample {
        locations: &[],
        values: &[],
        labels: &[],
    };

    assert!(!profile.add_sample(&bad_sample));
    assert!(!profile.add_sample(&bad_sample));
    let errors = profile.take_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].operation, "Profile::add_sample");
    assert!(!errors[0].message.is_empty());
    assert!(profile.take_errors().is_empty());

    profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
    assert!(!profile.add_sample(&bad_sample));
    assert!(!profile.add_sample(&bad_sample));
    assert_eq!(profile.take_errors().len(), 2);
    assert!(profile.take_errors().is_empty());

    assert!(!profile.add_sample(&bad_sample));
    assert_eq!(profile.take_errors().len(), 1);
    assert!(profile.take_errors().is_empty());
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
fn test_profiles_dictionary_error_storage_modes() {
    let dictionary = create_test_dictionary();

    dictionary.handle_error("ProfileDictionary::intern_string", "first");
    dictionary.handle_error("ProfileDictionary::intern_string", "second");
    let errors = dictionary.take_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].operation, "ProfileDictionary::intern_string");
    assert_eq!(errors[0].message, "first");
    assert!(dictionary.take_errors().is_empty());

    dictionary.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
    dictionary.handle_error("ProfileDictionary::intern_string", "first");
    dictionary.handle_error("ProfileDictionary::intern_string", "second");
    assert_eq!(dictionary.take_errors().len(), 2);
    assert!(dictionary.take_errors().is_empty());

    dictionary.handle_error("ProfileDictionary::intern_string", "third");
    assert_eq!(dictionary.take_errors().len(), 1);
    assert!(dictionary.take_errors().is_empty());
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
fn test_profile_add_dictionary_sample_rejects_zero_timestamp() {
    let dictionary = create_test_dictionary();
    let mut profile = create_test_profile_with_dictionary(&dictionary);
    let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
    let locations = vec![location];
    let sample = ffi::DictionarySample {
        locations: &locations,
        values: &values,
        labels: &labels,
    };

    profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
    // SAFETY: sample ids were produced by the dictionary used to create profile.
    assert!(!unsafe { profile.add_dictionary_sample_with_timestamp(&sample, 0) });
    let errors = profile.take_errors();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("endtime_ns must be non-zero"));
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
fn test_send_encoded_profile_with_attachments() {
    let mut profile = create_test_profile();
    profile.add_sample(&create_test_sample());
    profile.add_endpoint_count("/api/test", 100);

    let encoded = profile.serialize().unwrap();
    let mut exporter = create_test_exporter();
    let attachment_data = br#"{"test": "data", "number": 123}"#.to_vec();

    // Should fail with connection error because test exporter points at localhost:1,
    // but this validates request construction and the encoded-profile API boundary.
    let result = exporter.send_encoded_profile(
        encoded,
        vec![ffi::AttachmentFile {
            name: "metadata.json",
            data: &attachment_data,
        }],
        vec![ffi::Tag {
            key: "profile_type",
            value: "cpu",
        }],
        "language:rust,profiler_version:1.0",
        r#"{"version": "1.0", "profiler": "test"}"#,
        r#"{"os": "linux", "arch": "x86_64", "cores": 8}"#,
    );

    assert!(!result.ok(), "Should fail when no server is available");
    assert_eq!(result.operation(), "ProfileExporter::send_encoded_profile");
    assert!(!result.message().is_empty());
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_send_encoded_profile_file_export_preserves_metadata() {
    let mut profile = create_test_profile();
    profile.add_sample(&create_test_sample());
    profile.add_endpoint_count("/api/test", 2);
    profile.add_endpoint_count("/api/test", 3);
    profile.add_endpoint_count("/api/other", 7);

    let encoded = profile.serialize().unwrap();
    let (mut exporter, file_path) = create_test_file_exporter("cxx_send_encoded_profile");
    let attachment_data = br#"{"test": "data", "number": 123}"#.to_vec();

    exporter
        .send_encoded_profile(
            encoded,
            vec![ffi::AttachmentFile {
                name: "metadata.json",
                data: &attachment_data,
            }],
            vec![ffi::Tag {
                key: "profile_type",
                value: "cpu",
            }],
            "language:rust,profiler_version:1.0",
            r#"{"version": "1.0", "profiler": "test"}"#,
            r#"{"os": "linux", "arch": "x86_64", "cores": 8}"#,
        )
        .unwrap();

    let (request, event_json) = read_dumped_request_and_event(file_path.as_ref());
    assert_eq!(request.method, "POST");
    assert_eq!(
        event_json["attachments"],
        json!(["metadata.json", "profile.pprof"])
    );
    assert_eq!(
        event_json["endpoint_counts"],
        json!({
            "/api/test": 5,
            "/api/other": 7,
        })
    );
    assert_eq!(
        event_json["process_tags"],
        "language:rust,profiler_version:1.0"
    );
    assert_eq!(event_json["internal"]["version"], "1.0");
    assert_eq!(event_json["internal"]["profiler"], "test");
    assert_eq!(
        event_json["internal"]["libdatadog_version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(event_json["info"]["os"], "linux");
    assert_eq!(event_json["info"]["arch"], "x86_64");
    assert_eq!(event_json["info"]["cores"], 8);

    let tags_profiler = event_json["tags_profiler"].as_str().unwrap();
    assert!(tags_profiler.split(',').any(|tag| tag == "env:test"));
    assert!(tags_profiler
        .split(',')
        .any(|tag| tag == "profile_type:cpu"));
    assert!(tags_profiler
        .split(',')
        .any(|tag| tag.starts_with("runtime_platform:")));

    let attachment_part = request
        .multipart_parts
        .iter()
        .find(|part| part.filename.as_deref() == Some("metadata.json"))
        .expect("metadata.json multipart part");
    assert!(!attachment_part.content.is_empty());
    let profile_part = request
        .multipart_parts
        .iter()
        .find(|part| part.name == "profile.pprof")
        .expect("profile.pprof multipart part");
    assert!(!profile_part.content.is_empty());
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_send_encoded_profile_with_cancelled_token_does_not_send() {
    let mut profile = create_test_profile();
    profile.add_sample(&create_test_sample());
    let encoded = profile.serialize().unwrap();
    let (mut exporter, file_path) = create_test_file_exporter("cxx_send_encoded_profile_cancelled");
    let cancel = CancellationToken::create();
    cancel.cancel();

    let result = exporter.send_encoded_profile_with_cancellation(
        encoded,
        vec![],
        vec![],
        "",
        "",
        "",
        &cancel,
    );

    assert!(!result.ok(), "pre-cancelled upload should fail");
    assert_eq!(
        result.operation(),
        "ProfileExporter::send_encoded_profile_with_cancellation"
    );
    assert!(
        !file_path.exists(),
        "pre-cancelled upload should not write a request dump"
    );
}

#[test]
fn test_endpoint_operations() {
    let mut profile = create_test_profile();

    profile.add_endpoint(12345, "/api/test");
    profile.add_endpoint_count("/api/test", 100);
}

#[test]
fn test_profile_add_dictionary_sample_rejects_wrong_value_count() {
    let dictionary = create_test_dictionary();
    let mut profile = create_test_profile_with_dictionary(&dictionary);
    let (location, _values, labels) = create_test_dictionary_sample_parts(&dictionary);
    let locations = vec![location];
    let values = Vec::new();
    let sample = ffi::DictionarySample {
        locations: &locations,
        values: &values,
        labels: &labels,
    };

    profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
    // SAFETY: sample ids were produced by the dictionary used to create profile.
    assert!(!unsafe { profile.add_dictionary_sample_with_timestamp(&sample, 42) });
    assert!(!profile.take_errors().is_empty());
}

#[test]
fn test_profile_add_dictionary_sample_requires_dictionary_profile() {
    let dictionary = create_test_dictionary();
    let mut profile = create_test_profile();
    let (location, values, labels) = create_test_dictionary_sample_parts(&dictionary);
    let locations = vec![location];
    let sample = ffi::DictionarySample {
        locations: &locations,
        values: &values,
        labels: &labels,
    };

    profile.set_error_policy(ffi::ErrorPolicy::StoreEveryOccurrence);
    // SAFETY: sample ids were produced by the dictionary used to create profile.
    assert!(!unsafe { profile.add_dictionary_sample_with_timestamp(&sample, 42) });
    let errors = profile.take_errors();
    assert!(errors[0].message.contains("profiles dictionary not set"));
}

#[test]
fn test_exporter_create() {
    // Test agent exporter with default timeout
    assert!(ProfileExporter::create_agent_exporter(
        TEST_LIB_NAME,
        TEST_LIB_VERSION,
        TEST_FAMILY,
        vec![ffi::Tag {
            key: "service",
            value: "test"
        }],
        "http://localhost:8126",
        0,
        false,
    )
    .is_ok());

    // Test with multiple tags and custom timeout
    assert!(ProfileExporter::create_agent_exporter(
        TEST_LIB_NAME,
        TEST_LIB_VERSION,
        TEST_FAMILY,
        vec![
            ffi::Tag {
                key: "service",
                value: "my-service"
            },
            ffi::Tag {
                key: "env",
                value: "prod"
            },
            ffi::Tag {
                key: "version",
                value: "2.0"
            },
        ],
        "http://localhost:8126",
        10000,
        false,
    )
    .is_ok());

    // Test agentless exporters with different sites
    assert!(ProfileExporter::create_agentless_exporter(
        TEST_LIB_NAME,
        TEST_LIB_VERSION,
        TEST_FAMILY,
        vec![],
        "datadoghq.com",
        "fake-api-key",
        5000,
        false,
    )
    .is_ok());

    assert!(ProfileExporter::create_agentless_exporter(
        TEST_LIB_NAME,
        TEST_LIB_VERSION,
        TEST_FAMILY,
        vec![],
        "datadoghq.eu",
        "fake-api-key",
        0,
        false,
    )
    .is_ok());

    // Test with no tags
    assert!(ProfileExporter::create_agent_exporter(
        TEST_LIB_NAME,
        TEST_LIB_VERSION,
        TEST_FAMILY,
        vec![],
        "http://localhost:8126",
        0,
        false,
    )
    .is_ok());
}

#[test]
fn test_type_conversions() {
    // AttachmentFile conversion
    let data = vec![1u8, 2, 3, 4, 5, 255, 128, 0];
    let file: exporter::File = (&ffi::AttachmentFile {
        name: "test.bin",
        data: &data,
    })
        .into();
    assert_eq!(file.name, "test.bin");
    assert_eq!(file.bytes, data.as_slice());

    // Tag conversion with special characters
    let tag: libdd_common::tag::Tag = (&ffi::Tag {
        key: "test-key.with_special:chars",
        value: "test_value/with@special#chars",
    })
        .try_into()
        .unwrap();
    assert_eq!(
        tag.as_ref(),
        "test-key.with_special:chars:test_value/with@special#chars"
    );

    // Tag validation - empty key should fail
    assert!(TryInto::<libdd_common::tag::Tag>::try_into(&ffi::Tag {
        key: "",
        value: "value"
    })
    .is_err());

    // SampleType conversion
    let st: api::SampleType = ffi::SampleType::CpuSamples.try_into().unwrap();
    let vt: api::ValueType<'static> = st.into();
    assert_eq!(vt.r#type, "cpu-samples");
    assert_eq!(vt.unit, "count");

    // Mapping conversion
    let mapping: api::Mapping = (&ffi::Mapping {
        memory_start: 0x1000,
        memory_limit: 0x2000,
        file_offset: 0x100,
        filename: "/lib/test.so",
        build_id: "build123",
    })
        .into();
    assert_eq!(
        (
            mapping.memory_start,
            mapping.memory_limit,
            mapping.file_offset
        ),
        (0x1000, 0x2000, 0x100)
    );
    assert_eq!(
        (mapping.filename, mapping.build_id),
        ("/lib/test.so", "build123")
    );

    // Function conversion
    let function: api::Function = (&ffi::Function {
        name: "my_func",
        system_name: "_Z7my_funcv",
        filename: "/src/file.cpp",
    })
        .into();
    assert_eq!(
        (function.name, function.system_name, function.filename),
        ("my_func", "_Z7my_funcv", "/src/file.cpp")
    );

    // Label conversion
    let label: api::Label = (&ffi::Label {
        key: "thread_id",
        str: "",
        num: 42,
        num_unit: "thread",
    })
        .into();
    assert_eq!(
        (label.key, label.num, label.num_unit),
        ("thread_id", 42, "thread")
    );
}
