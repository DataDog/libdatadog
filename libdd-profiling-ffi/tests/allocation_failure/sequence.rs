// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Valid profiling API sequences decoded from bytes: a new profile, then up to [`MAX_OPS`]
//! calls among add, endpoint metadata, upscaling rules, serialize, bytes, send and reset.
//!
//! Shared by the `profile_sequence` fuzz target (allocation failure disabled) and by the
//! harness, which turns the fuzz inputs that reach new owned branches into named scenarios
//! (`promoted.rs`). Every byte string decodes to a valid call sequence: handles are always
//! valid and calls come in an order the API allows. Inputs can still be rejected (a duplicate
//! label, an overlapping upscaling rule): that is a branch to cover, not an error.
//!
//! Decoding allocates; executing only builds the `#[repr(C)]` views and calls the C symbols.
//! Strings are interned once per process, so a long fuzzing run does not grow memory.

use crate::agent::local_agent;
use crate::ffi;
use libdd_common::tag::Tag;
use libdd_common_ffi::{CharSlice, Handle, Slice, Timespec, ToInner};
use std::collections::HashSet;
use std::num::NonZeroI64;
use std::sync::{Mutex, OnceLock};

/// Calls after `Profile_new`.
pub const MAX_OPS: usize = 24;
/// Sends per sequence: each one serializes, compresses and posts a profile.
const MAX_SENDS: usize = 2;

const SAMPLE_TYPES: [(ffi::SampleType, &str); 8] = [
    (ffi::SampleType::WallTime, "wall-time"),
    (ffi::SampleType::WallSamples, "wall-samples"),
    (ffi::SampleType::CpuTime, "cpu-time"),
    (ffi::SampleType::CpuSamples, "cpu-samples"),
    (ffi::SampleType::ExceptionSamples, "exception-samples"),
    (ffi::SampleType::LockCount, "lock-count"),
    (ffi::SampleType::AllocSamples, "alloc-samples"),
    (ffi::SampleType::AllocSize, "alloc-size"),
];

const FRAMES: [&str; 4] = [
    "|lm:System.Private.CoreLib |ns:System.Threading |ct:ExecutionContext |fn:RunInternal",
    "|lm:System.Private.CoreLib |ns:System.Threading |ct:ThreadPoolWorkQueue |fn:Dispatch",
    "|lm:System.Private.CoreLib |ns:System.Threading |ct:Thread |fn:StartCallback",
    "",
];
const MAPPINGS: [&str; 3] = ["MyApp.dll", "System.Private.CoreLib.dll", ""];
const ENDPOINTS: [&str; 3] = ["GET /orders/{id}", "POST /orders", ""];
const LABEL_KEYS: [&str; 4] = [
    "thread name",
    "appdomain name",
    "exception type",
    "lock name",
];
const LABEL_VALUES: [&str; 4] = [".NET ThreadPool Worker", "MyApp", "System.Exception", ""];
/// Upscaling rule label names. An empty name with an empty value is a by-value rule.
const UPSCALING_LABEL_KEYS: [&str; 5] = [
    "thread name",
    "appdomain name",
    "exception type",
    "lock name",
    "",
];

fn interned() -> &'static Mutex<HashSet<&'static str>> {
    static STRINGS: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    STRINGS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Leaks `s` once per process. Every generated string comes from a bounded set.
fn intern(s: String) -> &'static str {
    let mut strings = interned().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&known) = strings.get(s.as_str()) {
        return known;
    }
    let leaked: &'static str = Box::leak(s.into_boxed_str());
    strings.insert(leaked);
    leaked
}

struct Input<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Input<'_> {
    /// Next byte, 0 once the input is exhausted.
    fn byte(&mut self) -> u8 {
        let b = self.data.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    fn done(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[usize::from(self.byte()) % items.len()]
    }
}

pub struct AddCall {
    locations: Vec<ffi::Location<'static>>,
    values: Vec<i64>,
    labels: Vec<ffi::Label<'static>>,
    timestamp: Option<NonZeroI64>,
}

impl AddCall {
    fn as_ffi(&self) -> ffi::Sample<'_> {
        ffi::Sample {
            locations: Slice::from(self.locations.as_slice()),
            values: Slice::from(self.values.as_slice()),
            labels: Slice::from(self.labels.as_slice()),
        }
    }
}

pub enum Op {
    Add(AddCall),
    SetEndpoint {
        span: u64,
        endpoint: &'static str,
    },
    EndpointCount {
        endpoint: &'static str,
        value: i64,
    },
    Proportional {
        offsets: Vec<usize>,
        label: (&'static str, &'static str),
        sampled: u64,
        real: u64,
    },
    Poisson {
        offsets: Vec<usize>,
        label: (&'static str, &'static str),
        sum: usize,
        count: usize,
        distance: u64,
    },
    Serialize {
        start: Option<Timespec>,
        end: Option<Timespec>,
    },
    Bytes,
    Send {
        tags: bool,
        attachment: bool,
    },
    Reset,
}

impl Op {
    fn name(&self) -> &'static str {
        match self {
            Op::Add(_) => "add",
            Op::SetEndpoint { .. } => "set_endpoint",
            Op::EndpointCount { .. } => "add_endpoint_count",
            Op::Proportional { .. } => "upscaling_proportional",
            Op::Poisson { .. } => "upscaling_poisson",
            Op::Serialize { .. } => "serialize",
            Op::Bytes => "bytes",
            Op::Send { .. } => "send",
            Op::Reset => "reset",
        }
    }
}

pub struct Plan {
    sample_types: Vec<ffi::SampleType>,
    type_names: Vec<&'static str>,
    period: Option<ffi::Period>,
    pub ops: Vec<Op>,
    /// Sample added by the harness's post-failure check.
    pub probe: AddCall,
}

fn timespec(input: &mut Input) -> Option<Timespec> {
    let b = input.byte();
    (b & 1 == 1).then(|| Timespec {
        seconds: 1_763_600_000 + i64::from(b >> 1),
        nanoseconds: u32::from(input.byte()) * 1_000_000,
    })
}

fn offsets(input: &mut Input, values: usize) -> Vec<usize> {
    let mask = input.byte();
    let offsets: Vec<usize> = (0..values.min(8))
        .filter(|i| mask & (1 << i) != 0)
        .collect();
    if offsets.is_empty() { vec![0] } else { offsets }
}

fn endpoint(input: &mut Input) -> &'static str {
    let b = input.byte();
    if b < 192 {
        ENDPOINTS[usize::from(b) % ENDPOINTS.len()]
    } else {
        intern(format!("GET /api/v{}/items", b % 32))
    }
}

fn add_call(input: &mut Input, values: usize) -> AddCall {
    let depth = 1 + usize::from(input.byte() % 8);
    let mut locations = Vec::with_capacity(depth);
    for _ in 0..depth {
        let b = input.byte();
        let name = if b < 192 {
            intern(format!(
                "|lm:MyApp |ns:MyApp.Orders |ct:OrderService{} |fn:Step{}",
                b % 16,
                b % 4
            ))
        } else {
            FRAMES[usize::from(b) % FRAMES.len()]
        };
        let filename = input.pick(&MAPPINGS);
        locations.push(ffi::Location {
            mapping: ffi::Mapping {
                filename: CharSlice::from(filename),
                ..Default::default()
            },
            function: ffi::Function {
                name: CharSlice::from(name),
                ..Default::default()
            },
            line: i64::from(input.byte()),
            ..Default::default()
        });
    }
    let values = (0..values)
        .map(|_| i64::from(input.byte() as i8) * 1_000)
        .collect();
    let str_label = |key: &'static str, value: &'static str| ffi::Label {
        key: CharSlice::from(key),
        str: CharSlice::from(value),
        ..Default::default()
    };
    let num_label = |key: &'static str, num: i64, unit: &'static str| ffi::Label {
        key: CharSlice::from(key),
        num,
        num_unit: CharSlice::from(unit),
        ..Default::default()
    };
    let flags = input.byte();
    let mut labels = Vec::new();
    if flags & 1 != 0 {
        let b = input.byte() % 32;
        labels.push(str_label("thread id", intern(format!("<0x{b:x}> [#{b}]"))));
    }
    if flags & 2 != 0 {
        let key = input.pick(&LABEL_KEYS);
        labels.push(str_label(key, input.pick(&LABEL_VALUES)));
    }
    if flags & 4 != 0 {
        let span = i64::from(input.byte() % 16);
        labels.push(num_label("local root span id", span, ""));
    }
    if flags & 8 != 0 {
        labels.push(num_label("span id", i64::from(input.byte()), ""));
    }
    if flags & 16 != 0 {
        labels.push(str_label("trace endpoint", endpoint(input)));
    }
    if flags & 32 != 0 {
        labels.push(num_label(
            "allocation size",
            i64::from(input.byte()),
            "bytes",
        ));
    }
    if flags & 64 != 0 {
        // A string value for a numeric-only key, or a repeated key: rejected inputs.
        if input.byte() & 1 == 0 {
            labels.push(str_label("local root span id", "1"));
        } else if let Some(&first) = labels.first() {
            labels.push(first);
        }
    }
    let timestamp = if flags & 128 != 0 {
        NonZeroI64::new(1_763_600_000_000_000_000 + i64::from(input.byte()))
    } else {
        None
    };
    AddCall {
        locations,
        values,
        labels,
        timestamp,
    }
}

impl Plan {
    pub fn decode(data: &[u8]) -> Self {
        let mut input = Input { data, pos: 0 };
        let mask = match input.byte() {
            0 => 0b1111,
            m => m,
        };
        let selected: Vec<_> = SAMPLE_TYPES
            .iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, t)| *t)
            .collect();
        let sample_types: Vec<_> = selected.iter().map(|(t, _)| *t).collect();
        let type_names = selected.iter().map(|(_, n)| *n).collect();
        let period_byte = input.byte();
        let period = (period_byte & 1 == 1).then(|| ffi::Period {
            sample_type: sample_types[usize::from(period_byte >> 1) % sample_types.len()],
            value: 10_000_000,
        });
        let n = sample_types.len();
        let mut ops = Vec::new();
        let mut sends = 0;
        let mut serialized = false;
        while !input.done() && ops.len() < MAX_OPS {
            let op = match input.byte() % 10 {
                0 | 1 => Op::Add(add_call(&mut input, n)),
                2 => Op::SetEndpoint {
                    span: u64::from(input.byte() % 16),
                    endpoint: endpoint(&mut input),
                },
                3 => Op::EndpointCount {
                    endpoint: endpoint(&mut input),
                    value: i64::from(input.byte() as i8),
                },
                4 => Op::Proportional {
                    offsets: offsets(&mut input, n),
                    label: (input.pick(&UPSCALING_LABEL_KEYS), input.pick(&LABEL_VALUES)),
                    sampled: u64::from(input.byte()),
                    real: u64::from(input.byte()) * 10,
                },
                5 => Op::Poisson {
                    offsets: offsets(&mut input, n),
                    label: (input.pick(&UPSCALING_LABEL_KEYS), input.pick(&LABEL_VALUES)),
                    sum: usize::from(input.byte()) % (n + 1),
                    count: usize::from(input.byte()) % (n + 1),
                    distance: u64::from(input.byte()) * 64,
                },
                6 => Op::Serialize {
                    start: timespec(&mut input),
                    end: timespec(&mut input),
                },
                7 => Op::Bytes,
                8 if sends < MAX_SENDS => {
                    sends += 1;
                    let b = input.byte();
                    Op::Send {
                        tags: b & 1 != 0,
                        attachment: b & 2 != 0,
                    }
                }
                _ => Op::Reset,
            };
            // `profiling.h`: upscaling rules "must be called before serialize and must not be
            // called after". A send serializes too. The rule's bytes are still consumed.
            let rule = matches!(op, Op::Proportional { .. } | Op::Poisson { .. });
            if rule && serialized {
                continue;
            }
            serialized |= matches!(op, Op::Serialize { .. } | Op::Send { .. });
            ops.push(op);
        }
        let probe = add_call(&mut Input { data: &[], pos: 0 }, n);
        Plan {
            sample_types,
            type_names,
            period,
            ops,
            probe,
        }
    }

    /// Error buffer for [`execute`], with room for every call.
    pub fn errors(&self) -> Vec<bool> {
        Vec::with_capacity(self.ops.len() + 1)
    }

    pub fn sends(&self) -> bool {
        self.ops.iter().any(|op| matches!(op, Op::Send { .. }))
    }

    /// One line for the scenario manifest: the profile shape, then each call.
    pub fn describe(&self) -> String {
        let period = if self.period.is_some() {
            ", period"
        } else {
            ""
        };
        let mut s = format!("Profile_new({}{period})", self.type_names.join(" "));
        for op in &self.ops {
            s.push_str(", ");
            s.push_str(op.name());
            if let Op::Proportional { label, .. } | Op::Poisson { label, .. } = op {
                s.push_str(if label.0.is_empty() && label.1.is_empty() {
                    "(by value)"
                } else {
                    "(by label)"
                });
            }
            if let Op::Add(call) = op {
                s.push_str(&format!(
                    "({} frames, {} labels{})",
                    call.locations.len(),
                    call.labels.len(),
                    if call.timestamp.is_some() { ", ts" } else { "" }
                ));
            }
        }
        s
    }
}

/// Handles of one execution. Drops whatever is left.
pub struct Run {
    pub profile: Option<ffi::Profile>,
    encoded: Handle<ffi::EncodedProfile>,
    exporter: Handle<ffi::ProfileExporter>,
    agent: Option<&'static str>,
    tags: libdd_common_ffi::Vec<Tag>,
    additional_tags: libdd_common_ffi::Vec<Tag>,
}

impl Run {
    /// `agent` is the local agent URL, needed only when the plan sends.
    pub fn new(agent: Option<&'static str>) -> Self {
        let tags: Vec<Tag> = [("service", "my-app"), ("env", "prod"), ("version", "1.2.3")]
            .into_iter()
            .map(|(k, v)| Tag::new(k, v).expect("tag"))
            .collect();
        let additional: Vec<Tag> = vec![Tag::new("cycle", "42").expect("tag")];
        Run {
            profile: None,
            encoded: Handle::empty(),
            exporter: Handle::empty(),
            agent,
            tags: tags.into(),
            additional_tags: additional.into(),
        }
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        // SAFETY: every handle comes from this API, and the drop functions accept empty ones.
        unsafe {
            ffi::ddog_prof_EncodedProfile_drop(&mut self.encoded);
            ffi::ddog_prof_Exporter_drop(&mut self.exporter);
            if let Some(profile) = &mut self.profile {
                ffi::ddog_prof_Profile_drop(profile);
            }
        }
    }
}

fn drop_error(mut error: libdd_common_ffi::Error) {
    // SAFETY: `error` is a valid error returned by the FFI.
    unsafe { libdd_common_ffi::ddog_Error_drop(Some(&mut error)) };
}

fn profile_result(result: ffi::ProfileResult) -> bool {
    match result {
        ffi::ProfileResult::Ok(_) => false,
        ffi::ProfileResult::Err(error) => {
            drop_error(error);
            true
        }
    }
}

/// Adds `call` to `profile`. `true` when the call returned an error.
pub fn add(profile: &mut ffi::Profile, call: &AddCall) -> bool {
    // SAFETY: `profile` comes from `ddog_prof_Profile_new`, and `call` borrows live data.
    profile_result(unsafe { ffi::ddog_prof_Profile_add(profile, call.as_ffi(), call.timestamp) })
}

/// Serializes into `run.encoded`, dropping a previous encoded profile.
fn serialize(run: &mut Run, start: Option<&Timespec>, end: Option<&Timespec>) -> bool {
    let Some(profile) = &mut run.profile else {
        return false;
    };
    // SAFETY: `profile` comes from `ddog_prof_Profile_new`; the encoded handle accepts empty.
    unsafe {
        ffi::ddog_prof_EncodedProfile_drop(&mut run.encoded);
        match ffi::ddog_prof_Profile_serialize(profile, start, end) {
            ffi::SerializeResult::Ok(encoded) => {
                run.encoded = encoded;
                false
            }
            ffi::SerializeResult::Err(error) => {
                drop_error(error);
                true
            }
        }
    }
}

fn exporter_ready(run: &mut Run) -> bool {
    let mut handle: *mut Handle<ffi::ProfileExporter> = &mut run.exporter;
    // SAFETY: the handle is empty or holds the exporter from `ddog_prof_Exporter_new`.
    if unsafe { handle.to_inner_mut() }.is_ok() {
        return true;
    }
    let Some(url) = run.agent else {
        return false;
    };
    // SAFETY: every slice and the tag vector are live for the duration of the call.
    match unsafe {
        ffi::ddog_prof_Exporter_new(
            CharSlice::from("dd-trace-dotnet"),
            CharSlice::from("3.30.0"),
            CharSlice::from("dotnet"),
            Some(&run.tags),
            ffi::ProfilingEndpoint::Agent(CharSlice::from(url), 10_000, false),
        )
    } {
        libdd_common_ffi::Result::Ok(exporter) => {
            run.exporter = exporter;
            true
        }
        libdd_common_ffi::Result::Err(error) => {
            drop_error(error);
            false
        }
    }
}

fn send(run: &mut Run, tags: bool, attachment: bool) -> bool {
    let mut handle: *mut Handle<ffi::EncodedProfile> = &mut run.encoded;
    // SAFETY: the handle is empty or holds the profile from `ddog_prof_Profile_serialize`.
    if unsafe { handle.to_inner_mut() }.is_err() && serialize(run, None, None) {
        return true;
    }
    if !exporter_ready(run) {
        return true;
    }
    static ATTACHMENT: &[u8] = br#"{"runtime":"dotnet","events":[{"type":"gc"}]}"#;
    let files = [ffi::File {
        name: CharSlice::from("metrics.json"),
        file: Slice::from(ATTACHMENT),
    }];
    let files: &[ffi::File] = if attachment { &files } else { &[] };
    let process_tags = CharSlice::from("entrypoint.name:MyApp");
    // SAFETY: the handles come from this API, and every borrowed input is live.
    match unsafe {
        ffi::ddog_prof_Exporter_send_blocking(
            &mut run.exporter,
            &mut run.encoded,
            Slice::from(files),
            tags.then_some(&run.additional_tags),
            tags.then_some(&process_tags),
            None,
            None,
            std::ptr::null_mut(),
        )
    } {
        libdd_common_ffi::Result::Ok(status) => !(200..300).contains(&status.0),
        libdd_common_ffi::Result::Err(error) => {
            drop_error(error);
            true
        }
    }
}

/// Runs `Profile_new` then every op. `errors[0]` is `Profile_new`, `errors[i + 1]` is op `i`.
///
/// With `expected`, an error at an index where `expected` has none stops the run: that is
/// `Err(index)`. Without it (fuzzing), errors are ignored and every op runs.
///
/// `errors` comes from [`Plan::errors`]: pushing into it allocates nothing inside a scope.
pub fn execute(
    plan: &Plan,
    run: &mut Run,
    expected: Option<&[bool]>,
    errors: &mut Vec<bool>,
) -> Result<(), usize> {
    errors.clear();
    let unexpected = |index: usize, errored: bool| {
        errored && expected.is_some_and(|e| !e.get(index).copied().unwrap_or(false))
    };
    // SAFETY: the slice and the period are valid for the duration of the call.
    let created = unsafe {
        ffi::ddog_prof_Profile_new(
            Slice::from(plan.sample_types.as_slice()),
            plan.period.as_ref(),
        )
    };
    match created {
        ffi::ProfileNewResult::Ok(profile) => {
            run.profile = Some(profile);
            errors.push(false);
        }
        ffi::ProfileNewResult::Err(error) => {
            drop_error(error);
            errors.push(true);
            return if unexpected(0, true) { Err(0) } else { Ok(()) };
        }
    }
    for (i, op) in plan.ops.iter().enumerate() {
        let profile = run.profile.as_mut().expect("created above");
        // SAFETY (every call below): `profile` comes from `ddog_prof_Profile_new`, and every
        // slice borrows live data owned by `plan`.
        let errored = match op {
            Op::Add(call) => add(profile, call),
            Op::SetEndpoint { span, endpoint } => profile_result(unsafe {
                ffi::ddog_prof_Profile_set_endpoint(profile, *span, CharSlice::from(*endpoint))
            }),
            Op::EndpointCount { endpoint, value } => profile_result(unsafe {
                ffi::ddog_prof_Profile_add_endpoint_count(
                    profile,
                    CharSlice::from(*endpoint),
                    *value,
                )
            }),
            Op::Proportional {
                offsets,
                label,
                sampled,
                real,
            } => profile_result(unsafe {
                ffi::ddog_prof_Profile_add_upscaling_rule_proportional(
                    profile,
                    Slice::from(offsets.as_slice()),
                    CharSlice::from(label.0),
                    CharSlice::from(label.1),
                    *sampled,
                    *real,
                )
            }),
            Op::Poisson {
                offsets,
                label,
                sum,
                count,
                distance,
            } => profile_result(unsafe {
                ffi::ddog_prof_Profile_add_upscaling_rule_poisson(
                    profile,
                    Slice::from(offsets.as_slice()),
                    CharSlice::from(label.0),
                    CharSlice::from(label.1),
                    *sum,
                    *count,
                    *distance,
                )
            }),
            Op::Serialize { start, end } => serialize(run, start.as_ref(), end.as_ref()),
            Op::Bytes => {
                let mut handle: *mut Handle<ffi::EncodedProfile> = &mut run.encoded;
                // SAFETY: the handle is empty or holds a serialized profile.
                if unsafe { handle.to_inner_mut() }.is_ok() {
                    // SAFETY: as above, and the handle is not empty.
                    match unsafe { ffi::ddog_prof_EncodedProfile_bytes(&mut run.encoded) } {
                        libdd_common_ffi::Result::Ok(_) => false,
                        libdd_common_ffi::Result::Err(error) => {
                            drop_error(error);
                            true
                        }
                    }
                } else {
                    false
                }
            }
            Op::Send { tags, attachment } => send(run, *tags, *attachment),
            Op::Reset => profile_result(unsafe { ffi::ddog_prof_Profile_reset(profile) }),
        };
        errors.push(errored);
        if unexpected(i + 1, errored) {
            return Err(i + 1);
        }
    }
    Ok(())
}

/// Fuzzing entry point: one process-wide local agent, errors ignored.
pub fn fuzz_one(data: &[u8]) {
    static AGENT: OnceLock<&'static str> = OnceLock::new();
    let plan = Plan::decode(data);
    let agent = plan.sends().then(|| *AGENT.get_or_init(local_agent));
    let mut run = Run::new(agent);
    let _ = execute(&plan, &mut run, None, &mut plan.errors());
}
