use std::sync::atomic::{AtomicUsize, Ordering};

use datadog_profiling::internal::Profile;
use datadog_profiling_ffi::*;
use datadog_profiling_protobuf::prost_impls::Profile as ProstProfile;
use ddcommon_ffi::{Handle, Slice};
use prost::Message;
use tokio_util::sync::CancellationToken;

pub extern "C" fn test_cpu_sampler_callback(_profile: *mut Profile) {}

pub static UPLOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

pub extern "C" fn test_upload_callback(
    _profile: *mut Handle<Profile>,
    _token: &mut std::option::Option<CancellationToken>,
) {
    let upload_count = UPLOAD_COUNT.fetch_add(1, Ordering::SeqCst);
    println!("[upload_callback] called, count: {upload_count}");
}

#[repr(C)]
pub struct TestSample<'a> {
    pub values: [i64; 1],
    pub locations: [profiles::datatypes::Location<'a>; 1],
}

#[allow(dead_code)]
pub fn create_test_sample(value: i64) -> TestSample<'static> {
    let function = profiles::datatypes::Function {
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
        locations: [profiles::datatypes::Location {
            function,
            ..Default::default()
        }],
    }
}

pub extern "C" fn test_converter(sample: &SendSample) -> profiles::datatypes::Sample {
    let test_sample = unsafe { &*(sample.as_ptr() as *const TestSample) };

    profiles::datatypes::Sample {
        locations: Slice::from(&test_sample.locations[..]),
        values: Slice::from(&test_sample.values[..]),
        labels: Slice::empty(),
    }
}

pub extern "C" fn test_reset(sample: &mut SendSample) {
    let test_sample = unsafe { &mut *(sample.as_ptr() as *mut TestSample) };
    test_sample.values[0] = 0;
    test_sample.locations[0] = profiles::datatypes::Location {
        function: profiles::datatypes::Function {
            name: "".into(),
            system_name: "".into(),
            filename: "".into(),
            ..Default::default()
        },
        ..Default::default()
    };
}

pub extern "C" fn test_drop(sample: SendSample) {
    let _test_sample = unsafe { Box::from_raw(sample.as_ptr() as *mut TestSample) };
    // Box will be dropped here, freeing the memory
}

// --- Helpers for profile sample checking ---

pub fn decode_pprof(encoded: &[u8]) -> ProstProfile {
    let mut decoder = lz4_flex::frame::FrameDecoder::new(encoded);
    let mut buf = std::vec::Vec::new();
    use std::io::Read;
    decoder.read_to_end(&mut buf).unwrap();
    ProstProfile::decode(buf.as_slice()).unwrap()
}

pub fn roundtrip_to_pprof(
    profile: std::result::Result<Box<datadog_profiling::internal::Profile>, anyhow::Error>,
) -> ProstProfile {
    let encoded = (*profile.expect("Failed to extract profile"))
        .serialize_into_compressed_pprof(None, None)
        .unwrap();
    decode_pprof(&encoded.buffer)
}

pub fn assert_profile_has_sample_values(
    profile: std::result::Result<Box<datadog_profiling::internal::Profile>, anyhow::Error>,
    expected_values: &[i64],
) {
    let pprof = roundtrip_to_pprof(profile);
    let mut found = vec![false; expected_values.len()];
    for sample in &pprof.samples {
        for (i, &expected) in expected_values.iter().enumerate() {
            if sample.values.contains(&expected) {
                found[i] = true;
            }
        }
    }
    for (i, &was_found) in found.iter().enumerate() {
        assert!(
            was_found,
            "Expected sample value {} not found in profile",
            expected_values[i]
        );
    }
}
