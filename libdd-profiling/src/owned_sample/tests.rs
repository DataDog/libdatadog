// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::api::{Function, Label, Location, Mapping};

#[test]
fn test_owned_sample_basic() {
    let metadata = Arc::new(Metadata::new(vec![
        SampleType::Cpu,
        SampleType::Wall,
    ], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata.clone());
    
    sample.set_value(SampleType::Cpu, 100).unwrap();
    sample.set_value(SampleType::Wall, 200).unwrap();

    sample.add_location(Location {
        mapping: Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "libfoo.so",
            build_id: "abc123",
        },
        function: Function {
            name: "my_function",
            system_name: "_Z11my_functionv",
            filename: "foo.cpp",
        },
        address: 0x1234,
        line: 42,
    });

    sample.add_label(Label { key: "thread_name", str: "worker-1", num: 0, num_unit: "" });
    sample.add_label(Label { key: "thread_id", str: "", num: 123, num_unit: "" });

    assert_eq!(sample.num_locations(), 1);
    assert_eq!(sample.num_labels(), 2);
    assert_eq!(sample.get_value(SampleType::Cpu).unwrap(), 100);
    assert_eq!(sample.get_value(SampleType::Wall).unwrap(), 200);

    let location = sample.get_location(0).unwrap();
    assert_eq!(location.mapping.filename, "libfoo.so");
    assert_eq!(location.function.name, "my_function");
    assert_eq!(location.address, 0x1234);

    let label = sample.get_label(0).unwrap();
    assert_eq!(label.key, "thread_name");
    assert_eq!(label.str, "worker-1");
}


#[test]
fn test_as_sample() {
    let metadata = Arc::new(Metadata::new(vec![
        SampleType::Cpu,
        SampleType::Wall,
    ], 64, true).unwrap());
    let mut owned = OwnedSample::new(metadata.clone());
    owned.set_value(SampleType::Cpu, 100).unwrap();
    owned.set_value(SampleType::Wall, 200).unwrap();
    owned.add_location(Location {
        mapping: Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "libfoo.so",
            build_id: "abc123",
        },
        function: Function {
            name: "my_function",
            system_name: "_Z11my_functionv",
            filename: "foo.cpp",
        },
        address: 0x1234,
        line: 42,
    });
    owned.add_label(Label { key: "key", str: "value", num: 0, num_unit: "" });

    let borrowed = owned.as_sample();
    assert_eq!(borrowed.values, &[100, 200]);
    assert_eq!(borrowed.locations.len(), 1);
    assert_eq!(borrowed.labels.len(), 1);
    assert_eq!(borrowed.locations[0].function.name, "my_function");
    assert_eq!(borrowed.labels[0].key, "key");
}

#[test]
fn test_set_value_error() {
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    // Should work for configured type
    assert!(sample.set_value(SampleType::Cpu, 100).is_ok());
    assert_eq!(sample.get_value(SampleType::Cpu).unwrap(), 100);
    
    // Should fail for unconfigured type
    assert!(sample.set_value(SampleType::Wall, 200).is_err());
    assert!(sample.get_value(SampleType::Wall).is_err());
}

#[test]
fn test_sample_type_indices_basic() {
    let metadata = Metadata::new(vec![
        SampleType::Cpu,
        SampleType::Wall,
        SampleType::Allocation,
    ], 64, true).unwrap();

    assert_eq!(metadata.len(), 3);
    assert!(!metadata.is_empty());

    assert_eq!(metadata.get_index(&SampleType::Cpu), Some(0));
    assert_eq!(metadata.get_index(&SampleType::Wall), Some(1));
    assert_eq!(metadata.get_index(&SampleType::Allocation), Some(2));
    assert_eq!(metadata.get_index(&SampleType::Heap), None);

    assert_eq!(metadata.get_type(0), Some(SampleType::Cpu));
    assert_eq!(metadata.get_type(1), Some(SampleType::Wall));
    assert_eq!(metadata.get_type(2), Some(SampleType::Allocation));
    assert_eq!(metadata.get_type(3), None);
}

#[test]
fn test_sample_type_indices_duplicate_error() {
    let result = Metadata::new(vec![
        SampleType::Cpu,
        SampleType::Wall,
        SampleType::Cpu, // Duplicate
        SampleType::Allocation,
    ], 64, true);

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("duplicate"));
}

#[test]
fn test_sample_type_indices_empty_error() {
    let result = Metadata::new(vec![], 64, true);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("empty"));
}

#[test]
fn test_sample_type_indices_iter() {
    let metadata = Metadata::new(vec![
        SampleType::Cpu,
        SampleType::Wall,
        SampleType::Allocation,
    ], 64, true).unwrap();

    let types: Vec<_> = metadata.iter().copied().collect();
    assert_eq!(types, vec![
        SampleType::Cpu,
        SampleType::Wall,
        SampleType::Allocation,
    ]);
}

#[test]
fn test_reset() {
    let metadata = Arc::new(Metadata::new(vec![
        SampleType::Cpu,
        SampleType::Wall,
        SampleType::Allocation,
    ], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    sample.set_value(SampleType::Cpu, 100).unwrap();
    sample.set_value(SampleType::Wall, 200).unwrap();
    sample.set_value(SampleType::Allocation, 300).unwrap();
    
    sample.add_location(Location {
        mapping: Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0,
            filename: "libfoo.so",
            build_id: "abc123",
        },
        function: Function {
            name: "my_function",
            system_name: "_Z11my_functionv",
            filename: "foo.cpp",
        },
        address: 0x1234,
        line: 42,
    });
    sample.add_label(Label { key: "key", str: "value", num: 0, num_unit: "" });
    
    assert_eq!(sample.num_locations(), 1);
    assert_eq!(sample.num_labels(), 1);
    assert_eq!(sample.get_value(SampleType::Cpu).unwrap(), 100);
    assert_eq!(sample.get_value(SampleType::Wall).unwrap(), 200);
    assert_eq!(sample.get_value(SampleType::Allocation).unwrap(), 300);

    // Reset clears locations/labels and zeros values
    sample.reset();
    
    assert_eq!(sample.num_locations(), 0);
    assert_eq!(sample.num_labels(), 0);
    assert_eq!(sample.get_value(SampleType::Cpu).unwrap(), 0);
    assert_eq!(sample.get_value(SampleType::Wall).unwrap(), 0);
    assert_eq!(sample.get_value(SampleType::Allocation).unwrap(), 0);

    // Can add new data after reset
    sample.add_location(Location {
        mapping: Mapping {
            memory_start: 0,
            memory_limit: 0,
            file_offset: 0,
            filename: "new.so",
            build_id: "",
        },
        function: Function {
            name: "new_func",
            system_name: "",
            filename: "",
        },
        address: 0,
        line: 1,
    });
    assert_eq!(sample.num_locations(), 1);
    let loc = sample.get_location(0).unwrap();
    assert_eq!(loc.mapping.filename, "new.so");
}

#[test]
fn test_add_multiple() {
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    // Add multiple locations at once
    let locations = &[
        Location {
            mapping: Mapping { memory_start: 0x1000, memory_limit: 0x2000, file_offset: 0, filename: "lib1.so", build_id: "" },
            function: Function { name: "func1", system_name: "", filename: "file1.c" },
            address: 0x1234,
            line: 10,
        },
        Location {
            mapping: Mapping { memory_start: 0x3000, memory_limit: 0x4000, file_offset: 0, filename: "lib2.so", build_id: "" },
            function: Function { name: "func2", system_name: "", filename: "file2.c" },
            address: 0x5678,
            line: 20,
        },
    ];
    sample.add_locations(locations);
    
    // Add multiple labels at once
    let labels = &[
        Label { key: "thread", str: "main", num: 0, num_unit: "" },
        Label { key: "thread_id", str: "", num: 123, num_unit: "" },
    ];
    sample.add_labels(labels);
    
    assert_eq!(sample.num_locations(), 2);
    assert_eq!(sample.num_labels(), 2);
    
    let loc0 = sample.get_location(0).unwrap();
    assert_eq!(loc0.mapping.filename, "lib1.so");
    assert_eq!(loc0.function.name, "func1");
    
    let loc1 = sample.get_location(1).unwrap();
    assert_eq!(loc1.mapping.filename, "lib2.so");
    assert_eq!(loc1.function.name, "func2");
    
    let label0 = sample.get_label(0).unwrap();
    assert_eq!(label0.key, "thread");
    assert_eq!(label0.str, "main");
    
    let label1 = sample.get_label(1).unwrap();
    assert_eq!(label1.key, "thread_id");
    assert_eq!(label1.num, 123);
}

#[test]
fn test_endtime_ns() {
    use std::num::NonZeroI64;
    
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    // Initially, endtime_ns should be None
    assert_eq!(sample.endtime_ns(), None);
    
    // Set a non-zero endtime
    sample.set_endtime_ns(123456789);
    assert_eq!(sample.endtime_ns(), NonZeroI64::new(123456789));
    
    // Setting to 0 should clear it
    sample.set_endtime_ns(0);
    assert_eq!(sample.endtime_ns(), None);
    
    // Set another value
    sample.set_endtime_ns(987654321);
    assert_eq!(sample.endtime_ns(), NonZeroI64::new(987654321));
    
    // Reset should clear endtime_ns
    sample.reset();
    assert_eq!(sample.endtime_ns(), None);
}

#[test]
fn test_set_endtime_ns_now() {
    use std::time::SystemTime;
    
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    // Initially, endtime_ns should be None
    assert_eq!(sample.endtime_ns(), None);
    
    // Get approximate current time
    let approx_now_ns = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    
    // Set endtime to now and get the returned timestamp
    let returned_time = sample.set_endtime_ns_now().unwrap();
    
    // The endtime should be set to a reasonable value
    let endtime = sample.endtime_ns().unwrap().get();
    
    // The returned time should match what was set
    assert_eq!(returned_time, endtime);
    
    // Allow for a 1 second difference due to monotonic vs realtime clock differences
    // and the time taken to compute the offset
    let second_ns = 1_000_000_000i64;
    assert!(
        (endtime - approx_now_ns).abs() < second_ns,
        "endtime {} should be within 1 second of approx_now {}",
        endtime,
        approx_now_ns
    );
    
    // Test that calling it twice gives increasing values
    let first_endtime = sample.endtime_ns().unwrap().get();
    std::thread::sleep(std::time::Duration::from_millis(1));
    sample.set_endtime_ns_now().unwrap();
    let second_endtime = sample.endtime_ns().unwrap().get();
    assert!(
        second_endtime >= first_endtime,
        "second endtime {} should be >= first endtime {}",
        second_endtime,
        first_endtime
    );
    
    // Reset should clear it
    sample.reset();
    assert_eq!(sample.endtime_ns(), None);
}

#[test]
fn test_timeline_enabled() {
    // Test with timeline enabled
    let metadata_enabled = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    assert!(metadata_enabled.is_timeline_enabled());
    let mut sample_enabled = OwnedSample::new(metadata_enabled);
    
    // Set endtime should work when timeline is enabled
    sample_enabled.set_endtime_ns(123456789);
    assert_eq!(sample_enabled.endtime_ns().unwrap().get(), 123456789);
    
    // set_endtime_ns_now should return the timestamp it sets when enabled
    let returned_time = sample_enabled.set_endtime_ns_now().unwrap();
    assert_ne!(returned_time, 0); // should not be 0 when timeline is enabled
    assert_eq!(sample_enabled.endtime_ns().unwrap().get(), returned_time); // should match
    
    // Test with timeline disabled
    let metadata_disabled = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, false).unwrap());
    assert!(!metadata_disabled.is_timeline_enabled());
    let mut sample_disabled = OwnedSample::new(metadata_disabled);
    
    // Set endtime should be a no-op when timeline is disabled
    sample_disabled.set_endtime_ns(987654321);
    assert_eq!(sample_disabled.endtime_ns(), None); // not set
    
    // set_endtime_ns_now should still calculate and return time when disabled, but not set it
    let returned_time = sample_disabled.set_endtime_ns_now().unwrap();
    assert_ne!(returned_time, 0); // still returns the calculated timestamp
    assert_eq!(sample_disabled.endtime_ns(), None); // but doesn't set it
}

#[test]
#[cfg(unix)]
fn test_set_endtime_from_monotonic_ns() {
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    // Set endtime from a monotonic time
    let monotonic_ns = 123456789000; // Some monotonic time
    sample.set_endtime_from_monotonic_ns(monotonic_ns).unwrap();
    
    // The endtime should be set (monotonic + offset)
    let endtime = sample.endtime_ns();
    assert!(endtime.is_some());
    
    // The endtime should be much larger than the monotonic time
    // (because it includes the offset from system boot to epoch)
    let endtime_val = endtime.unwrap().get();
    
    // Get current epoch time to verify the conversion is reasonable
    use std::time::SystemTime;
    let now_epoch_ns = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    
    // The converted time should be somewhere near the current time
    // (within a reasonable range, e.g., the last year and next minute)
    let year_ns = 365 * 24 * 60 * 60 * 1_000_000_000i64;
    let minute_ns = 60 * 1_000_000_000i64;
    assert!(endtime_val > now_epoch_ns - year_ns, "endtime too far in the past");
    assert!(endtime_val < now_epoch_ns + minute_ns, "endtime too far in the future");
    
    // Set endtime from another monotonic time
    let monotonic_ns2 = monotonic_ns + 1_000_000; // 1ms later
    sample.set_endtime_from_monotonic_ns(monotonic_ns2).unwrap();
    
    let endtime2 = sample.endtime_ns().unwrap().get();
    // The difference should match (1ms)
    assert_eq!(endtime2 - endtime_val, 1_000_000);
}

#[test]
fn test_reverse_locations() {
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    // Initially, reverse_locations should be false
    assert!(!sample.is_reverse_locations());
    
    // Add three locations
    sample.add_location(Location {
        mapping: Mapping { memory_start: 0x1000, memory_limit: 0x2000, file_offset: 0, filename: "lib1.so", build_id: "" },
        function: Function { name: "func1", system_name: "", filename: "" },
        address: 0x1001,
        line: 10,
    });
    sample.add_location(Location {
        mapping: Mapping { memory_start: 0x2000, memory_limit: 0x3000, file_offset: 0, filename: "lib2.so", build_id: "" },
        function: Function { name: "func2", system_name: "", filename: "" },
        address: 0x2002,
        line: 20,
    });
    sample.add_location(Location {
        mapping: Mapping { memory_start: 0x3000, memory_limit: 0x4000, file_offset: 0, filename: "lib3.so", build_id: "" },
        function: Function { name: "func3", system_name: "", filename: "" },
        address: 0x3003,
        line: 30,
    });
    
    // Get sample with normal order
    let normal_sample = sample.as_sample();
    assert_eq!(normal_sample.locations.len(), 3);
    assert_eq!(normal_sample.locations[0].function.name, "func1");
    assert_eq!(normal_sample.locations[1].function.name, "func2");
    assert_eq!(normal_sample.locations[2].function.name, "func3");
    
    // Enable reverse locations
    sample.set_reverse_locations(true);
    assert!(sample.is_reverse_locations());
    
    // Get sample with reversed order
    let reversed_sample = sample.as_sample();
    assert_eq!(reversed_sample.locations.len(), 3);
    assert_eq!(reversed_sample.locations[0].function.name, "func3");
    assert_eq!(reversed_sample.locations[1].function.name, "func2");
    assert_eq!(reversed_sample.locations[2].function.name, "func1");
    
    // Disable reverse locations
    sample.set_reverse_locations(false);
    assert!(!sample.is_reverse_locations());
    
    // Should be back to normal order
    let normal_again = sample.as_sample();
    assert_eq!(normal_again.locations[0].function.name, "func1");
    assert_eq!(normal_again.locations[1].function.name, "func2");
    assert_eq!(normal_again.locations[2].function.name, "func3");
    
    // Reset should clear the flag
    sample.set_reverse_locations(true);
    sample.reset();
    assert!(!sample.is_reverse_locations());
}

#[test]
fn test_label_key() {
    // Test as_str()
    assert_eq!(LabelKey::ExceptionType.as_str(), "exception type");
    assert_eq!(LabelKey::ThreadId.as_str(), "thread id");
    assert_eq!(LabelKey::ThreadNativeId.as_str(), "thread native id");
    assert_eq!(LabelKey::ThreadName.as_str(), "thread name");
    assert_eq!(LabelKey::TaskId.as_str(), "task id");
    assert_eq!(LabelKey::TaskName.as_str(), "task name");
    assert_eq!(LabelKey::SpanId.as_str(), "span id");
    assert_eq!(LabelKey::LocalRootSpanId.as_str(), "local root span id");
    assert_eq!(LabelKey::TraceType.as_str(), "trace type");
    assert_eq!(LabelKey::ClassName.as_str(), "class name");
    assert_eq!(LabelKey::LockName.as_str(), "lock name");
    assert_eq!(LabelKey::GpuDeviceName.as_str(), "gpu device name");
    
    // Test AsRef<str>
    let key: &str = LabelKey::ThreadId.as_ref();
    assert_eq!(key, "thread id");
    
    // Test Display
    assert_eq!(format!("{}", LabelKey::ThreadName), "thread name");
    
    // Test that it can be used as a label key
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    sample.add_label(Label {
        key: LabelKey::ThreadId.as_str(),
        str: "",
        num: 42,
        num_unit: "",
    });
    
    sample.add_label(Label {
        key: LabelKey::ThreadName.as_str(),
        str: "worker-1",
        num: 0,
        num_unit: "",
    });
    
    assert_eq!(sample.num_labels(), 2);
}

#[test]
fn test_add_string_label() {
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    // Add string labels using the convenience method
    sample.add_string_label(LabelKey::ThreadName, "worker-1");
    sample.add_string_label(LabelKey::ExceptionType, "ValueError");
    sample.add_string_label(LabelKey::ClassName, "MyClass");
    
    assert_eq!(sample.num_labels(), 3);
    
    // Verify the labels were added correctly
    let api_sample = sample.as_sample();
    assert_eq!(api_sample.labels.len(), 3);
    
    // Check first label
    assert_eq!(api_sample.labels[0].key, "thread name");
    assert_eq!(api_sample.labels[0].str, "worker-1");
    assert_eq!(api_sample.labels[0].num, 0);
    
    // Check second label
    assert_eq!(api_sample.labels[1].key, "exception type");
    assert_eq!(api_sample.labels[1].str, "ValueError");
    assert_eq!(api_sample.labels[1].num, 0);
}

#[test]
fn test_add_num_label() {
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    // Add numeric labels using the convenience method
    sample.add_num_label(LabelKey::ThreadId, 42);
    sample.add_num_label(LabelKey::ThreadNativeId, 12345);
    sample.add_num_label(LabelKey::SpanId, 98765);
    
    assert_eq!(sample.num_labels(), 3);
    
    // Verify the labels were added correctly
    let api_sample = sample.as_sample();
    assert_eq!(api_sample.labels.len(), 3);
    
    // Check first label
    assert_eq!(api_sample.labels[0].key, "thread id");
    assert_eq!(api_sample.labels[0].str, "");
    assert_eq!(api_sample.labels[0].num, 42);
    
    // Check second label
    assert_eq!(api_sample.labels[1].key, "thread native id");
    assert_eq!(api_sample.labels[1].num, 12345);
    
    // Check third label
    assert_eq!(api_sample.labels[2].key, "span id");
    assert_eq!(api_sample.labels[2].num, 98765);
}

#[test]
fn test_mixed_label_types() {
    let metadata = Arc::new(Metadata::new(vec![SampleType::Cpu], 64, true).unwrap());
    let mut sample = OwnedSample::new(metadata);
    
    // Mix string and numeric labels
    sample.add_string_label(LabelKey::ThreadName, "worker-1");
    sample.add_num_label(LabelKey::ThreadId, 42);
    sample.add_string_label(LabelKey::ExceptionType, "RuntimeError");
    sample.add_num_label(LabelKey::SpanId, 12345);
    
    assert_eq!(sample.num_labels(), 4);
    
    let api_sample = sample.as_sample();
    assert_eq!(api_sample.labels[0].key, "thread name");
    assert_eq!(api_sample.labels[0].str, "worker-1");
    assert_eq!(api_sample.labels[1].key, "thread id");
    assert_eq!(api_sample.labels[1].num, 42);
    assert_eq!(api_sample.labels[2].key, "exception type");
    assert_eq!(api_sample.labels[2].str, "RuntimeError");
    assert_eq!(api_sample.labels[3].key, "span id");
    assert_eq!(api_sample.labels[3].num, 12345);
}
