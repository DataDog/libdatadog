// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::profiles::datatypes::Sample;
use datadog_profiling::api::ValueType;
use datadog_profiling::internal::Profile;
use ddcommon_ffi::{Handle, ToInner};
use tokio_util::sync::CancellationToken;

use super::profiler_manager::{
    ManagedSampleCallbacks, ManagerCallbacks, ProfilerManager, ProfilerManagerConfig,
};
use crate::manager::samples::SendSample;
use datadog_profiling_protobuf::prost_impls::Profile as ProstProfile;
use ddcommon_ffi::Slice;
use prost::Message;

extern "C" fn test_cpu_sampler_callback(_profile: *mut Profile) {}

static UPLOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn test_upload_callback(
    profile: *mut Handle<Profile>,
    _token: &mut Option<CancellationToken>,
) {
    let upload_count = UPLOAD_COUNT.fetch_add(1, Ordering::SeqCst);

    // On the first upload (when upload_count is 0), verify the samples
    if upload_count == 0 {
        let profile = unsafe { *(*profile).take().unwrap() };
        verify_samples(profile);
    }
}

#[repr(C)]
struct TestSample<'a> {
    values: [i64; 1],
    locations: [crate::profiles::datatypes::Location<'a>; 1],
}

fn create_test_sample(value: i64) -> TestSample<'static> {
    let function = crate::profiles::datatypes::Function {
        name: match value {
            42 => "function_1",
            43 => "function_2",
            44 => "function_3",
            45 => "function_4",
            46 => "function_5",
            _ => "unknown_function",
        }
        .into(),
        system_name: match value {
            42 => "function_1",
            43 => "function_2",
            44 => "function_3",
            45 => "function_4",
            46 => "function_5",
            _ => "unknown_function",
        }
        .into(),
        filename: "test.rs".into(),
        ..Default::default()
    };

    TestSample {
        values: [value],
        locations: [crate::profiles::datatypes::Location {
            function,
            ..Default::default()
        }],
    }
}

extern "C" fn test_converter(sample: &SendSample) -> Sample {
    let test_sample = unsafe { &*(sample.as_ptr() as *const TestSample) };

    Sample {
        locations: Slice::from(&test_sample.locations[..]),
        values: Slice::from(&test_sample.values[..]),
        labels: Slice::empty(),
    }
}

extern "C" fn test_reset(sample: &mut SendSample) {
    let test_sample = unsafe { &mut *(sample.as_ptr() as *mut TestSample) };
    test_sample.values[0] = 0;
    test_sample.locations[0] = crate::profiles::datatypes::Location {
        function: crate::profiles::datatypes::Function {
            name: "".into(),
            system_name: "".into(),
            filename: "".into(),
            ..Default::default()
        },
        ..Default::default()
    };
}

extern "C" fn test_drop(sample: SendSample) {
    let test_sample = unsafe { Box::from_raw(sample.as_ptr() as *mut TestSample) };
    // Box will be dropped here, freeing the memory
}

fn decode_pprof(encoded: &[u8]) -> ProstProfile {
    let mut decoder = lz4_flex::frame::FrameDecoder::new(encoded);
    let mut buf = Vec::new();
    use std::io::Read;
    decoder.read_to_end(&mut buf).unwrap();
    ProstProfile::decode(buf.as_slice()).unwrap()
}

fn roundtrip_to_pprof(profile: datadog_profiling::internal::Profile) -> ProstProfile {
    let encoded = profile.serialize_into_compressed_pprof(None, None).unwrap();
    decode_pprof(&encoded.buffer)
}

fn string_table_fetch(profile: &ProstProfile, id: i64) -> &str {
    profile
        .string_table
        .get(id as usize)
        .map(|s| s.as_str())
        .unwrap_or("")
}

fn verify_samples(profile: datadog_profiling::internal::Profile) {
    let pprof = roundtrip_to_pprof(profile);
    println!("Number of samples in profile: {}", pprof.samples.len());
    println!(
        "Sample values: {:?}",
        pprof
            .samples
            .iter()
            .map(|s| s.values[0])
            .collect::<Vec<_>>()
    );
    assert_eq!(pprof.samples.len(), 5);

    // Sort samples by their first value
    let mut samples = pprof.samples.clone();
    samples.sort_by_key(|s| s.values[0]);

    // Check each sample's value and function name
    for (i, sample) in samples.iter().enumerate() {
        let value = 42 + i as i64;
        assert_eq!(sample.values[0], value);

        // Get the function name from the location
        let location_id = sample.location_ids[0];
        let location = pprof
            .locations
            .iter()
            .find(|l| l.id == location_id)
            .unwrap();
        let function_id = location.lines[0].function_id;
        let function = pprof
            .functions
            .iter()
            .find(|f| f.id == function_id)
            .unwrap();
        let function_name = string_table_fetch(&pprof, function.name);

        let expected_function = match value {
            42 => "function_1",
            43 => "function_2",
            44 => "function_3",
            45 => "function_4",
            46 => "function_5",
            _ => "unknown_function",
        };
        assert_eq!(function_name, expected_function);
    }
}

#[test]
fn test_profiler_manager() {
    let config = ProfilerManagerConfig {
        channel_depth: 10,
        cpu_sampling_interval_ms: 100, // 100ms for faster testing
        upload_interval_ms: 500,       // 500ms for faster testing
    };

    let sample_callbacks = ManagedSampleCallbacks::new(test_converter, test_reset, test_drop);

    let sample_types = [ValueType::new("samples", "count")];
    let profile = Profile::new(&sample_types, None);
    let client = ProfilerManager::start(
        profile,
        ManagerCallbacks {
            cpu_sampler_callback: test_cpu_sampler_callback,
            upload_callback: test_upload_callback,
            sample_callbacks,
        },
        config,
    )
    .unwrap();

    // Send multiple samples
    for i in 0..5 {
        let test_sample = create_test_sample(42 + i as i64);
        let sample_ptr = Box::into_raw(Box::new(test_sample)) as *mut c_void;
        unsafe {
            client.send_sample(sample_ptr).unwrap();
        }
    }

    // Give the manager thread time to process samples and trigger an upload
    std::thread::sleep(std::time::Duration::from_millis(600));

    // Verify samples were uploaded
    assert_eq!(UPLOAD_COUNT.load(Ordering::SeqCst), 1);

    // Get the profile and verify it has no samples (they were consumed by the upload)
    let profile = ProfilerManager::terminate().unwrap();
    let pprof = roundtrip_to_pprof(profile);
    assert_eq!(pprof.samples.len(), 0);
}
