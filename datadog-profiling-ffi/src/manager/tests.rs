use std::ffi::c_void;

use crate::profiles::datatypes::Sample;
use datadog_profiling::internal::Profile;
use tokio_util::sync::CancellationToken;

use super::{ManagedSampleCallbacks, ProfilerManager};
use crate::manager::profiler_manager::ProfilerManagerConfig;
use crate::manager::samples::SendSample;
use ddcommon_ffi::Slice;

extern "C" fn test_cpu_sampler_callback(_profile: *mut Profile) {}

extern "C" fn test_upload_callback(_profile: *mut Profile, _token: &mut Option<CancellationToken>) {
}

extern "C" fn test_converter(sample: &SendSample) -> Sample<'static> {
    static VALUES: [i64; 1] = [42];
    Sample {
        locations: Slice::empty(),
        values: Slice::from(&VALUES[..]),
        labels: Slice::empty(),
    }
}

extern "C" fn test_reset(_sample: &mut SendSample) {}

extern "C" fn test_drop(_sample: SendSample) {}

#[test]
fn test_profiler_manager() {
    let config = ProfilerManagerConfig {
        channel_depth: 1,
        cpu_sampling_interval_ms: 100, // 100ms for faster testing
        upload_interval_ms: 500,       // 500ms for faster testing
    };

    let sample_callbacks = ManagedSampleCallbacks::new(test_converter, test_reset, test_drop);

    let profile = Profile::new(&[], None);
    let client = ProfilerManager::start(
        profile,
        test_cpu_sampler_callback,
        test_upload_callback,
        sample_callbacks,
        config,
    )
    .unwrap();

    // Send a sample
    let sample_ptr = Box::into_raw(Box::new(42)) as *mut c_void;
    unsafe {
        client.send_sample(sample_ptr).unwrap();
    }

    // Receive a recycled sample
    let recycled = client.try_recv_recycled().unwrap();
    assert_eq!(unsafe { *(recycled as *const i32) }, 42);

    // Shutdown
    let _profile = client.shutdown().unwrap();
}
