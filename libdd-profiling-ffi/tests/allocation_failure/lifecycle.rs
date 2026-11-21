// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 lifecycle scenarios: the profile and exporter calls a .NET profiler makes on every
//! upload cycle.
//!
//! They go through the C ABI, like a native caller. The FFI modules of this crate are private,
//! so [`crate::ffi`] mirrors their `#[repr(C)]` types and imports the exported symbols. Every input
//! is built in the setup, outside the scope, so the scope only sees the call under test.
//!
//! Each scenario checks a post-failure contract once the scope has ended:
//! - a failed constructor publishes no handle;
//! - a profile is still usable after a failed add or serialize;
//! - `send_blocking` takes the encoded profile even when it fails, and the exporter can send again.
//!
//! The exporter sends to a local agent served by a thread of the worker. The file endpoint is
//! not used: each exporter starts a dump server whose socket has a random name in `/tmp`, which
//! makes the trace nondeterministic and leaves one socket file per worker.

use crate::agent::local_agent;
use crate::ffi;
use crate::scenarios::{Scenario, ScenarioError};
use libdd_common::tag::Tag;
use libdd_common_ffi::{CharSlice, Handle, Slice, ToInner};
use std::any::Any;
use std::num::NonZeroI64;

use ffi::{EncodedProfile, ProfileExporter};

pub const PROFILE_NEW: &str = "profile_new";
pub const PROFILE_ADD_FIRST: &str = "profile_add_first";
pub const PROFILE_ADD_INTERNED: &str = "profile_add_interned";
pub const PROFILE_ADD_NEW_STRINGS: &str = "profile_add_new_strings";
pub const PROFILE_ADD_TIMESTAMPED: &str = "profile_add_timestamped";
pub const PROFILE_ADD_GROWTH: &str = "profile_add_growth";
pub const PROFILE_SERIALIZE_EMPTY: &str = "profile_serialize_empty";
pub const PROFILE_SERIALIZE_POPULATED: &str = "profile_serialize_populated";
pub const PROFILE_SERIALIZE_ENDPOINTS: &str = "profile_serialize_endpoints";
pub const PROFILE_SERIALIZE_UPSCALING: &str = "profile_serialize_upscaling";
pub const PROFILE_SERIALIZE_LARGE: &str = "profile_serialize_large";
pub const EXPORTER_NEW: &str = "exporter_new";
pub const EXPORTER_SEND_COLD: &str = "exporter_send_cold";
pub const EXPORTER_SEND_WARM: &str = "exporter_send_warm";

pub const SET_ENDPOINT_FIRST: &str = "set_endpoint_first";
pub const SET_ENDPOINT_INTERNED: &str = "set_endpoint_interned";
pub const SET_ENDPOINT_NEW: &str = "set_endpoint_new";
pub const SET_ENDPOINT_GROWTH: &str = "set_endpoint_growth";
pub const ENDPOINT_COUNT_FIRST: &str = "endpoint_count_first";
pub const ENDPOINT_COUNT_EXISTING: &str = "endpoint_count_existing";
pub const ENDPOINT_COUNT_GROWTH: &str = "endpoint_count_growth";
pub const UPSCALING_PROPORTIONAL_FIRST: &str = "upscaling_proportional_first";
pub const UPSCALING_PROPORTIONAL_AGAIN: &str = "upscaling_proportional_again";
pub const UPSCALING_POISSON_FIRST: &str = "upscaling_poisson_first";
pub const UPSCALING_POISSON_AGAIN: &str = "upscaling_poisson_again";

pub const CONTRACT_ENDPOINT_AGENT: &str = "contract_endpoint_agent";
pub const CONTRACT_ENDPOINT_AGENTLESS: &str = "contract_endpoint_agentless";
pub const CONTRACT_ENCODED_BYTES: &str = "contract_encoded_bytes";
pub const CONTRACT_VEC_TAG_NEW: &str = "contract_vec_tag_new";
pub const CONTRACT_DROP_PROFILE: &str = "contract_drop_profile";
pub const CONTRACT_DROP_ENCODED: &str = "contract_drop_encoded";
pub const CONTRACT_DROP_EXPORTER: &str = "contract_drop_exporter";
pub const CONTRACT_DROP_ERROR: &str = "contract_drop_error";
pub const CONTRACT_DROP_VEC_TAG: &str = "contract_drop_vec_tag";
pub const CONTRACT_DROP_CANCEL: &str = "contract_drop_cancel";

/// Mappings or counts already stored before the growth call.
///
/// `HashMap::try_reserve(1)` grows when `len` is 0, 3, 7, 14 or 28, so 28 leaves the table
/// exactly full: the call under test is the one that grows it.
const METADATA_GROWTH: u32 = 28;
const LABEL_NAME: &str = "thread name";
const LABEL_VALUE: &str = ".NET ThreadPool Worker";
const OFFSETS_WIDE: [usize; 2] = [0, 1];
const OFFSETS_HEAD: [usize; 1] = [0];
const OFFSETS_TAIL: [usize; 1] = [2];

/// Distinct samples already in the profile before the growth sample is added.
pub const GROWTH_SAMPLES: u32 = 28;
/// Distinct samples in the large profile.
pub const LARGE_SAMPLES: u32 = 256;
/// Distinct samples in the populated profiles.
const POPULATED_SAMPLES: u32 = 8;

const SAMPLE_TYPES: [ffi::SampleType; 4] = [
    ffi::SampleType::CpuTime,
    ffi::SampleType::CpuSamples,
    ffi::SampleType::WallTime,
    ffi::SampleType::WallSamples,
];
const PERIOD: ffi::Period = ffi::Period {
    sample_type: ffi::SampleType::WallTime,
    value: 10_000_000,
};
const ENDPOINT: &str = "GET /orders/{id}";

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn error_text(mut error: libdd_common_ffi::Error) -> String {
    let text = error.to_string();
    // SAFETY: `error` is a valid error returned by the FFI.
    unsafe { libdd_common_ffi::ddog_Error_drop(Some(&mut error)) };
    text
}

fn drop_error(error: libdd_common_ffi::Error) -> ScenarioError {
    let mut error = error;
    // SAFETY: `error` is a valid error returned by the FFI.
    unsafe { libdd_common_ffi::ddog_Error_drop(Some(&mut error)) };
    ScenarioError::Oom
}

/// A .NET-shaped sample: managed frames in the `|lm: |ns: |ct: |fn:` format, wall and CPU
/// values, and the labels the .NET profiler sets (thread, app domain, span IDs).
struct DotnetSample {
    locations: Vec<ffi::Location<'static>>,
    values: Vec<i64>,
    labels: Vec<ffi::Label<'static>>,
}

impl DotnetSample {
    /// The runtime frames and some label values are shared by every variant. The application
    /// frames and the thread and span labels are unique to `variant`: new strings for a profile.
    fn new(variant: u32) -> Self {
        let mapping = |filename: &'static str| ffi::Mapping {
            filename: CharSlice::from(filename),
            ..Default::default()
        };
        let location =
            |mapping: ffi::Mapping<'static>, name: &'static str, line: i64| ffi::Location {
                mapping,
                function: ffi::Function {
                    name: CharSlice::from(name),
                    ..Default::default()
                },
                line,
                ..Default::default()
            };
        let app = mapping("MyApp.dll");
        let corelib = mapping("System.Private.CoreLib.dll");
        let mut locations = Vec::new();
        for depth in 0..4 {
            let name = leak(format!(
                "|lm:MyApp |ns:MyApp.Orders |ct:OrderService{variant} |fn:Step{depth}"
            ));
            locations.push(location(app, name, 10 + depth));
        }
        for name in [
            "|lm:System.Private.CoreLib |ns:System.Threading |ct:ExecutionContext |fn:RunInternal",
            "|lm:System.Private.CoreLib |ns:System.Threading |ct:ThreadPoolWorkQueue |fn:Dispatch",
            "|lm:System.Private.CoreLib |ns:System.Threading |ct:PortableThreadPool+WorkerThread \
             |fn:WorkerThreadStart",
            "|lm:System.Private.CoreLib |ns:System.Threading |ct:Thread |fn:StartCallback",
        ] {
            locations.push(location(corelib, name, 0));
        }
        let str_label = |key: &'static str, value: &'static str| ffi::Label {
            key: CharSlice::from(key),
            str: CharSlice::from(value),
            ..Default::default()
        };
        let num_label = |key: &'static str, num: i64| ffi::Label {
            key: CharSlice::from(key),
            num,
            ..Default::default()
        };
        let labels = vec![
            str_label(
                "thread id",
                leak(format!("<0x{:x}> [#{variant}]", 0x1a2b + variant)),
            ),
            str_label("thread name", ".NET ThreadPool Worker"),
            str_label("appdomain name", "MyApp"),
            num_label("pid", 4242),
            num_label("local root span id", local_root_span_id(variant) as i64),
            num_label("span id", 2_000 + i64::from(variant)),
        ];
        DotnetSample {
            locations,
            values: vec![9_000_000, 1, 10_000_000, 1],
            labels,
        }
    }

    fn as_ffi(&self) -> ffi::Sample<'_> {
        ffi::Sample {
            locations: Slice::from(self.locations.as_slice()),
            values: Slice::from(self.values.as_slice()),
            labels: Slice::from(self.labels.as_slice()),
        }
    }
}

fn local_root_span_id(variant: u32) -> u64 {
    1_000 + u64::from(variant)
}

fn new_profile() -> Result<ffi::Profile, ScenarioError> {
    // SAFETY: the slice and the period are valid for the duration of the call.
    match unsafe { ffi::ddog_prof_Profile_new(Slice::from(&SAMPLE_TYPES[..]), Some(&PERIOD)) } {
        ffi::ProfileNewResult::Ok(profile) => Ok(profile),
        ffi::ProfileNewResult::Err(error) => Err(drop_error(error)),
    }
}

fn add(
    profile: &mut ffi::Profile,
    sample: &DotnetSample,
    timestamp: Option<NonZeroI64>,
) -> Result<(), String> {
    // SAFETY: `profile` comes from `ddog_prof_Profile_new`, and `sample` borrows live data.
    match unsafe { ffi::ddog_prof_Profile_add(profile, sample.as_ffi(), timestamp) } {
        ffi::ProfileResult::Ok(_) => Ok(()),
        ffi::ProfileResult::Err(error) => Err(error_text(error)),
    }
}

fn serialize(profile: &mut ffi::Profile) -> Result<Handle<EncodedProfile>, String> {
    // SAFETY: `profile` comes from `ddog_prof_Profile_new`.
    match unsafe { ffi::ddog_prof_Profile_serialize(profile, None, None) } {
        ffi::SerializeResult::Ok(encoded) => Ok(encoded),
        ffi::SerializeResult::Err(error) => Err(error_text(error)),
    }
}

fn setup_ok(result: ffi::ProfileResult, what: &str) {
    if let ffi::ProfileResult::Err(error) = result {
        panic!("setup {what}: {}", error_text(error));
    }
}

struct ProfileFixture {
    profile: ffi::Profile,
    /// Sample added by the scenario's call.
    sample: DotnetSample,
    timestamp: Option<NonZeroI64>,
    /// Sample added by the post-failure check.
    probe: DotnetSample,
    encoded: Handle<EncodedProfile>,
}

impl Drop for ProfileFixture {
    fn drop(&mut self) {
        // SAFETY: both come from this API, and the drop functions accept empty handles.
        unsafe {
            ffi::ddog_prof_EncodedProfile_drop(&mut self.encoded);
            ffi::ddog_prof_Profile_drop(&mut self.profile);
        }
    }
}

/// A profile that already holds `variants` distinct samples (variant 0 first).
fn profile_with(variants: u32) -> ProfileFixture {
    let mut profile = new_profile().expect("setup profile");
    for variant in 0..variants {
        add(&mut profile, &DotnetSample::new(variant), None).expect("setup add");
    }
    ProfileFixture {
        profile,
        sample: DotnetSample::new(variants),
        timestamp: None,
        probe: DotnetSample::new(1_000_000),
        encoded: Handle::empty(),
    }
}

fn profile_fixture(fixture: &mut Box<dyn Any>) -> &mut ProfileFixture {
    fixture.downcast_mut::<ProfileFixture>().unwrap()
}

fn run_add(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = profile_fixture(fixture);
    add(&mut fx.profile, &fx.sample, fx.timestamp).map_err(|_| ScenarioError::Oom)
}

fn run_serialize(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = profile_fixture(fixture);
    fx.encoded = serialize(&mut fx.profile).map_err(|_| ScenarioError::Oom)?;
    Ok(())
}

/// The profile is still usable: a new sample can be added, and the profile serialized.
///
/// Contract: provisional. `profiling.h` does not say what state `Profile_add` or
/// `Profile_serialize` leave the profile in after an error.
fn check_profile_usable(fixture: &mut Box<dyn Any>, _failed: bool) -> Result<(), String> {
    let fx = profile_fixture(fixture);
    if fx.profile.inner.is_null() {
        return Err("the profile handle is null".to_string());
    }
    add(&mut fx.profile, &fx.probe, None).map_err(|e| format!("add after the call: {e}"))?;
    let mut encoded =
        serialize(&mut fx.profile).map_err(|e| format!("serialize after the call: {e}"))?;
    // SAFETY: `encoded` comes from `ddog_prof_Profile_serialize`.
    unsafe { ffi::ddog_prof_EncodedProfile_drop(&mut encoded) };
    Ok(())
}

struct NewProfileFixture {
    profile: Option<ffi::Profile>,
    probe: DotnetSample,
}

impl Drop for NewProfileFixture {
    fn drop(&mut self) {
        if let Some(profile) = &mut self.profile {
            // SAFETY: the profile comes from `ddog_prof_Profile_new`.
            unsafe { ffi::ddog_prof_Profile_drop(profile) };
        }
    }
}

/// A failed `Profile_new` publishes no handle; a profile it returns is usable.
///
/// Contract: "must call `ddog_prof_Profile_drop`" on a returned profile is documented. That the
/// error variant carries no profile is undocumented ownership.
fn check_new_profile(fixture: &mut Box<dyn Any>, failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<NewProfileFixture>().unwrap();
    match (&mut fx.profile, failed) {
        (Some(_), true) => Err("an error was returned with a profile".to_string()),
        (None, false) => Err("success without a profile".to_string()),
        (None, true) => Ok(()),
        (Some(profile), false) => {
            if profile.inner.is_null() {
                return Err("Profile_new returned Ok with a null profile".to_string());
            }
            add(profile, &fx.probe, None).map_err(|e| format!("add on the new profile: {e}"))?;
            let mut encoded =
                serialize(profile).map_err(|e| format!("serialize the new profile: {e}"))?;
            // SAFETY: `encoded` comes from `ddog_prof_Profile_serialize`.
            unsafe { ffi::ddog_prof_EncodedProfile_drop(&mut encoded) };
            Ok(())
        }
    }
}

/// A serialized profile with a few samples, endpoints included.
fn encoded_profile() -> Handle<EncodedProfile> {
    let mut fx = profile_with(POPULATED_SAMPLES);
    add_endpoints(&mut fx.profile, POPULATED_SAMPLES);
    serialize(&mut fx.profile).expect("setup serialize")
}

fn add_endpoints(profile: &mut ffi::Profile, variants: u32) {
    for variant in 0..variants {
        // SAFETY: `profile` comes from `ddog_prof_Profile_new`; the slice is live.
        setup_ok(
            unsafe {
                ffi::ddog_prof_Profile_set_endpoint(
                    profile,
                    local_root_span_id(variant),
                    CharSlice::from(ENDPOINT),
                )
            },
            "set_endpoint",
        );
    }
    // SAFETY: as above.
    setup_ok(
        unsafe { ffi::ddog_prof_Profile_add_endpoint_count(profile, CharSlice::from(ENDPOINT), 3) },
        "add_endpoint_count",
    );
}

fn agent_endpoint(url: &'static str) -> ffi::ProfilingEndpoint<'static> {
    ffi::ProfilingEndpoint::Agent(CharSlice::from(url), 10_000, false)
}

fn dotnet_tags() -> libdd_common_ffi::Vec<Tag> {
    let tags: Vec<Tag> = [
        ("service", "my-app"),
        ("env", "prod"),
        ("version", "1.2.3"),
        ("host", "web-1"),
        ("runtime-id", "8f2b6c1e-6a4b-4f7e-9d3c-2b1a0e9f8d7c"),
        ("profiler_version", "3.30.0"),
    ]
    .into_iter()
    .map(|(k, v)| Tag::new(k, v).expect("tag"))
    .collect();
    tags.into()
}

struct ExporterFixture {
    exporter: Handle<ProfileExporter>,
    endpoint: ffi::ProfilingEndpoint<'static>,
    tags: libdd_common_ffi::Vec<Tag>,
    encoded: Handle<EncodedProfile>,
    files: Vec<ffi::File<'static>>,
    additional_tags: libdd_common_ffi::Vec<Tag>,
    process_tags: CharSlice<'static>,
    internal_metadata: CharSlice<'static>,
    info: CharSlice<'static>,
    status: Option<u16>,
}

impl Drop for ExporterFixture {
    fn drop(&mut self) {
        // SAFETY: both come from this API, and the drop functions accept empty handles.
        unsafe {
            ffi::ddog_prof_EncodedProfile_drop(&mut self.encoded);
            ffi::ddog_prof_Exporter_drop(&mut self.exporter);
        }
    }
}

fn exporter_fixture(endpoint: ffi::ProfilingEndpoint<'static>) -> ExporterFixture {
    let attachment: &'static [u8] = Box::leak(
        br#"{"runtime":"dotnet","events":[{"type":"gc","duration_ms":3}]}"#
            .to_vec()
            .into_boxed_slice(),
    );
    let files = vec![ffi::File {
        name: CharSlice::from("metrics.json"),
        file: Slice::from(attachment),
    }];
    let additional_tags: Vec<Tag> = vec![Tag::new("cycle", "42").expect("tag")];
    ExporterFixture {
        exporter: Handle::empty(),
        endpoint,
        tags: dotnet_tags(),
        encoded: Handle::empty(),
        files,
        additional_tags: additional_tags.into(),
        process_tags: CharSlice::from("entrypoint.name:MyApp,entrypoint.type:dotnet"),
        internal_metadata: CharSlice::from(r#"{"no_signals_workaround_enabled":"false"}"#),
        info: CharSlice::from(r#"{"application":{"start_time":"2026-11-20T00:00:00Z"}}"#),
        status: None,
    }
}

fn new_exporter(fx: &ExporterFixture) -> Result<Handle<ProfileExporter>, String> {
    // SAFETY: every slice and the tag vector are live for the duration of the call.
    match unsafe {
        ffi::ddog_prof_Exporter_new(
            CharSlice::from("dd-trace-dotnet"),
            CharSlice::from("3.30.0"),
            CharSlice::from("dotnet"),
            Some(&fx.tags),
            fx.endpoint,
        )
    } {
        libdd_common_ffi::Result::Ok(exporter) => Ok(exporter),
        libdd_common_ffi::Result::Err(error) => Err(error_text(error)),
    }
}

fn send(fx: &mut ExporterFixture) -> Result<u16, String> {
    // SAFETY: the handles come from this API, and every borrowed input is live.
    match unsafe {
        ffi::ddog_prof_Exporter_send_blocking(
            &mut fx.exporter,
            &mut fx.encoded,
            Slice::from(fx.files.as_slice()),
            Some(&fx.additional_tags),
            Some(&fx.process_tags),
            Some(&fx.internal_metadata),
            Some(&fx.info),
            std::ptr::null_mut(),
        )
    } {
        libdd_common_ffi::Result::Ok(status) => Ok(status.0),
        libdd_common_ffi::Result::Err(error) => Err(error_text(error)),
    }
}

fn exporter_ready(endpoint: ffi::ProfilingEndpoint<'static>, warm: bool) -> ExporterFixture {
    let mut fx = exporter_fixture(endpoint);
    fx.exporter = new_exporter(&fx).expect("setup exporter");
    if warm {
        // SAFETY: the exporter comes from `ddog_prof_Exporter_new`.
        let result = unsafe { ffi::ddog_prof_Exporter_init_runtime(&mut fx.exporter) };
        if let libdd_common_ffi::VoidResult::Err(error) = result {
            panic!("setup init_runtime: {}", error_text(error));
        }
        fx.encoded = encoded_profile();
        send(&mut fx).expect("setup send");
    }
    fx.encoded = encoded_profile();
    fx
}

fn run_new_exporter(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<ExporterFixture>().unwrap();
    fx.exporter = new_exporter(fx).map_err(|_| ScenarioError::Oom)?;
    Ok(())
}

fn run_send(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<ExporterFixture>().unwrap();
    fx.status = Some(send(fx).map_err(|_| ScenarioError::Oom)?);
    Ok(())
}

/// The exporter can send a new profile, and the endpoint accepts it.
fn check_exporter_sends(fx: &mut ExporterFixture) -> Result<(), String> {
    fx.encoded = encoded_profile();
    let status = send(fx).map_err(|e| format!("send after the call: {e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("send after the call returned HTTP {status}"));
    }
    Ok(())
}

/// A failed `Exporter_new` publishes no handle; an exporter it returns can send.
///
/// Contract: undocumented ownership (no handle on error) and provisional (it can send).
fn check_new_exporter(fixture: &mut Box<dyn Any>, failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<ExporterFixture>().unwrap();
    let mut handle: *mut Handle<ProfileExporter> = &mut fx.exporter;
    // SAFETY: the handle is either empty or holds the exporter from `ddog_prof_Exporter_new`.
    let published = unsafe { handle.to_inner_mut() }.is_ok();
    match (published, failed) {
        (true, true) => Err("an error was returned with an exporter".to_string()),
        (false, false) => Err("success without an exporter".to_string()),
        (false, true) => Ok(()),
        (true, false) => check_exporter_sends(fx),
    }
}

/// `send_blocking` takes the encoded profile whatever the result, and the exporter can send
/// again.
///
/// Contract: "Takes ownership of the profile" and "Borrows the exporter" are documented. That the
/// exporter can send again after an error is provisional.
fn check_send(fixture: &mut Box<dyn Any>, failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<ExporterFixture>().unwrap();
    let mut handle: *mut Handle<EncodedProfile> = &mut fx.encoded;
    // SAFETY: the handle is either empty or holds the profile from `ddog_prof_Profile_serialize`.
    if let Ok(profile) = unsafe { handle.take() } {
        drop(profile);
        return Err("the encoded profile handle still owns the profile".to_string());
    }
    if !failed && !fx.status.is_some_and(|s| (200..300).contains(&s)) {
        return Err(format!("success with HTTP status {:?}", fx.status));
    }
    check_exporter_sends(fx)
}

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: PROFILE_NEW,
        inputs: "Profile_new: cpu/wall time and samples, wall period 10 ms",
        setup: || {
            Box::new(NewProfileFixture {
                profile: None,
                probe: DotnetSample::new(0),
            })
        },
        run: |fixture| {
            let fx = fixture.downcast_mut::<NewProfileFixture>().unwrap();
            fx.profile = Some(new_profile()?);
            Ok(())
        },
        check: Some(check_new_profile),
    },
    Scenario {
        name: PROFILE_ADD_FIRST,
        inputs: "Profile_add: first .NET sample (8 frames, 6 labels)",
        setup: || Box::new(profile_with(0)),
        run: run_add,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_ADD_INTERNED,
        inputs: "Profile_add: the same sample again, every string interned",
        setup: || {
            let mut fx = profile_with(1);
            fx.sample = DotnetSample::new(0);
            Box::new(fx)
        },
        run: run_add,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_ADD_NEW_STRINGS,
        inputs: "Profile_add: second sample with new frame and label strings",
        setup: || Box::new(profile_with(1)),
        run: run_add,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_ADD_TIMESTAMPED,
        inputs: "Profile_add: interned sample with a timestamp",
        setup: || {
            let mut fx = profile_with(1);
            fx.sample = DotnetSample::new(0);
            fx.timestamp = NonZeroI64::new(1_763_600_000_000_000_000);
            Box::new(fx)
        },
        run: run_add,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_ADD_GROWTH,
        inputs: "Profile_add: 29th distinct sample (collection growth)",
        setup: || Box::new(profile_with(GROWTH_SAMPLES)),
        run: run_add,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_SERIALIZE_EMPTY,
        inputs: "Profile_serialize: no sample",
        setup: || Box::new(profile_with(0)),
        run: run_serialize,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_SERIALIZE_POPULATED,
        inputs: "Profile_serialize: 8 samples",
        setup: || Box::new(profile_with(POPULATED_SAMPLES)),
        run: run_serialize,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_SERIALIZE_ENDPOINTS,
        inputs: "Profile_serialize: 8 samples, 8 endpoint mappings, 1 count",
        setup: || {
            let mut fx = profile_with(POPULATED_SAMPLES);
            add_endpoints(&mut fx.profile, POPULATED_SAMPLES);
            Box::new(fx)
        },
        run: run_serialize,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_SERIALIZE_UPSCALING,
        inputs: "Profile_serialize: 8 samples, 1 proportional upscaling rule",
        setup: || {
            let mut fx = profile_with(POPULATED_SAMPLES);
            let offsets = [0usize, 1];
            // SAFETY: the profile comes from `ddog_prof_Profile_new`; the slices are live.
            setup_ok(
                unsafe {
                    ffi::ddog_prof_Profile_add_upscaling_rule_proportional(
                        &mut fx.profile,
                        Slice::from(&offsets[..]),
                        CharSlice::from("thread name"),
                        CharSlice::from(".NET ThreadPool Worker"),
                        10,
                        100,
                    )
                },
                "add_upscaling_rule_proportional",
            );
            Box::new(fx)
        },
        run: run_serialize,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_SERIALIZE_LARGE,
        inputs: "Profile_serialize: 256 samples, 256 endpoint mappings",
        setup: || {
            let mut fx = profile_with(LARGE_SAMPLES);
            add_endpoints(&mut fx.profile, LARGE_SAMPLES);
            Box::new(fx)
        },
        run: run_serialize,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: EXPORTER_NEW,
        inputs: "Exporter_new: local agent endpoint, 6 .NET tags",
        setup: || Box::new(exporter_fixture(agent_endpoint(local_agent()))),
        run: run_new_exporter,
        check: Some(check_new_exporter),
    },
    Scenario {
        name: EXPORTER_SEND_COLD,
        inputs: "send_blocking: first send (cold runtime), attachment, tags, metadata",
        setup: || Box::new(exporter_ready(agent_endpoint(local_agent()), false)),
        run: run_send,
        check: Some(check_send),
    },
    Scenario {
        name: EXPORTER_SEND_WARM,
        inputs: "send_blocking: second send (runtime already started)",
        setup: || Box::new(exporter_ready(agent_endpoint(local_agent()), true)),
        run: run_send,
        check: Some(check_send),
    },
    Scenario {
        name: SET_ENDPOINT_FIRST,
        inputs: "set_endpoint: first mapping",
        setup: || meta_profile(fresh_profile(), Meta::set(1, ENDPOINT)),
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: SET_ENDPOINT_INTERNED,
        inputs: "set_endpoint: interned endpoint, table full at 3 mappings",
        setup: || {
            // Three mappings fill the table. The call sets the already-interned endpoint on a
            // new span, so only the table grows.
            let mut profile = fresh_profile();
            set_endpoint(&mut profile, 1, ENDPOINT).expect("setup endpoint");
            for n in 0..2 {
                set_endpoint(&mut profile, 100 + u64::from(n), endpoint_name(n))
                    .expect("setup endpoint");
            }
            meta_profile(profile, Meta::set(2, ENDPOINT))
        },
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: SET_ENDPOINT_NEW,
        inputs: "set_endpoint: new endpoint string",
        setup: || {
            let mut profile = fresh_profile();
            set_endpoint(&mut profile, 1, ENDPOINT).expect("setup endpoint");
            meta_profile(profile, Meta::set(2, endpoint_name(1)))
        },
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: SET_ENDPOINT_GROWTH,
        inputs: "set_endpoint: 29th mapping (table growth)",
        setup: || {
            meta_profile(
                profile_with_endpoints(METADATA_GROWTH),
                Meta::set(50_000, endpoint_name(METADATA_GROWTH)),
            )
        },
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: ENDPOINT_COUNT_FIRST,
        inputs: "add_endpoint_count: first count",
        setup: || meta_profile(fresh_profile(), Meta::count(ENDPOINT, 3)),
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: ENDPOINT_COUNT_EXISTING,
        inputs: "add_endpoint_count: increment an existing count",
        setup: || {
            let mut profile = fresh_profile();
            add_count(&mut profile, ENDPOINT, 1).expect("setup count");
            meta_profile(profile, Meta::count(ENDPOINT, 4))
        },
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: ENDPOINT_COUNT_GROWTH,
        inputs: "add_endpoint_count: 29th endpoint (table growth)",
        setup: || {
            meta_profile(
                profile_with_counts(METADATA_GROWTH),
                Meta::count(endpoint_name(METADATA_GROWTH), 1),
            )
        },
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: UPSCALING_PROPORTIONAL_FIRST,
        inputs: "proportional rule: offsets [0, 1] on thread name",
        setup: || {
            meta_profile(
                fresh_profile(),
                Meta::proportional(&OFFSETS_WIDE, LABEL_NAME, LABEL_VALUE),
            )
        },
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: UPSCALING_PROPORTIONAL_AGAIN,
        inputs: "proportional rule: offset [2] on the same label",
        setup: || {
            let mut profile = fresh_profile();
            add_proportional(&mut profile, &OFFSETS_WIDE, LABEL_NAME, LABEL_VALUE)
                .expect("setup rule");
            meta_profile(
                profile,
                Meta::proportional(&OFFSETS_TAIL, LABEL_NAME, LABEL_VALUE),
            )
        },
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: UPSCALING_POISSON_FIRST,
        inputs: "poisson rule: offset [0], sum 0, count 1, distance 1024",
        setup: || {
            meta_profile(
                fresh_profile(),
                Meta::poisson(&OFFSETS_HEAD, LABEL_NAME, LABEL_VALUE, 0, 1, 1_024),
            )
        },
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: UPSCALING_POISSON_AGAIN,
        inputs: "poisson rule: offset [2] on the same label",
        setup: || {
            let mut profile = fresh_profile();
            add_poisson(
                &mut profile,
                &OFFSETS_HEAD,
                LABEL_NAME,
                LABEL_VALUE,
                0,
                1,
                1_024,
            )
            .expect("setup rule");
            meta_profile(
                profile,
                Meta::poisson(&OFFSETS_TAIL, LABEL_NAME, LABEL_VALUE, 2, 3, 1_024),
            )
        },
        run: run_meta,
        check: Some(check_meta),
    },
    Scenario {
        name: CONTRACT_ENDPOINT_AGENT,
        inputs: "Endpoint_agent(http://127.0.0.1:8126)",
        setup: || {
            Box::new(EndpointContract {
                url: "http://127.0.0.1:8126",
                site: "",
                api_key: "",
                endpoint: None,
            })
        },
        run: run_endpoint_agent,
        check: Some(check_endpoint_agent),
    },
    Scenario {
        name: CONTRACT_ENDPOINT_AGENTLESS,
        inputs: "Endpoint_agentless(datadoghq.com, API key)",
        setup: || {
            Box::new(EndpointContract {
                url: "",
                site: "datadoghq.com",
                api_key: "0123456789abcdef0123456789abcdef",
                endpoint: None,
            })
        },
        run: run_endpoint_agentless,
        check: Some(check_endpoint_agentless),
    },
    Scenario {
        name: CONTRACT_ENCODED_BYTES,
        inputs: "EncodedProfile_bytes on a serialized profile",
        setup: || {
            Box::new(BytesContract {
                encoded: encoded_profile(),
                len: None,
            })
        },
        run: run_encoded_bytes,
        check: Some(check_encoded_bytes),
    },
    Scenario {
        name: CONTRACT_VEC_TAG_NEW,
        inputs: "ddog_Vec_Tag_new",
        setup: || Box::new(TagContract { tags: None }),
        run: run_vec_tag_new,
        check: Some(check_vec_tag_new),
    },
    Scenario {
        name: CONTRACT_DROP_PROFILE,
        inputs: "Profile_drop, then again on the emptied profile",
        setup: || {
            Box::new(DropProfile {
                profile: take_profile(profile_with(1)),
            })
        },
        run: run_drop_profile,
        check: Some(check_drop_profile),
    },
    Scenario {
        name: CONTRACT_DROP_ENCODED,
        inputs: "EncodedProfile_drop on a serialized profile",
        setup: || {
            Box::new(DropEncoded {
                encoded: encoded_profile(),
            })
        },
        run: run_drop_encoded,
        check: Some(check_drop_encoded),
    },
    Scenario {
        name: CONTRACT_DROP_EXPORTER,
        inputs: "Exporter_drop on a new exporter",
        setup: || {
            let mut fx = exporter_fixture(agent_endpoint(local_agent()));
            fx.exporter = new_exporter(&fx).expect("setup exporter");
            Box::new(fx)
        },
        run: run_drop_exporter,
        check: Some(check_drop_exporter),
    },
    Scenario {
        name: CONTRACT_DROP_ERROR,
        inputs: "ddog_Error_drop on a profile error",
        setup: || {
            Box::new(DropError {
                error: profile_error(),
            })
        },
        run: run_drop_error,
        check: Some(check_drop_error),
    },
    Scenario {
        name: CONTRACT_DROP_VEC_TAG,
        inputs: "ddog_Vec_Tag_drop on a populated vector",
        setup: || {
            Box::new(DropTags {
                tags: Some(populated_tags()),
            })
        },
        run: run_drop_tags,
        check: Some(check_drop_tags),
    },
    Scenario {
        name: CONTRACT_DROP_CANCEL,
        inputs: "CancellationToken_drop on a new token",
        setup: || {
            Box::new(DropCancel {
                token: unsafe { ffi::ddog_CancellationToken_new() },
            })
        },
        run: run_drop_cancel,
        check: Some(check_drop_cancel),
    },
];

// ---------------------------------------------------------------------------------------------
// Tier 2 metadata
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Meta {
    SetEndpoint {
        span: u64,
        endpoint: &'static str,
    },
    Count {
        endpoint: &'static str,
        value: i64,
    },
    Proportional {
        offsets: &'static [usize],
        name: &'static str,
        value: &'static str,
    },
    Poisson {
        offsets: &'static [usize],
        name: &'static str,
        value: &'static str,
        sum: usize,
        count: usize,
        distance: u64,
    },
}

impl Meta {
    fn set(span: u64, endpoint: &'static str) -> Self {
        Meta::SetEndpoint { span, endpoint }
    }

    fn count(endpoint: &'static str, value: i64) -> Self {
        Meta::Count { endpoint, value }
    }

    fn proportional(offsets: &'static [usize], name: &'static str, value: &'static str) -> Self {
        Meta::Proportional {
            offsets,
            name,
            value,
        }
    }

    fn poisson(
        offsets: &'static [usize],
        name: &'static str,
        value: &'static str,
        sum: usize,
        count: usize,
        distance: u64,
    ) -> Self {
        Meta::Poisson {
            offsets,
            name,
            value,
            sum,
            count,
            distance,
        }
    }
}

struct MetaFx {
    profile: ffi::Profile,
    call: Meta,
    probe: DotnetSample,
    encoded: Handle<EncodedProfile>,
}

impl Drop for MetaFx {
    fn drop(&mut self) {
        // SAFETY: both come from this API, and the drop functions accept empty handles.
        unsafe {
            ffi::ddog_prof_EncodedProfile_drop(&mut self.encoded);
            ffi::ddog_prof_Profile_drop(&mut self.profile);
        }
    }
}

fn meta_profile(profile: ffi::Profile, call: Meta) -> Box<MetaFx> {
    Box::new(MetaFx {
        profile,
        call,
        probe: DotnetSample::new(1_000_000),
        encoded: Handle::empty(),
    })
}

fn endpoint_name(n: u32) -> &'static str {
    leak(format!("GET /items/{n}"))
}

fn fresh_profile() -> ffi::Profile {
    new_profile().expect("setup profile")
}

/// Moves the profile out and leaves the fixture's handle null, so its drop is a no-op.
fn take_profile(mut fixture: ProfileFixture) -> ffi::Profile {
    let profile = ffi::Profile {
        inner: fixture.profile.inner,
    };
    fixture.profile.inner = std::ptr::null_mut();
    profile
}

fn profile_with_endpoints(n: u32) -> ffi::Profile {
    let mut profile = fresh_profile();
    for i in 0..n {
        set_endpoint(&mut profile, 10_000 + u64::from(i), endpoint_name(i))
            .expect("setup endpoint");
    }
    profile
}

fn profile_with_counts(n: u32) -> ffi::Profile {
    let mut profile = fresh_profile();
    for i in 0..n {
        add_count(&mut profile, endpoint_name(i), 1).expect("setup count");
    }
    profile
}

fn set_endpoint(profile: &mut ffi::Profile, span: u64, endpoint: &str) -> Result<(), String> {
    // SAFETY: `profile` comes from `ddog_prof_Profile_new`; the slice is live.
    match unsafe { ffi::ddog_prof_Profile_set_endpoint(profile, span, CharSlice::from(endpoint)) } {
        ffi::ProfileResult::Ok(_) => Ok(()),
        ffi::ProfileResult::Err(error) => Err(error_text(error)),
    }
}

fn add_count(profile: &mut ffi::Profile, endpoint: &str, value: i64) -> Result<(), String> {
    // SAFETY: `profile` comes from `ddog_prof_Profile_new`; the slice is live.
    match unsafe {
        ffi::ddog_prof_Profile_add_endpoint_count(profile, CharSlice::from(endpoint), value)
    } {
        ffi::ProfileResult::Ok(_) => Ok(()),
        ffi::ProfileResult::Err(error) => Err(error_text(error)),
    }
}

fn add_proportional(
    profile: &mut ffi::Profile,
    offsets: &[usize],
    name: &str,
    value: &str,
) -> Result<(), String> {
    // SAFETY: `profile` comes from `ddog_prof_Profile_new`; the slices are live.
    match unsafe {
        ffi::ddog_prof_Profile_add_upscaling_rule_proportional(
            profile,
            Slice::from(offsets),
            CharSlice::from(name),
            CharSlice::from(value),
            10,
            100,
        )
    } {
        ffi::ProfileResult::Ok(_) => Ok(()),
        ffi::ProfileResult::Err(error) => Err(error_text(error)),
    }
}

fn add_poisson(
    profile: &mut ffi::Profile,
    offsets: &[usize],
    name: &str,
    value: &str,
    sum: usize,
    count: usize,
    distance: u64,
) -> Result<(), String> {
    // SAFETY: `profile` comes from `ddog_prof_Profile_new`; the slices are live.
    match unsafe {
        ffi::ddog_prof_Profile_add_upscaling_rule_poisson(
            profile,
            Slice::from(offsets),
            CharSlice::from(name),
            CharSlice::from(value),
            sum,
            count,
            distance,
        )
    } {
        ffi::ProfileResult::Ok(_) => Ok(()),
        ffi::ProfileResult::Err(error) => Err(error_text(error)),
    }
}

fn apply_meta(profile: &mut ffi::Profile, call: Meta) -> Result<(), String> {
    match call {
        Meta::SetEndpoint { span, endpoint } => set_endpoint(profile, span, endpoint),
        Meta::Count { endpoint, value } => add_count(profile, endpoint, value),
        Meta::Proportional {
            offsets,
            name,
            value,
        } => add_proportional(profile, offsets, name, value),
        Meta::Poisson {
            offsets,
            name,
            value,
            sum,
            count,
            distance,
        } => add_poisson(profile, offsets, name, value, sum, count, distance),
    }
}

fn meta_fx(fixture: &mut Box<dyn Any>) -> &mut MetaFx {
    fixture.downcast_mut::<MetaFx>().unwrap()
}

fn run_meta(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = meta_fx(fixture);
    apply_meta(&mut fx.profile, fx.call).map_err(|_| ScenarioError::Oom)
}

/// After a failure the same call can be retried, and the profile can take a sample.
///
/// Contract: provisional. The header documents no error state for these calls.
fn check_meta(fixture: &mut Box<dyn Any>, failed: bool) -> Result<(), String> {
    let fx = meta_fx(fixture);
    if fx.profile.inner.is_null() {
        return Err("the profile handle is null".to_string());
    }
    if failed {
        apply_meta(&mut fx.profile, fx.call)
            .map_err(|e| format!("the call cannot be retried: {e}"))?;
    }
    add(&mut fx.profile, &fx.probe, None).map_err(|e| format!("add after the call: {e}"))?;
    let mut encoded =
        serialize(&mut fx.profile).map_err(|e| format!("serialize after the call: {e}"))?;
    // SAFETY: `encoded` comes from `ddog_prof_Profile_serialize`.
    unsafe { ffi::ddog_prof_EncodedProfile_drop(&mut encoded) };
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Contract-only calls: enumerated only when the baseline allocates.
// ---------------------------------------------------------------------------------------------

fn same_chars(slice: CharSlice, text: &str) -> bool {
    let (ptr, len) = slice.as_raw_parts();
    len == text.len() && ptr.cast() == text.as_ptr()
}

struct EndpointContract {
    url: &'static str,
    site: &'static str,
    api_key: &'static str,
    endpoint: Option<ffi::ProfilingEndpoint<'static>>,
}

fn run_endpoint_agent(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<EndpointContract>().unwrap();
    // SAFETY: the slice is live for the duration of the call, and the result borrows it.
    fx.endpoint =
        Some(unsafe { ffi::ddog_prof_Endpoint_agent(CharSlice::from(fx.url), 10_000, false) });
    Ok(())
}

fn check_endpoint_agent(fixture: &mut Box<dyn Any>, failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<EndpointContract>().unwrap();
    if failed {
        return Err("Endpoint_agent does not report errors".to_string());
    }
    match fx.endpoint {
        Some(ffi::ProfilingEndpoint::Agent(url, timeout, resolver))
            if same_chars(url, fx.url) && timeout == 10_000 && !resolver =>
        {
            Ok(())
        }
        _ => Err("Endpoint_agent did not return the url, timeout and resolver".to_string()),
    }
}

fn run_endpoint_agentless(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<EndpointContract>().unwrap();
    // SAFETY: both slices are live for the duration of the call, and the result borrows them.
    fx.endpoint = Some(unsafe {
        ffi::ddog_prof_Endpoint_agentless(
            CharSlice::from(fx.site),
            CharSlice::from(fx.api_key),
            10_000,
            false,
        )
    });
    Ok(())
}

fn check_endpoint_agentless(fixture: &mut Box<dyn Any>, failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<EndpointContract>().unwrap();
    if failed {
        return Err("Endpoint_agentless does not report errors".to_string());
    }
    match fx.endpoint {
        Some(ffi::ProfilingEndpoint::Agentless(site, key, timeout, resolver))
            if same_chars(site, fx.site)
                && same_chars(key, fx.api_key)
                && timeout == 10_000
                && !resolver =>
        {
            Ok(())
        }
        _ => {
            Err("Endpoint_agentless did not return the site, key, timeout and resolver".to_string())
        }
    }
}

struct BytesContract {
    encoded: Handle<EncodedProfile>,
    len: Option<usize>,
}

impl Drop for BytesContract {
    fn drop(&mut self) {
        // SAFETY: the handle comes from `ddog_prof_Profile_serialize`, or it is empty.
        unsafe { ffi::ddog_prof_EncodedProfile_drop(&mut self.encoded) };
    }
}

fn encoded_owned(encoded: &mut Handle<EncodedProfile>) -> bool {
    let mut handle: *mut Handle<EncodedProfile> = encoded;
    // SAFETY: the handle is empty or holds the profile from `ddog_prof_Profile_serialize`.
    unsafe { handle.to_inner_mut() }.is_ok()
}

fn run_encoded_bytes(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<BytesContract>().unwrap();
    // SAFETY: the handle comes from `ddog_prof_Profile_serialize`.
    match unsafe { ffi::ddog_prof_EncodedProfile_bytes(&mut fx.encoded) } {
        libdd_common_ffi::Result::Ok(bytes) => {
            let (ptr, len) = bytes.as_raw_parts();
            if ptr.is_null() && len > 0 {
                return Err(ScenarioError::Rejected);
            }
            fx.len = Some(len);
            Ok(())
        }
        libdd_common_ffi::Result::Err(error) => {
            drop_error(error);
            Err(ScenarioError::Oom)
        }
    }
}

/// `bytes` borrows the encoded profile: the handle still owns it afterwards.
///
/// Contract: documented ("valid for use until the encoded_profile is modified").
fn check_encoded_bytes(fixture: &mut Box<dyn Any>, failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<BytesContract>().unwrap();
    if !encoded_owned(&mut fx.encoded) {
        return Err("EncodedProfile_bytes took the handle".to_string());
    }
    if !failed {
        match fx.len {
            Some(len) if len > 0 => {}
            _ => return Err("EncodedProfile_bytes returned an empty slice".to_string()),
        }
    }
    Ok(())
}

struct TagContract {
    tags: Option<libdd_common_ffi::Vec<Tag>>,
}

fn run_vec_tag_new(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<TagContract>().unwrap();
    fx.tags = Some(libdd_common_ffi::tags::ddog_Vec_Tag_new());
    Ok(())
}

/// A new tag vector is empty, and a tag can be pushed into it.
///
/// Contract: provisional. `ddog_Vec_Tag_new` has no doc comment.
fn check_vec_tag_new(fixture: &mut Box<dyn Any>, _failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<TagContract>().unwrap();
    let tags = fx.tags.as_mut().ok_or("Vec_Tag_new returned nothing")?;
    if !tags.is_empty() {
        return Err(format!("Vec_Tag_new returned {} tags", tags.len()));
    }
    // SAFETY: `tags` comes from `ddog_Vec_Tag_new`; both slices are live.
    let result = unsafe {
        libdd_common_ffi::tags::ddog_Vec_Tag_push(
            tags,
            CharSlice::from("env"),
            CharSlice::from("prod"),
        )
    };
    if matches!(result, libdd_common_ffi::tags::PushTagResult::Ok) && tags.len() == 1 {
        Ok(())
    } else {
        Err("a tag cannot be pushed into the new vector".to_string())
    }
}

struct DropProfile {
    profile: ffi::Profile,
}

impl Drop for DropProfile {
    fn drop(&mut self) {
        // SAFETY: the profile comes from `ddog_prof_Profile_new`, or it was already dropped.
        unsafe { ffi::ddog_prof_Profile_drop(&mut self.profile) };
    }
}

fn run_drop_profile(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<DropProfile>().unwrap();
    // SAFETY: the profile comes from `ddog_prof_Profile_new`.
    unsafe { ffi::ddog_prof_Profile_drop(&mut fx.profile) };
    Ok(())
}

/// Drop nulls the handle, and a second drop is a no-op.
///
/// Contract: the header accepts a null `profile` pointer and forbids dropping a profile twice.
/// Nulling the handle is undocumented; the second drop on the nulled handle is out of contract,
/// an implementation observation that must not become a gate.
fn check_drop_profile(fixture: &mut Box<dyn Any>, _failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<DropProfile>().unwrap();
    if !fx.profile.inner.is_null() {
        return Err("Profile_drop left the profile handle set".to_string());
    }
    // SAFETY: out of contract; the `ddog_prof_Profile_drop` implementation ignores a null
    // handle.
    unsafe { ffi::ddog_prof_Profile_drop(&mut fx.profile) };
    if fx.profile.inner.is_null() {
        Ok(())
    } else {
        Err("the second Profile_drop restored a handle".to_string())
    }
}

struct DropEncoded {
    encoded: Handle<EncodedProfile>,
}

impl Drop for DropEncoded {
    fn drop(&mut self) {
        // SAFETY: the handle comes from `ddog_prof_Profile_serialize`, or it is empty.
        unsafe { ffi::ddog_prof_EncodedProfile_drop(&mut self.encoded) };
    }
}

fn run_drop_encoded(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<DropEncoded>().unwrap();
    // SAFETY: the handle comes from `ddog_prof_Profile_serialize`.
    unsafe { ffi::ddog_prof_EncodedProfile_drop(&mut fx.encoded) };
    Ok(())
}

/// Drop empties the handle, and a second drop is a no-op.
///
/// Contract: the header says "do not call this twice on the same object".
/// Nulling the handle is undocumented ownership; the second drop is out of contract, an
/// implementation observation that must not become a gate.
fn check_drop_encoded(fixture: &mut Box<dyn Any>, _failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<DropEncoded>().unwrap();
    if encoded_owned(&mut fx.encoded) {
        return Err("EncodedProfile_drop left the handle set".to_string());
    }
    // SAFETY: out of contract; the `ddog_prof_EncodedProfile_drop` implementation ignores an empty
    // handle.
    unsafe { ffi::ddog_prof_EncodedProfile_drop(&mut fx.encoded) };
    if encoded_owned(&mut fx.encoded) {
        Err("the second EncodedProfile_drop restored a handle".to_string())
    } else {
        Ok(())
    }
}

fn exporter_owned(exporter: &mut Handle<ProfileExporter>) -> bool {
    let mut handle: *mut Handle<ProfileExporter> = exporter;
    // SAFETY: the handle is empty or holds the exporter from `ddog_prof_Exporter_new`.
    unsafe { handle.to_inner_mut() }.is_ok()
}

fn run_drop_exporter(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<ExporterFixture>().unwrap();
    // SAFETY: the exporter comes from `ddog_prof_Exporter_new`.
    unsafe { ffi::ddog_prof_Exporter_drop(&mut fx.exporter) };
    Ok(())
}

/// Drop empties the handle, and a second drop is a no-op.
///
/// Contract: the header forbids an exporter "that has already been dropped".
/// Nulling the handle is undocumented ownership; the second drop is out of contract, an
/// implementation observation that must not become a gate.
fn check_drop_exporter(fixture: &mut Box<dyn Any>, _failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<ExporterFixture>().unwrap();
    if exporter_owned(&mut fx.exporter) {
        return Err("Exporter_drop left the handle set".to_string());
    }
    // SAFETY: out of contract; the `ddog_prof_Exporter_drop` implementation ignores an empty
    // handle.
    unsafe { ffi::ddog_prof_Exporter_drop(&mut fx.exporter) };
    if exporter_owned(&mut fx.exporter) {
        Err("the second Exporter_drop restored a handle".to_string())
    } else {
        Ok(())
    }
}

struct DropError {
    error: libdd_common_ffi::Error,
}

fn profile_error() -> libdd_common_ffi::Error {
    let bad = unsafe { Slice::<ffi::SampleType>::from_raw_parts(std::ptr::null(), 1) };
    // SAFETY: a null pointer with a non-zero length is the documented invalid slice.
    match unsafe { ffi::ddog_prof_Profile_new(bad, None) } {
        ffi::ProfileNewResult::Err(error) => error,
        ffi::ProfileNewResult::Ok(mut profile) => {
            // SAFETY: the profile comes from `ddog_prof_Profile_new`.
            unsafe { ffi::ddog_prof_Profile_drop(&mut profile) };
            panic!("Profile_new accepted an invalid slice");
        }
    }
}

fn run_drop_error(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<DropError>().unwrap();
    // SAFETY: `error` is a valid error returned by the FFI.
    unsafe { libdd_common_ffi::ddog_Error_drop(Some(&mut fx.error)) };
    Ok(())
}

/// Drop clears the message, and a second drop stays cleared.
///
/// Contract: "It should not be used after this". The second drop is out of contract (the header
/// only says the implementation "tries to limit the damage").
fn check_drop_error(fixture: &mut Box<dyn Any>, _failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<DropError>().unwrap();
    if !fx.error.as_ref().is_empty() {
        return Err("Error_drop left a message".to_string());
    }
    // SAFETY: the error is still a valid (cleared) error.
    unsafe { libdd_common_ffi::ddog_Error_drop(Some(&mut fx.error)) };
    if fx.error.as_ref().is_empty() {
        Ok(())
    } else {
        Err("the second Error_drop restored a message".to_string())
    }
}

fn populated_tags() -> libdd_common_ffi::Vec<Tag> {
    let mut tags = libdd_common_ffi::tags::ddog_Vec_Tag_new();
    // SAFETY: `tags` comes from `ddog_Vec_Tag_new`; both slices are live.
    let result = unsafe {
        libdd_common_ffi::tags::ddog_Vec_Tag_push(
            &mut tags,
            CharSlice::from("env"),
            CharSlice::from("prod"),
        )
    };
    assert!(
        matches!(result, libdd_common_ffi::tags::PushTagResult::Ok),
        "setup tag push"
    );
    tags
}

struct DropTags {
    tags: Option<libdd_common_ffi::Vec<Tag>>,
}

fn run_drop_tags(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<DropTags>().unwrap();
    libdd_common_ffi::tags::ddog_Vec_Tag_drop(fx.tags.take().expect("tags"));
    Ok(())
}

/// Drop consumes the vector.
///
/// Contract: by-value ownership is in the signature; `ddog_Vec_Tag_drop` has no doc comment.
fn check_drop_tags(fixture: &mut Box<dyn Any>, _failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<DropTags>().unwrap();
    if fx.tags.is_some() {
        Err("Vec_Tag_drop did not consume the vector".to_string())
    } else {
        Ok(())
    }
}

struct DropCancel {
    token: Handle<ffi::CancellationToken>,
}

impl Drop for DropCancel {
    fn drop(&mut self) {
        // SAFETY: the handle comes from `ddog_CancellationToken_new`, or it is empty.
        unsafe { ffi::ddog_CancellationToken_drop(&mut self.token) };
    }
}

fn cancel_owned(token: &mut Handle<ffi::CancellationToken>) -> bool {
    let mut handle: *mut Handle<ffi::CancellationToken> = token;
    // SAFETY: the handle is empty or holds the token from `ddog_CancellationToken_new`.
    unsafe { handle.to_inner_mut() }.is_ok()
}

fn run_drop_cancel(fixture: &mut Box<dyn Any>) -> Result<(), ScenarioError> {
    let fx = fixture.downcast_mut::<DropCancel>().unwrap();
    // SAFETY: the token comes from `ddog_CancellationToken_new`.
    unsafe { ffi::ddog_CancellationToken_drop(&mut fx.token) };
    Ok(())
}

/// Drop empties the handle, and a second drop is a no-op.
///
/// Contract: the header forbids a token that has "been dropped already".
/// Nulling the handle is undocumented ownership; the second drop is out of contract, an
/// implementation observation that must not become a gate.
fn check_drop_cancel(fixture: &mut Box<dyn Any>, _failed: bool) -> Result<(), String> {
    let fx = fixture.downcast_mut::<DropCancel>().unwrap();
    if cancel_owned(&mut fx.token) {
        return Err("CancellationToken_drop left the handle set".to_string());
    }
    // SAFETY: out of contract; the `ddog_CancellationToken_drop` implementation ignores an empty
    // handle.
    unsafe { ffi::ddog_CancellationToken_drop(&mut fx.token) };
    if cancel_owned(&mut fx.token) {
        Err("the second CancellationToken_drop restored a handle".to_string())
    } else {
        Ok(())
    }
}
