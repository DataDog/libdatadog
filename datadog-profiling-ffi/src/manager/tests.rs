use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::LazyLock;

use crate::profiles::datatypes::Sample;
use datadog_profiling::api::ValueType;
use datadog_profiling::internal::Profile;
use tokio_util::sync::CancellationToken;

use super::{ManagedSampleCallbacks, ProfilerManager};
use crate::manager::profiler_manager::ProfilerManagerConfig;
use crate::manager::samples::SendSample;
use ddcommon_ffi::Slice;

extern "C" fn test_cpu_sampler_callback(_profile: *mut Profile) {}

static UPLOAD_COUNT: AtomicUsize = AtomicUsize::new(0);
static SAMPLE_COUNT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn test_upload_callback(
    profile: *mut Profile,
    _token: &mut Option<CancellationToken>,
) {
    let profile = unsafe { &*profile };
    let count = profile.only_for_testing_num_aggregated_samples();
    SAMPLE_COUNT.store(count, Ordering::SeqCst);
    UPLOAD_COUNT.fetch_add(1, Ordering::SeqCst);
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
        }.into(),
        system_name: match value {
            42 => "function_1",
            43 => "function_2",
            44 => "function_3",
            45 => "function_4",
            46 => "function_5",
            _ => "unknown_function",
        }.into(),
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

extern "C" fn test_reset(_sample: &mut SendSample) {}

extern "C" fn test_drop(_sample: SendSample) {}

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
        test_cpu_sampler_callback,
        test_upload_callback,
        sample_callbacks,
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
    assert_eq!(SAMPLE_COUNT.load(Ordering::SeqCst), 5);

    // Shutdown
    let _profile = client.shutdown().unwrap();
}
