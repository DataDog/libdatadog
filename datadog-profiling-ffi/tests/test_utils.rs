use std::sync::atomic::{AtomicUsize, Ordering};

use datadog_profiling::internal::Profile;
use datadog_profiling_ffi::*;
use ddcommon_ffi::{Handle, Slice};
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
