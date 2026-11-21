// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A profile built against real intern tables, lowered to pprof and OTLP,
//! and decoded again with `prost` structs written from the `.proto` files.

#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::struct_field_names,
    reason = "test fixtures mirror the protos field for field"
)]

extern crate std;

use crate::encode::otlp::{OtlpSampleType, ProfileSpec, Scope};
use crate::encode::{EncodeError, EncodeOptions, Period, Resource, otlp, pprof};
use crate::intern_table::{Config, FunctionTable, StringId, StringTable, semconv};
use crate::profile::{
    Frame, Function, Label, LabelValue, Link, Profile, ProfileStacks, Sample, SampleType,
    SampleTypeId, SampleValue,
};
use crate::request;
use prost::Message;
use rust_alloc::ffi::CString;
use rust_alloc::string::{String, ToString};
use rust_alloc::vec;
use rust_alloc::vec::Vec;

// pprof, from perftools.profiles.

#[derive(Clone, PartialEq, Message)]
struct PProfile {
    #[prost(message, repeated, tag = "1")]
    sample_type: Vec<PValueType>,
    #[prost(message, repeated, tag = "2")]
    sample: Vec<PSample>,
    #[prost(message, repeated, tag = "4")]
    location: Vec<PLocation>,
    #[prost(message, repeated, tag = "5")]
    function: Vec<PFunction>,
    #[prost(string, repeated, tag = "6")]
    string_table: Vec<String>,
    #[prost(int64, tag = "9")]
    time_nanos: i64,
    #[prost(int64, tag = "10")]
    duration_nanos: i64,
    #[prost(message, optional, tag = "11")]
    period_type: Option<PValueType>,
    #[prost(int64, tag = "12")]
    period: i64,
}

#[derive(Clone, PartialEq, Message)]
struct PValueType {
    #[prost(int64, tag = "1")]
    r#type: i64,
    #[prost(int64, tag = "2")]
    unit: i64,
}

#[derive(Clone, PartialEq, Message)]
struct PSample {
    #[prost(uint64, repeated, tag = "1")]
    location_id: Vec<u64>,
    #[prost(int64, repeated, tag = "2")]
    value: Vec<i64>,
    #[prost(message, repeated, tag = "3")]
    label: Vec<PLabel>,
}

#[derive(Clone, PartialEq, Message)]
struct PLabel {
    #[prost(int64, tag = "1")]
    key: i64,
    #[prost(int64, tag = "2")]
    str: i64,
    #[prost(int64, tag = "3")]
    num: i64,
}

#[derive(Clone, PartialEq, Message)]
struct PLocation {
    #[prost(uint64, tag = "1")]
    id: u64,
    #[prost(uint64, tag = "3")]
    address: u64,
    #[prost(message, repeated, tag = "4")]
    line: Vec<PLine>,
}

#[derive(Clone, PartialEq, Message)]
struct PLine {
    #[prost(uint64, tag = "1")]
    function_id: u64,
    #[prost(int64, tag = "2")]
    line: i64,
}

#[derive(Clone, PartialEq, Message)]
struct PFunction {
    #[prost(uint64, tag = "1")]
    id: u64,
    #[prost(int64, tag = "2")]
    name: i64,
    #[prost(int64, tag = "4")]
    filename: i64,
    #[prost(int64, tag = "5")]
    start_line: i64,
}

// OTLP, from opentelemetry.proto.collector.profiles.v1development.

#[derive(Clone, PartialEq, Message)]
struct ORequest {
    #[prost(message, repeated, tag = "1")]
    resource_profiles: Vec<OResourceProfiles>,
    #[prost(message, optional, tag = "2")]
    dictionary: Option<ODictionary>,
}

#[derive(Clone, PartialEq, Message)]
struct OResourceProfiles {
    #[prost(message, optional, tag = "1")]
    resource: Option<OResource>,
    #[prost(message, repeated, tag = "2")]
    scope_profiles: Vec<OScopeProfiles>,
}

#[derive(Clone, PartialEq, Message)]
struct OResource {
    #[prost(message, repeated, tag = "1")]
    attributes: Vec<OKeyValue>,
}

#[derive(Clone, PartialEq, Message)]
struct OKeyValue {
    #[prost(string, tag = "1")]
    key: String,
    #[prost(message, optional, tag = "2")]
    value: Option<OAnyValue>,
}

#[derive(Clone, PartialEq, Message)]
struct OAnyValue {
    #[prost(string, optional, tag = "1")]
    string_value: Option<String>,
    #[prost(int64, optional, tag = "3")]
    int_value: Option<i64>,
}

#[derive(Clone, PartialEq, Message)]
struct OScopeProfiles {
    #[prost(message, optional, tag = "1")]
    scope: Option<OScope>,
    #[prost(message, repeated, tag = "2")]
    profiles: Vec<OProfile>,
}

#[derive(Clone, PartialEq, Message)]
struct OScope {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(string, tag = "2")]
    version: String,
    #[prost(message, repeated, tag = "3")]
    attributes: Vec<OKeyValue>,
}

#[derive(Clone, PartialEq, Message)]
struct OProfile {
    #[prost(message, optional, tag = "1")]
    sample_type: Option<OValueType>,
    #[prost(message, repeated, tag = "2")]
    samples: Vec<OSample>,
    #[prost(fixed64, tag = "3")]
    time_unix_nano: u64,
    #[prost(uint64, tag = "4")]
    duration_nano: u64,
    #[prost(int64, tag = "6")]
    period: i64,
}

#[derive(Clone, PartialEq, Message)]
struct OValueType {
    #[prost(int32, tag = "1")]
    type_strindex: i32,
    #[prost(int32, tag = "2")]
    unit_strindex: i32,
}

#[derive(Clone, PartialEq, Message)]
struct OSample {
    #[prost(int32, tag = "1")]
    stack_index: i32,
    #[prost(int32, repeated, tag = "2")]
    attribute_indices: Vec<i32>,
    #[prost(int32, tag = "3")]
    link_index: i32,
    #[prost(int64, repeated, tag = "4")]
    values: Vec<i64>,
    #[prost(fixed64, repeated, tag = "5")]
    timestamps_unix_nano: Vec<u64>,
}

#[derive(Clone, PartialEq, Message)]
struct ODictionary {
    #[prost(message, repeated, tag = "1")]
    mapping_table: Vec<OEmpty>,
    #[prost(message, repeated, tag = "2")]
    location_table: Vec<OLocation>,
    #[prost(message, repeated, tag = "3")]
    function_table: Vec<OFunction>,
    #[prost(message, repeated, tag = "4")]
    link_table: Vec<OLink>,
    #[prost(string, repeated, tag = "5")]
    string_table: Vec<String>,
    #[prost(message, repeated, tag = "6")]
    attribute_table: Vec<OKeyValueAndUnit>,
    #[prost(message, repeated, tag = "7")]
    stack_table: Vec<OStack>,
}

#[derive(Clone, PartialEq, Message)]
struct OEmpty {}

#[derive(Clone, PartialEq, Message)]
struct OLocation {
    #[prost(int32, tag = "1")]
    mapping_index: i32,
    #[prost(uint64, tag = "2")]
    address: u64,
    #[prost(message, repeated, tag = "3")]
    lines: Vec<OLine>,
}

#[derive(Clone, PartialEq, Message)]
struct OLine {
    #[prost(int32, tag = "1")]
    function_index: i32,
    #[prost(int64, tag = "2")]
    line: i64,
}

#[derive(Clone, PartialEq, Message)]
struct OFunction {
    #[prost(int32, tag = "1")]
    name_strindex: i32,
    #[prost(int32, tag = "3")]
    filename_strindex: i32,
    #[prost(int64, tag = "4")]
    start_line: i64,
}

#[derive(Clone, PartialEq, Message)]
struct OLink {
    #[prost(bytes = "vec", tag = "1")]
    trace_id: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    span_id: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
struct OKeyValueAndUnit {
    #[prost(int32, tag = "1")]
    key_strindex: i32,
    #[prost(message, optional, tag = "2")]
    value: Option<OAnyValue>,
}

#[derive(Clone, PartialEq, Message)]
struct OStack {
    #[prost(int32, repeated, tag = "1")]
    location_indices: Vec<i32>,
}

fn lock_dir() -> Option<CString> {
    cfg!(unix).then(|| {
        let dir = std::env::temp_dir();
        CString::new(dir.into_os_string().as_encoded_bytes()).unwrap()
    })
}

fn tables() -> (StringTable, FunctionTable) {
    let dir = lock_dir();
    let strings = StringTable::new(
        Config {
            regions: 1,
            arena_bytes: 1 << 16,
        },
        dir.as_deref(),
    )
    .unwrap();
    let functions = FunctionTable::new(
        Config {
            regions: 1,
            arena_bytes: 0,
        },
        dir.as_deref(),
    )
    .unwrap();
    (strings, functions)
}

const START: i64 = 1_700_000_000_000_000_000;
const END: i64 = START + 60_000_000_000;
const TIMESTAMP: i64 = START + 1_000;
const TRACE_ID: [u8; 16] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
const SPAN_ID: [u8; 8] = [0, 0, 0, 0, 0, 0, 0x12, 0x34];

type TestProfile<'t> = Profile<&'t StringTable, &'t FunctionTable, ProfileStacks>;

/// A value for the sample type at `index`.
const fn v(index: u32, value: i64) -> SampleValue {
    SampleValue {
        sample_type: SampleTypeId::from_index(index),
        value,
    }
}

const PERIOD: Option<Period<'static>> = Some(Period {
    name: "cpu-time",
    unit: "nanoseconds",
    value: 10_000_000,
});

/// The fixture's OTLP profiles: cpu-time, and alloc-space as alloc-size.
const SPECS: [ProfileSpec<'static>; 2] = [
    ProfileSpec {
        source: SampleTypeId::from_index(0),
        sample_type: OtlpSampleType::CpuTime,
        period: PERIOD,
    },
    ProfileSpec {
        source: SampleTypeId::from_index(3),
        sample_type: OtlpSampleType::AllocSize,
        period: None,
    },
];

/// cpu-time, cpu-samples (CPU), wall-time (wall), alloc-space (memory), with
/// an aggregated sample in a span, a memory sample, and a timed sample.
fn build<'t>(strings: &'t StringTable, functions: &'t FunctionTable) -> TestProfile<'t> {
    let s = |text| strings.intern(text).unwrap();
    let sample_type = |name, unit| SampleType {
        name: s(name),
        unit: s(unit),
    };
    let types = [
        sample_type("cpu-time", "nanoseconds"),
        sample_type("cpu-samples", "count"),
        sample_type("wall-time", "nanoseconds"),
        sample_type("alloc-space", "bytes"),
    ];
    let main = functions
        .intern(Function {
            name: s("main"),
            system_name: s("main"),
            filename: s("app.py"),
            start_line: 1,
        })
        .unwrap();
    let foo = functions
        .intern(Function {
            name: s("foo"),
            system_name: s("foo"),
            filename: s("app.py"),
            start_line: 2,
        })
        .unwrap();
    let frame = |function, line| Frame {
        address: 0,
        line,
        function,
    };
    let mut profile =
        Profile::try_new(strings, functions, ProfileStacks::new(), &types, START).unwrap();
    let task = profile.intern_local("task-1234").unwrap();
    let labels = [
        Label {
            key: s("thread name"),
            value: LabelValue::Str(s("MainThread")),
        },
        Label {
            key: s("task name"),
            value: LabelValue::Local(task),
        },
        // Keyed by the reserved semantic-convention id, not by text.
        Label {
            key: semconv::THREAD_ID,
            value: LabelValue::Num {
                value: 7,
                unit: StringId::ZERO,
            },
        },
    ];
    let in_span = Sample {
        stack: &[frame(foo, 3), frame(main, 10)][..],
        values: &[v(0, 10), v(1, 1), v(2, 20)],
        labels: &labels,
        link: Link {
            trace_id: TRACE_ID,
            span_id: SPAN_ID,
        },
        local_root_span_id: 42,
        timestamp_ns: 0,
    };
    profile.add_sample(in_span).unwrap();
    profile.add_sample(in_span).unwrap();
    profile
        .add_sample(Sample {
            stack: &[frame(main, 10)][..],
            values: &[v(3, 512)],
            labels: &[],
            link: Link::default(),
            local_root_span_id: 0,
            timestamp_ns: 0,
        })
        .unwrap();
    profile
        .add_sample(Sample {
            stack: &[frame(foo, 3)][..],
            values: &[v(0, 5), v(1, 1), v(2, 5)],
            labels: &[],
            link: Link::default(),
            local_root_span_id: 0,
            timestamp_ns: TIMESTAMP,
        })
        .unwrap();
    profile.set_endpoint(42, "GET /users").unwrap();
    profile.add_endpoint_count("GET /users", 3).unwrap();
    profile
}

const RESOURCE: Resource<'static> = Resource {
    service: "svc",
    env: "prod",
    version: "1.2.3",
    host_name: "host-1",
    runtime_id: "rid",
    language: "python",
    runtime_name: "CPython",
    runtime_version: "3.13.1",
    runtime_description: "3.13.1 (main, Dec  6 2024) [Clang 18.1.8]",
    profiler_version: "4.0.0",
    container_id: "",
    cgroup_inode: 8154,
    process_id: 77,
    tags: &[
        ("team", "profiling"),
        // Datadog names with OTel equivalents: never sent under these names.
        ("env", "staging"), // the env field wins
        ("runtime-id", "other"),
        ("process_id", "78"),
        ("git.commit.sha", "abc123"),
        ("git.repository_url", "https://github.com/org/repo"),
        ("kube_namespace", "default"),
        ("pod_name", "web-1"),
    ],
};

const SCOPE: Scope<'static> = Scope {
    name: "dd-trace-py",
    version: "4.0.0",
    attributes: &[("datadog.process_tags", "entrypoint.name:app")],
};

#[test]
fn pprof_has_every_sample_type_and_span_labels() {
    let (strings, functions) = tables();
    let profile = build(&strings, &functions);
    let bytes = pprof::encode(&profile, END, PERIOD, EncodeOptions::default()).unwrap();
    let decoded = PProfile::decode(bytes.as_slice()).unwrap();
    let st = &decoded.string_table;
    let s = |i: i64| st[usize::try_from(i).unwrap()].as_str();

    assert_eq!(s(0), "");
    let types: Vec<_> = decoded.sample_type.iter().map(|t| s(t.r#type)).collect();
    assert_eq!(
        types,
        ["cpu-time", "cpu-samples", "wall-time", "alloc-space"]
    );
    assert_eq!(decoded.time_nanos, START);
    assert_eq!(decoded.duration_nanos, END - START);
    assert_eq!(s(decoded.period_type.as_ref().unwrap().r#type), "cpu-time");
    assert_eq!(decoded.period, 10_000_000);
    assert_eq!(decoded.sample.len(), 3);

    let function_name = |location_id: u64| {
        let location = &decoded.location[usize::try_from(location_id - 1).unwrap()];
        assert_eq!(location.id, location_id);
        let function =
            &decoded.function[usize::try_from(location.line[0].function_id - 1).unwrap()];
        s(function.name).to_string()
    };
    let labels = |sample: &PSample| -> Vec<(String, String, i64)> {
        sample
            .label
            .iter()
            .map(|l| (s(l.key).to_string(), s(l.str).to_string(), l.num))
            .collect()
    };

    let in_span = &decoded.sample[0];
    assert_eq!(in_span.value, [20, 2, 40, 0], "aggregated twice");
    let names: Vec<_> = in_span
        .location_id
        .iter()
        .map(|&id| function_name(id))
        .collect();
    assert_eq!(names, ["foo", "main"]);
    let in_span_labels = labels(in_span);
    assert!(in_span_labels.contains(&("thread name".into(), "MainThread".into(), 0)));
    assert!(in_span_labels.contains(&("task name".into(), "task-1234".into(), 0)));
    assert!(
        strings.intern("task-1234").unwrap().index() > 0,
        "only the profile held it"
    );
    assert!(in_span_labels.contains(&("span id".into(), String::new(), 0x1234)));
    assert!(in_span_labels.contains(&("local root span id".into(), String::new(), 42)));
    assert!(in_span_labels.contains(&("trace endpoint".into(), "GET /users".into(), 0)));
    // pprof has no trace id.
    assert!(!st.iter().any(|s| s.contains("trace id")));

    assert_eq!(decoded.sample[1].value, [0, 0, 0, 512]);
    assert_eq!(labels(&decoded.sample[1]), []);
    let timed = &decoded.sample[2];
    assert_eq!(
        labels(timed),
        [("end_timestamp_ns".into(), String::new(), TIMESTAMP)]
    );
}

#[test]
fn otlp_has_only_cpu_and_memory_and_links_traces() {
    let (strings, functions) = tables();
    let profile = build(&strings, &functions);
    let bytes = otlp::encode(
        &profile,
        END,
        &RESOURCE,
        &SCOPE,
        &SPECS,
        EncodeOptions::default(),
    )
    .unwrap();
    let decoded = ORequest::decode(bytes.as_slice()).unwrap();
    let dictionary = decoded.dictionary.unwrap();
    let st = &dictionary.string_table;
    let s = |i: i32| st[usize::try_from(i).unwrap()].as_str();

    // Zero entries.
    assert_eq!(s(0), "");
    assert_eq!(dictionary.mapping_table.len(), 1);
    assert_eq!(dictionary.location_table[0], OLocation::default());
    assert_eq!(dictionary.function_table[0], OFunction::default());
    assert_eq!(dictionary.stack_table[0], OStack::default());
    assert_eq!(dictionary.link_table[0], OLink::default());
    assert_eq!(dictionary.attribute_table[0], OKeyValueAndUnit::default());

    let [resource_profiles] = decoded.resource_profiles.as_slice() else {
        panic!("one resource");
    };
    let attributes: Vec<(String, Option<String>, Option<i64>)> = resource_profiles
        .resource
        .as_ref()
        .unwrap()
        .attributes
        .iter()
        .map(|kv| {
            let value = kv.value.clone().unwrap_or_default();
            (kv.key.clone(), value.string_value, value.int_value)
        })
        .collect();
    for (key, value) in [
        ("service.name", "svc"),
        ("deployment.environment.name", "prod"),
        ("service.version", "1.2.3"),
        ("host.name", "host-1"),
        ("service.instance.id", "rid"),
        ("telemetry.sdk.language", "python"),
        ("telemetry.sdk.name", "datadog"),
        ("telemetry.sdk.version", "4.0.0"),
        ("process.runtime.name", "CPython"),
        ("process.runtime.version", "3.13.1"),
        (
            "process.runtime.description",
            "3.13.1 (main, Dec  6 2024) [Clang 18.1.8]",
        ),
        ("vcs.ref.head.revision", "abc123"),
        ("vcs.repository.url.full", "https://github.com/org/repo"),
        ("k8s.namespace.name", "default"),
        ("k8s.pod.name", "web-1"),
        ("team", "profiling"),
    ] {
        assert!(
            attributes.contains(&(key.into(), Some(value.into()), None)),
            "{key} in {attributes:?}"
        );
    }
    assert!(attributes.contains(&("process.pid".into(), None, Some(77))));
    // An int, which is the only type DDOT's infraattributes processor reads.
    assert!(attributes.contains(&("datadog.container.cgroup_inode".into(), None, Some(8154))));
    // Each key once, the known fields winning over same-named tags.
    let mut keys: Vec<&str> = attributes.iter().map(|(k, _, _)| k.as_str()).collect();
    keys.sort_unstable();
    let before = keys.len();
    keys.dedup();
    assert_eq!(
        keys.len(),
        before,
        "a resource key was sent twice: {attributes:?}"
    );
    // No Datadog name is sent, on the resource or the scope.
    let scope = resource_profiles.scope_profiles[0].scope.as_ref().unwrap();
    for datadog in [
        "service",
        "env",
        "version",
        "host",
        "runtime-id",
        "language",
        "runtime",
        "runtime_version",
        "profiler_version",
        "container_id",
        "process_id",
        "git.commit.sha",
        "git.repository_url",
        "kube_namespace",
        "pod_name",
    ] {
        assert!(
            !attributes.iter().any(|(k, _, _)| k == datadog)
                && !scope.attributes.iter().any(|kv| kv.key == datadog),
            "{datadog} was sent"
        );
    }
    // The scope has only what the caller gave it.
    let scope_keys: Vec<&str> = scope.attributes.iter().map(|kv| kv.key.as_str()).collect();
    let expected: Vec<&str> = SCOPE.attributes.iter().map(|&(k, _)| k).collect();
    assert_eq!(scope_keys, expected);
    assert!(
        !attributes.iter().any(|(k, _, _)| k == "container.id"),
        "empty is omitted"
    );

    let [scope_profiles] = resource_profiles.scope_profiles.as_slice() else {
        panic!("one scope");
    };
    assert_eq!(scope_profiles.scope.as_ref().unwrap().name, "dd-trace-py");
    let types: Vec<_> = scope_profiles
        .profiles
        .iter()
        .map(|p| s(p.sample_type.as_ref().unwrap().type_strindex))
        .collect();
    assert_eq!(
        types,
        ["cpu-time", "alloc-size"],
        "only the requested profiles, under their sanctioned names"
    );

    let cpu = &scope_profiles.profiles[0];
    assert_eq!(cpu.time_unix_nano, START.cast_unsigned());
    assert_eq!(cpu.duration_nano, (END - START).cast_unsigned());
    assert_eq!(cpu.period, 10_000_000);
    assert_eq!(cpu.samples.len(), 2, "the memory-only sample is not CPU");
    let in_span = &cpu.samples[0];
    assert_eq!(in_span.values, [20]);
    let link = &dictionary.link_table[usize::try_from(in_span.link_index).unwrap()];
    assert_eq!(link.trace_id, TRACE_ID);
    assert_eq!(link.span_id, SPAN_ID);
    let attrs: Vec<(String, String, Option<i64>)> = in_span
        .attribute_indices
        .iter()
        .map(|&i| {
            let a = &dictionary.attribute_table[usize::try_from(i).unwrap()];
            let v = a.value.clone().unwrap_or_default();
            (
                s(a.key_strindex).to_string(),
                v.string_value.unwrap_or_default(),
                v.int_value,
            )
        })
        .collect();
    // Only labels with semantic-convention names are kept.
    assert_eq!(
        attrs,
        [
            ("thread.name".to_string(), "MainThread".to_string(), None),
            ("thread.id".to_string(), String::new(), Some(7)),
        ]
    );
    for dropped in [
        "local root span",
        "span id",
        "task",
        "trace endpoint",
        "GET /users",
        "datadog.",
    ] {
        assert!(
            !st.iter().any(|s| s.contains(dropped)),
            "{dropped} not dropped"
        );
    }

    let stack = &dictionary.stack_table[usize::try_from(in_span.stack_index).unwrap()];
    let names: Vec<_> = stack
        .location_indices
        .iter()
        .map(|&l| {
            let location = &dictionary.location_table[usize::try_from(l).unwrap()];
            let line = &location.lines[0];
            let f = &dictionary.function_table[usize::try_from(line.function_index).unwrap()];
            (s(f.name_strindex).to_string(), line.line)
        })
        .collect();
    assert_eq!(names, [("foo".into(), 3), ("main".into(), 10)]);

    let timed = &cpu.samples[1];
    assert_eq!(timed.values, [5]);
    assert_eq!(timed.timestamps_unix_nano, [TIMESTAMP.cast_unsigned()]);
    assert_eq!(timed.link_index, 0);

    let memory = &scope_profiles.profiles[1];
    assert_eq!(memory.samples.len(), 1);
    assert_eq!(memory.samples[0].values, [512]);
    assert_eq!(memory.period, 0, "the period belongs to cpu-time");
}

#[test]
fn otlp_writes_only_valid_requested_profiles() {
    let (strings, functions) = tables();
    let profile = build(&strings, &functions);
    let encode = |specs: &[ProfileSpec<'_>]| {
        otlp::encode(
            &profile,
            END,
            &RESOURCE,
            &SCOPE,
            specs,
            EncodeOptions::default(),
        )
    };
    let spec = |index, sample_type| ProfileSpec {
        source: SampleTypeId::from_index(index),
        sample_type,
        period: None,
    };
    assert_eq!(encode(&[]).unwrap_err(), EncodeError::NothingToEncode);
    for (specs, why) in [
        (
            &[spec(9, OtlpSampleType::CpuTime)][..],
            "no such sample type",
        ),
        (
            &[spec(1, OtlpSampleType::CpuTime)][..],
            "cpu-samples is a count",
        ),
        (
            &[
                spec(0, OtlpSampleType::CpuTime),
                spec(0, OtlpSampleType::CpuTime),
            ][..],
            "requested twice",
        ),
    ] {
        assert_eq!(
            encode(specs).unwrap_err(),
            EncodeError::InvalidArgument,
            "{why}"
        );
    }
    // wall-time is in nanoseconds, so it may be sent as cpu-time: the caller's call.
    assert!(encode(&[spec(2, OtlpSampleType::CpuTime)]).is_ok());
    // pprof carries every sample type, whatever OTLP is asked for.
    let decoded = PProfile::decode(
        pprof::encode(&profile, END, None, EncodeOptions::default())
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    assert_eq!(decoded.sample_type.len(), 4);
    assert!(
        decoded.period_type.is_none() && decoded.period == 0,
        "no period given"
    );
}

#[test]
fn the_size_limit_applies() {
    let (strings, functions) = tables();
    let profile = build(&strings, &functions);
    let tiny = EncodeOptions { max_bytes: 64 };
    assert_eq!(
        pprof::encode(&profile, END, PERIOD, tiny).unwrap_err(),
        EncodeError::TooLarge
    );
    assert_eq!(
        otlp::encode(&profile, END, &RESOURCE, &SCOPE, &SPECS, tiny).unwrap_err(),
        EncodeError::TooLarge
    );
}

#[test]
fn reset_and_invalid_samples() {
    fn bad(values: &[SampleValue]) -> Sample<'_, &'static [Frame]> {
        Sample {
            stack: &[],
            values,
            labels: &[],
            link: Link::default(),
            local_root_span_id: 0,
            timestamp_ns: 0,
        }
    }
    let (strings, functions) = tables();
    let mut profile = build(&strings, &functions);
    let before = profile.samples().count();
    let unknown = [v(4, 1)];
    let repeated = [v(0, 1), v(0, 2)];
    assert_eq!(
        profile.add_sample(bad(&unknown)).unwrap_err(),
        crate::profile::ProfileError::InvalidArgument,
        "an unknown sample type"
    );
    // Repeated types are caught only with debug assertions.
    if cfg!(debug_assertions) {
        assert_eq!(
            profile.add_sample(bad(&repeated)).unwrap_err(),
            crate::profile::ProfileError::InvalidArgument,
            "a repeated sample type"
        );
    }
    assert_eq!(profile.samples().count(), before);
    profile.reset(END).unwrap();
    assert_eq!(profile.samples().count(), 0);
    assert_eq!(profile.start_ns(), END);
    let decoded = PProfile::decode(
        pprof::encode(&profile, END + 1, PERIOD, EncodeOptions::default())
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    assert!(decoded.sample.is_empty() && decoded.location.is_empty());
    assert_eq!(decoded.sample_type.len(), 4);
}

/// The value of header `name`.
fn header<'r>(request: &'r request::Request, name: &str) -> &'r str {
    let header = request.headers.iter().find(|h| h.name == name).unwrap();
    core::str::from_utf8(&header.value).unwrap()
}

#[test]
fn datadog_request_is_a_multipart_form_with_event_json() {
    let (strings, functions) = tables();
    let profile = build(&strings, &functions);
    let options = request::DatadogOptions {
        library_name: "dd-trace-py",
        library_version: "4.0.0",
        family: "python",
        internal_json: r#"{"no_signals_workaround_enabled": "false"}"#,
        info_json: r#"{"profiler": {"version": "4.0.0"}}"#,
        process_tags: "entrypoint.name:app",
        period: PERIOD,
        attachments: &[("code-provenance.json", br#"{"v1":[]}"#)],
    };
    let req = request::datadog(
        &profile,
        END,
        &RESOURCE,
        &options,
        EncodeOptions::default(),
        request::Compression::None,
    )
    .unwrap();
    assert_eq!((req.method, req.path), ("POST", "/profiling/v1/input"));
    assert_eq!(header(&req, "DD-EVP-ORIGIN"), "dd-trace-py");
    assert_eq!(header(&req, "DD-EVP-ORIGIN-VERSION"), "4.0.0");
    let content_type = header(&req, "Content-Type");
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();

    // Split the body into parts: (headers, content).
    let body = req.body.as_slice();
    let delimiter = [b"--", boundary.as_bytes()].concat();
    let mut parts = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.windows(delimiter.len()).position(|w| w == delimiter) {
        let after = &rest[at + delimiter.len()..];
        if after.starts_with(b"--") {
            break;
        }
        let next = after
            .windows(delimiter.len())
            .position(|w| w == delimiter)
            .unwrap();
        let part = &after[2..next - 2]; // drop the CRLFs around the part
        let split = part.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        parts.push((
            String::from_utf8(part[..split].to_vec()).unwrap(),
            part[split + 4..].to_vec(),
        ));
        rest = &after[next..];
    }
    assert_eq!(parts.len(), 3);
    assert!(
        parts[0]
            .0
            .contains("name=\"event\"; filename=\"event.json\"")
    );
    assert!(parts[0].0.contains("Content-Type: application/json"));
    assert!(parts[1].0.contains("name=\"code-provenance.json\""));
    assert!(parts[2].0.contains("name=\"profile.pprof\""));
    let pprof = PProfile::decode(parts[2].1.as_slice()).unwrap();
    assert_eq!(pprof.sample.len(), 3);

    let event: serde_json::Value = serde_json::from_slice(&parts[0].1).unwrap();
    assert_eq!(
        event["attachments"],
        serde_json::json!(["code-provenance.json", "profile.pprof"])
    );
    assert_eq!(event["family"], "python");
    assert_eq!(event["version"], "4");
    assert_eq!(event["start"], "2023-11-14T22:13:20.000000000Z");
    assert_eq!(event["end"], "2023-11-14T22:14:20.000000000Z");
    assert_eq!(
        event["endpoint_counts"],
        serde_json::json!({"GET /users": 3})
    );
    assert_eq!(event["process_tags"], "entrypoint.name:app");
    assert_eq!(event["internal"]["no_signals_workaround_enabled"], "false");
    assert_eq!(
        event["internal"]["libdatadog_version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(event["info"]["profiler"]["version"], "4.0.0");
    let tags = event["tags_profiler"].as_str().unwrap();
    for tag in [
        "service:svc",
        "env:prod",
        "version:1.2.3",
        "runtime-id:rid",
        "language:python",
        "process_id:77",
        "team:profiling",
    ] {
        assert!(tags.split(',').any(|t| t == tag), "{tag} in {tags}");
    }
    assert!(
        !tags.contains("Clang"),
        "the runtime description is OTLP only: {tags}"
    );
}

/// Without zstd, Auto falls back to gzip (which `otel` builds in).
#[cfg(not(feature = "zstd"))]
#[test]
fn datadog_auto_compression_is_gzip_without_zstd() {
    let (strings, functions) = tables();
    let profile = build(&strings, &functions);
    let plain = pprof::encode(&profile, END, PERIOD, EncodeOptions::default()).unwrap();
    let options = request::DatadogOptions {
        family: "python",
        period: PERIOD,
        attachments: &[("code-provenance.json", br#"{"v1":[]}"#)],
        ..Default::default()
    };
    let req = request::datadog(
        &profile,
        END,
        &RESOURCE,
        &options,
        EncodeOptions::default(),
        request::Compression::Auto,
    )
    .unwrap();
    let body = req.body.as_slice();
    // Each part's content starts after its headers' blank line: event.json
    // plain, then the attachment and the profile as gzip members.
    let contents: Vec<&[u8]> = body
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == b"\r\n\r\n")
        .map(|(i, _)| &body[i + 4..])
        .collect();
    assert_eq!(contents.len(), 3, "event.json, the attachment, the profile");
    assert!(contents[0].starts_with(br#"{"attachments":["#));
    for part in &contents[1..] {
        assert!(part.starts_with(&[0x1f, 0x8b, 8]), "a gzip member");
    }
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(contents[2]), &mut decoded)
        .unwrap();
    assert_eq!(decoded, plain.as_slice());
}

#[cfg(feature = "zstd")]
#[test]
fn datadog_auto_compression_is_zstd_for_the_profile_and_attachments() {
    let (strings, functions) = tables();
    let profile = build(&strings, &functions);
    let plain = pprof::encode(&profile, END, PERIOD, EncodeOptions::default()).unwrap();
    let options = request::DatadogOptions {
        family: "python",
        period: PERIOD,
        attachments: &[("code-provenance.json", br#"{"v1":[]}"#)],
        ..Default::default()
    };
    let req = request::datadog(
        &profile,
        END,
        &RESOURCE,
        &options,
        EncodeOptions::default(),
        request::Compression::Auto,
    )
    .unwrap();
    let body = req.body.as_slice();
    // Each compressed part starts a zstd frame: the attachment, then the profile.
    let zstd_magic = [0x28, 0xb5, 0x2f, 0xfd];
    let frames: Vec<usize> = body
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == zstd_magic)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        frames.len(),
        2,
        "the attachment and the profile, not event.json"
    );
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(
        &mut zstd::stream::read::Decoder::new(&body[frames[1]..])
            .unwrap()
            .single_frame(),
        &mut decoded,
    )
    .unwrap();
    assert_eq!(decoded, plain.as_slice());
    assert!(
        body.windows(16)
            .any(|w| w == br#"{"attachments":["#.as_slice()),
        "event.json stays plain JSON"
    );
}

#[test]
fn otlp_request_is_protobuf_without_event_json() {
    let (strings, functions) = tables();
    let profile = build(&strings, &functions);
    let req = request::otlp(
        &profile,
        END,
        &RESOURCE,
        &SCOPE,
        &SPECS,
        EncodeOptions::default(),
        request::Compression::None,
    )
    .unwrap();
    assert_eq!((req.method, req.path), ("POST", "/v1development/profiles"));
    assert_eq!(header(&req, "Content-Type"), "application/x-protobuf");
    assert!(req.headers.iter().all(|h| h.name != "Content-Encoding"));
    let decoded = ORequest::decode(req.body.as_slice()).unwrap();
    assert_eq!(decoded.resource_profiles.len(), 1);
    // Unused, but the fixture interns this id too.
    let _ = StringId::ZERO;
}

#[cfg(feature = "zstd")]
#[test]
fn otlp_bodies_can_be_gzip_or_zstd() {
    use std::io::Read;

    let (strings, functions) = tables();
    let profile = build(&strings, &functions);
    let plain = otlp::encode(
        &profile,
        END,
        &RESOURCE,
        &SCOPE,
        &SPECS,
        EncodeOptions::default(),
    )
    .unwrap();
    let request = |compression| {
        request::otlp(
            &profile,
            END,
            &RESOURCE,
            &SCOPE,
            &SPECS,
            EncodeOptions::default(),
            compression,
        )
        .unwrap()
    };

    let gzip = request(request::Compression::Gzip);
    assert_eq!(header(&gzip, "Content-Encoding"), "gzip");
    let mut unzipped = Vec::new();
    flate2::read::GzDecoder::new(gzip.body.as_slice())
        .read_to_end(&mut unzipped)
        .unwrap();
    assert_eq!(unzipped, plain.as_slice());

    let zstd = request(request::Compression::Zstd);
    assert_eq!(header(&zstd, "Content-Encoding"), "zstd");
    let unzstd = zstd::decode_all(zstd.body.as_slice()).unwrap();
    assert_eq!(unzstd, plain.as_slice());
}

#[test]
fn otlp_combines_samples_with_the_same_identity() {
    let (strings, functions) = tables();
    let s = |text| strings.intern(text).unwrap();
    let types = [
        SampleType {
            name: s("cpu-time"),
            unit: s("nanoseconds"),
        },
        SampleType {
            name: s("alloc-space"),
            unit: s("bytes"),
        },
    ];
    let main = functions
        .intern(Function {
            name: s("main"),
            system_name: s("main"),
            filename: s("app.py"),
            start_line: 1,
        })
        .unwrap();
    let stack = [Frame {
        address: 0,
        line: 10,
        function: main,
    }];
    let mut profile =
        Profile::try_new(&strings, &functions, ProfileStacks::new(), &types, START).unwrap();
    let thread = |name| Label {
        key: s("thread name"),
        value: LabelValue::Str(s(name)),
    };
    let task = |name| Label {
        key: s("task name"),
        value: LabelValue::Str(s(name)),
    };
    let mut add = |labels: &[Label], local_root_span_id, timestamp_ns, dense: &[i64]| {
        let values: Vec<SampleValue> = (0..)
            .zip(dense)
            .filter(|&(_, &value)| value != 0)
            .map(|(index, &value)| v(index, value))
            .collect();
        profile
            .add_sample(Sample {
                stack: &stack[..],
                values: &values,
                labels,
                link: Link::default(),
                local_root_span_id,
                timestamp_ns,
            })
            .unwrap();
    };
    // Timed: differing only in a dropped label or the local root span id
    // still combines; a different thread name does not.
    add(&[thread("T1"), task("a")], 1, START + 10, &[5, 0]);
    add(&[thread("T1"), task("b")], 2, START + 20, &[7, 0]);
    add(&[thread("T2")], 0, START + 30, &[9, 0]);
    add(&[thread("T1")], 0, START + 40, &[0, 3]); // not a CPU sample
    add(&[thread("T1"), task("a")], 1, START + 50, &[11, 0]);
    // Untimed samples that combine are summed.
    add(&[thread("T1"), task("a")], 0, 0, &[2, 0]);
    add(&[thread("T1"), task("b")], 0, 0, &[3, 0]);

    let specs = [
        ProfileSpec {
            source: SampleTypeId::from_index(0),
            sample_type: OtlpSampleType::CpuTime,
            period: None,
        },
        ProfileSpec {
            source: SampleTypeId::from_index(1),
            sample_type: OtlpSampleType::AllocSize,
            period: None,
        },
    ];
    let bytes = otlp::encode(
        &profile,
        END,
        &RESOURCE,
        &SCOPE,
        &specs,
        EncodeOptions::default(),
    )
    .unwrap();
    let decoded = ORequest::decode(bytes.as_slice()).unwrap();
    let dictionary = decoded.dictionary.unwrap();
    let st = &dictionary.string_table;
    let thread_of = |sample: &OSample| -> String {
        let [index] = sample.attribute_indices.as_slice() else {
            panic!("only the thread name is kept");
        };
        let attribute = &dictionary.attribute_table[usize::try_from(*index).unwrap()];
        assert_eq!(
            st[usize::try_from(attribute.key_strindex).unwrap()],
            "thread.name"
        );
        attribute.value.clone().unwrap().string_value.unwrap()
    };
    let profiles = &decoded.resource_profiles[0].scope_profiles[0].profiles;
    let shapes = |profile: &OProfile| -> Vec<(String, Vec<i64>, Vec<u64>)> {
        profile
            .samples
            .iter()
            .map(|sample| {
                (
                    thread_of(sample),
                    sample.values.clone(),
                    sample.timestamps_unix_nano.clone(),
                )
            })
            .collect()
    };
    let at = |offset: i64| (START + offset).cast_unsigned();
    assert_eq!(
        shapes(&profiles[0]),
        [
            ("T1".to_string(), vec![5], vec![]),
            (
                "T1".to_string(),
                vec![5, 7, 11],
                vec![at(10), at(20), at(50)]
            ),
            ("T2".to_string(), vec![9], vec![at(30)]),
        ]
    );
    assert_eq!(
        shapes(&profiles[1]),
        [("T1".to_string(), vec![3], vec![at(40)])]
    );

    // pprof keeps every sample: 2 aggregated (their labels differ) and 5 timed.
    let pprof = PProfile::decode(
        pprof::encode(&profile, END, PERIOD, EncodeOptions::default())
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    assert_eq!(pprof.sample.len(), 7);
}
