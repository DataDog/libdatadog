// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Tier 1 lifecycle scenarios: the profile and exporter calls a .NET profiler makes on every
//! upload cycle.
//!
//! They go through the C ABI, like a native caller. The FFI modules of this crate are private,
//! so [`ffi`] mirrors their `#[repr(C)]` types and imports the exported symbols. Every input is
//! built in the setup, outside the scope, so the scope only sees the call under test.
//!
//! Each scenario checks a post-failure contract once the scope has ended:
//! - a failed constructor publishes no handle;
//! - a profile is still usable after a failed add or serialize;
//! - `send_blocking` takes the encoded profile even when it fails, and the exporter can send again.
//!
//! The exporter sends to a local agent served by a thread of the worker. The file endpoint is
//! not used: each exporter starts a dump server whose socket has a random name in `/tmp`, which
//! makes the trace nondeterministic and leaves one socket file per worker.

use crate::scenarios::{Scenario, ScenarioError};
use libdd_common::tag::Tag;
use libdd_common_ffi::{CharSlice, Handle, Slice, ToInner};
use std::any::Any;
use std::num::NonZeroI64;

#[allow(improper_ctypes, dead_code)]
mod ffi {
    use libdd_common::tag::Tag;
    use libdd_common_ffi::{CharSlice, Error, Handle, Result, Slice, Timespec, VoidResult};
    pub use libdd_profiling::api::{ManagedStringId, Period, SampleType};
    pub use libdd_profiling::exporter::ProfileExporter;
    pub use libdd_profiling::internal::EncodedProfile;
    use std::num::NonZeroI64;

    #[repr(C)]
    pub struct Profile {
        pub inner: *mut std::ffi::c_void,
    }

    #[repr(C)]
    pub enum ProfileNewResult {
        Ok(Profile),
        Err(Error),
    }

    #[repr(C)]
    pub enum ProfileResult {
        Ok(bool),
        Err(Error),
    }

    #[repr(C)]
    pub enum SerializeResult {
        Ok(Handle<EncodedProfile>),
        Err(Error),
    }

    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    pub struct Mapping<'a> {
        pub memory_start: u64,
        pub memory_limit: u64,
        pub file_offset: u64,
        pub filename: CharSlice<'a>,
        pub filename_id: ManagedStringId,
        pub build_id: CharSlice<'a>,
        pub build_id_id: ManagedStringId,
    }

    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    pub struct Function<'a> {
        pub name: CharSlice<'a>,
        pub name_id: ManagedStringId,
        pub system_name: CharSlice<'a>,
        pub system_name_id: ManagedStringId,
        pub filename: CharSlice<'a>,
        pub filename_id: ManagedStringId,
    }

    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    pub struct Location<'a> {
        pub mapping: Mapping<'a>,
        pub function: Function<'a>,
        pub address: u64,
        pub line: i64,
    }

    #[repr(C)]
    #[derive(Copy, Clone, Default)]
    pub struct Label<'a> {
        pub key: CharSlice<'a>,
        pub key_id: ManagedStringId,
        pub str: CharSlice<'a>,
        pub str_id: ManagedStringId,
        pub num: i64,
        pub num_unit: CharSlice<'a>,
        pub num_unit_id: ManagedStringId,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct Sample<'a> {
        pub locations: Slice<'a, Location<'a>>,
        pub values: Slice<'a, i64>,
        pub labels: Slice<'a, Label<'a>>,
    }

    #[repr(C)]
    #[derive(Copy, Clone)]
    pub enum ProfilingEndpoint<'a> {
        Agent(CharSlice<'a>, u64, bool),
        Agentless(CharSlice<'a>, CharSlice<'a>, u64, bool),
        File(CharSlice<'a>),
    }

    #[repr(C)]
    pub struct File<'a> {
        pub name: CharSlice<'a>,
        pub file: Slice<'a, u8>,
    }

    #[repr(C)]
    pub struct HttpStatus(pub u16);

    unsafe extern "C" {
        pub fn ddog_prof_Profile_new(
            sample_types: Slice<SampleType>,
            period: Option<&Period>,
        ) -> ProfileNewResult;
        pub fn ddog_prof_Profile_drop(profile: *mut Profile);
        pub fn ddog_prof_Profile_add(
            profile: *mut Profile,
            sample: Sample,
            timestamp: Option<NonZeroI64>,
        ) -> ProfileResult;
        pub fn ddog_prof_Profile_set_endpoint(
            profile: *mut Profile,
            local_root_span_id: u64,
            endpoint: CharSlice,
        ) -> ProfileResult;
        pub fn ddog_prof_Profile_add_endpoint_count(
            profile: *mut Profile,
            endpoint: CharSlice,
            value: i64,
        ) -> ProfileResult;
        pub fn ddog_prof_Profile_add_upscaling_rule_proportional(
            profile: *mut Profile,
            offset_values: Slice<usize>,
            label_name: CharSlice,
            label_value: CharSlice,
            total_sampled: u64,
            total_real: u64,
        ) -> ProfileResult;
        pub fn ddog_prof_Profile_serialize(
            profile: *mut Profile,
            start_time: Option<&Timespec>,
            end_time: Option<&Timespec>,
        ) -> SerializeResult;
        pub fn ddog_prof_EncodedProfile_drop(profile: *mut Handle<EncodedProfile>);
        pub fn ddog_prof_Exporter_new(
            profiling_library_name: CharSlice,
            profiling_library_version: CharSlice,
            family: CharSlice,
            tags: Option<&libdd_common_ffi::Vec<Tag>>,
            endpoint: ProfilingEndpoint,
        ) -> Result<Handle<ProfileExporter>>;
        pub fn ddog_prof_Exporter_init_runtime(
            exporter: *mut Handle<ProfileExporter>,
        ) -> VoidResult;
        pub fn ddog_prof_Exporter_send_blocking(
            exporter: *mut Handle<ProfileExporter>,
            profile: *mut Handle<EncodedProfile>,
            files_to_compress_and_export: Slice<File>,
            optional_additional_tags: Option<&libdd_common_ffi::Vec<Tag>>,
            optional_process_tags: Option<&CharSlice>,
            optional_internal_metadata_json: Option<&CharSlice>,
            optional_info_json: Option<&CharSlice>,
            cancel: *mut std::ffi::c_void,
        ) -> Result<HttpStatus>;
        pub fn ddog_prof_Exporter_drop(exporter: *mut Handle<ProfileExporter>);
    }
}

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

/// Reads one HTTP/1.1 request (`Content-Length` or chunked body) and answers 202.
fn serve_request(mut stream: std::net::TcpStream) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let mut request = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let mut body_start = None;
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);
        if body_start.is_none() {
            body_start = request
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4);
        }
        let Some(start) = body_start else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..start]).to_ascii_lowercase();
        let content_length = headers
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok());
        let done = match content_length {
            Some(len) => request.len() - start >= len,
            None if headers.contains("transfer-encoding: chunked") => {
                request.ends_with(b"0\r\n\r\n")
            }
            None => true,
        };
        if done {
            break;
        }
    }
    stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
}

/// A local agent on a loopback port, answering every request on its own thread. Its
/// allocations happen on that thread: the harness reports them, it does not enumerate them.
fn local_agent() -> &'static str {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the local agent");
    let port = listener.local_addr().expect("local agent address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let _ = serve_request(stream);
        }
    });
    leak(format!("http://127.0.0.1:{port}"))
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
        setup: || Box::new(profile_with(0)),
        run: run_add,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_ADD_INTERNED,
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
        setup: || Box::new(profile_with(1)),
        run: run_add,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_ADD_TIMESTAMPED,
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
        setup: || Box::new(profile_with(GROWTH_SAMPLES)),
        run: run_add,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_SERIALIZE_EMPTY,
        setup: || Box::new(profile_with(0)),
        run: run_serialize,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_SERIALIZE_POPULATED,
        setup: || Box::new(profile_with(POPULATED_SAMPLES)),
        run: run_serialize,
        check: Some(check_profile_usable),
    },
    Scenario {
        name: PROFILE_SERIALIZE_ENDPOINTS,
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
        setup: || Box::new(exporter_fixture(agent_endpoint(local_agent()))),
        run: run_new_exporter,
        check: Some(check_new_exporter),
    },
    Scenario {
        name: EXPORTER_SEND_COLD,
        setup: || Box::new(exporter_ready(agent_endpoint(local_agent()), false)),
        run: run_send,
        check: Some(check_send),
    },
    Scenario {
        name: EXPORTER_SEND_WARM,
        setup: || Box::new(exporter_ready(agent_endpoint(local_agent()), true)),
        run: run_send,
        check: Some(check_send),
    },
];
