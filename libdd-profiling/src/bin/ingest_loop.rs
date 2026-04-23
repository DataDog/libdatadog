// Copyright 2026-Present Datadog, Inc.
// SPDX-License-Identifier: Apache-2.0

use libc::{getrusage, rusage, RUSAGE_SELF};
use libdd_common::threading::get_current_thread_id;
use libdd_profiling::api2::Location2;
#[cfg(feature = "dynamic_profile")]
use libdd_profiling::dynamic::{DynamicLabel, DynamicLocation, DynamicProfile, DynamicSample};
use libdd_profiling::profiles::datatypes::{Function, FunctionId2, MappingId2, StringId2};
use libdd_profiling::{self as profiling, api, api2};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};
use rand::seq::SliceRandom;
use rand::{rngs::StdRng, SeedableRng};
use std::env;
use std::num::NonZeroI64;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};

static SHOULD_STOP: AtomicBool = AtomicBool::new(false);
const THREAD_NAME: &str = "this thread";
const RUNTIME_ID: &str = "runtime-1234";
const DEFAULT_WORKLOAD_SEED: u64 = 0x5eed_5eed_d15c_a11a;

#[derive(Clone, Copy)]
struct Frame {
    file_name: &'static str,
    line_number: u32,
    function_name: &'static str,
}

#[derive(Clone)]
struct SampleTemplate {
    frame_range: std::ops::Range<usize>,
    class_name: &'static str,
    category: &'static str,
    weight: usize,
}

impl Frame {
    const fn new(file_name: &'static str, line_number: u32, function_name: &'static str) -> Self {
        Self {
            file_name,
            line_number,
            function_name,
        }
    }
}

#[derive(Clone, Copy)]
struct Frame2 {
    function: FunctionId2,
    line_number: u32,
}

#[cfg(feature = "dynamic_profile")]
#[derive(Clone, Copy)]
struct DynamicFrame {
    function: libdd_profiling::dynamic::DynamicFunctionIndex,
    line_number: u32,
}

fn sample_types() -> Vec<api::SampleType> {
    vec![api::SampleType::CpuSamples]
}

fn profiler_frames() -> Vec<Frame> {
    vec![
        Frame::new(
            "libdd-profiling/src/bin/ingest_loop.rs",
            338,
            "ingest_loop::main",
        ),
        Frame::new(
            "libdd-profiling/src/bin/ingest_loop.rs",
            308,
            "ingest_loop::run_dynamic",
        ),
        Frame::new(
            "libdd-profiling/src/dynamic.rs",
            794,
            "libdd_profiling::dynamic::DynamicProfile::add_sample_by_locations",
        ),
        Frame::new(
            "libdd-profiling/src/dynamic.rs",
            782,
            "libdd_profiling::dynamic::DynamicProfile::add_sample_by_stacktrace",
        ),
        Frame::new(
            "libdd-profiling/src/dynamic.rs",
            982,
            "libdd_profiling::dynamic::DynamicProfile::store_labels",
        ),
        Frame::new(
            "libdd-profiling/src/dynamic.rs",
            457,
            "libdd_profiling::dynamic::DynamicLabelSetTable::intern",
        ),
        Frame::new(
            "libdd-profiling/src/dynamic.rs",
            162,
            "libdd_profiling::dynamic::StoredLabelSlice::hash",
        ),
        Frame::new(
            "libdd-profiling/src/dynamic.rs",
            358,
            "libdd_profiling::dynamic::DynamicLocationSlice::hash",
        ),
        Frame::new(
            "libdd-profiling/src/dynamic.rs",
            349,
            "libdd_profiling::dynamic::DynamicLocationSlice::eq",
        ),
        Frame::new(
            "libdd-profiling/src/dynamic.rs",
            332,
            "libdd_profiling::dynamic::try_allocate_arena_slice",
        ),
        Frame::new(
            "libdd-profiling/src/collections/string_table/mod.rs",
            157,
            "libdd_profiling::collections::string_table::StringTable::try_intern",
        ),
        Frame::new(
            "libdd-profiling/src/internal/profile/mod.rs",
            174,
            "libdd_profiling::internal::profile::Profile::try_add_sample2",
        ),
        Frame::new(
            "libdd-profiling/src/internal/profile/mod.rs",
            307,
            "libdd_profiling::internal::profile::Profile::try_add_sample_internal",
        ),
        Frame::new(
            "libdd-profiling/src/internal/observation/observations.rs",
            53,
            "libdd_profiling::internal::observation::observations::Observations::add",
        ),
        Frame::new(
            "libdd-profiling/src/internal/observation/timestamped_observations.rs",
            49,
            "libdd_profiling::internal::observation::timestamped_observations::TimestampedObservations::add",
        ),
        Frame::new(
            "libdd-profiling/src/internal/observation/observations.rs",
            129,
            "libdd_profiling::internal::observation::observations::AggregatedObservations::add",
        ),
        Frame::new(
            "libdd-profiling/src/internal/profile/profiles_dictionary_translator.rs",
            1,
            "libdd_profiling::internal::profile::profiles_dictionary_translator::ProfilesDictionaryTranslator::translate_string",
        ),
        Frame::new(
            "libdd-profiling/src/internal/profile/mod.rs",
            1045,
            "libdd_profiling::internal::profile::Profile::serialize_into_compressed_pprof",
        ),
        Frame::new(
            "libdd-profiling/src/exporter/profile_exporter.rs",
            313,
            "libdd_profiling::exporter::profile_exporter::ProfileExporter::build",
        ),
        Frame::new(
            "libdd-profiling/src/dynamic.rs",
            1069,
            "libdd_profiling::dynamic::DynamicProfile::materialize_pprof",
        ),
    ]
}

fn profiler_sample_templates() -> [SampleTemplate; 8] {
    [
        SampleTemplate {
            frame_range: 0..6,
            class_name: "dynamic-ingest",
            category: "labels",
            weight: 320,
        },
        SampleTemplate {
            frame_range: 0..10,
            class_name: "dynamic-ingest",
            category: "stacktrace-cache",
            weight: 280,
        },
        SampleTemplate {
            frame_range: 0..11,
            class_name: "string-table",
            category: "intern",
            weight: 260,
        },
        SampleTemplate {
            frame_range: 0..14,
            class_name: "timestamped",
            category: "native-timestamped",
            weight: 180,
        },
        SampleTemplate {
            frame_range: 0..16,
            class_name: "native-ingest",
            category: "aggregated",
            weight: 160,
        },
        SampleTemplate {
            frame_range: 0..17,
            class_name: "dictionary",
            category: "translate",
            weight: 140,
        },
        SampleTemplate {
            frame_range: 0..18,
            class_name: "export",
            category: "compress",
            weight: 90,
        },
        SampleTemplate {
            frame_range: 0..20,
            class_name: "dynamic-export",
            category: "materialize",
            weight: 70,
        },
    ]
}

fn install_signal_handlers() {
    let action = SigAction::new(
        SigHandler::Handler(handle_signal),
        SaFlags::empty(),
        SigSet::empty(),
    );
    for signal in [
        Signal::SIGINT,
        Signal::SIGTERM,
        Signal::SIGUSR1,
        Signal::SIGUSR2,
    ] {
        unsafe {
            signal::sigaction(signal, &action).expect("install signal handler");
        }
    }
}

extern "C" fn handle_signal(_: i32) {
    SHOULD_STOP.store(true, Ordering::SeqCst);
}

fn make_stack_api(frames: &[Frame]) -> (Vec<api::Location<'static>>, Vec<i64>) {
    let mapping = api::Mapping::default();
    let mut locations = Vec::with_capacity(frames.len());
    for frame in frames {
        locations.push(api::Location {
            mapping,
            function: api::Function {
                name: frame.function_name,
                filename: frame.file_name,
                ..Default::default()
            },
            line: frame.line_number as i64,
            ..Default::default()
        });
    }
    (locations, vec![1])
}

fn make_stack_api2(frames: &[Frame2]) -> (Vec<Location2>, Vec<i64>) {
    let mut locations = Vec::with_capacity(frames.len());
    for frame in frames {
        locations.push(Location2 {
            mapping: MappingId2::default(),
            function: frame.function,
            address: 0,
            line: frame.line_number as i64,
        });
    }
    (locations, vec![1])
}

#[cfg(feature = "dynamic_profile")]
fn make_stack_dynamic(frames: &[DynamicFrame]) -> (Vec<DynamicLocation>, Vec<i64>) {
    let mut locations = Vec::with_capacity(frames.len());
    for frame in frames {
        locations.push(DynamicLocation {
            function: frame.function,
            line: frame.line_number,
        });
    }
    (locations, vec![1])
}

fn peak_rss_bytes() -> usize {
    let mut usage = std::mem::MaybeUninit::<rusage>::uninit();
    let rc = unsafe { getrusage(RUSAGE_SELF, usage.as_mut_ptr()) };
    if rc != 0 {
        return 0;
    }
    let usage = unsafe { usage.assume_init() };
    #[cfg(target_os = "macos")]
    {
        usage.ru_maxrss as usize
    }
    #[cfg(not(target_os = "macos"))]
    {
        (usage.ru_maxrss as usize) * 1024
    }
}

fn should_continue(processed: usize, target_samples: Option<usize>) -> bool {
    !SHOULD_STOP.load(Ordering::Relaxed) && target_samples.is_none_or(|target| processed < target)
}

fn batch_size(processed: usize, target_samples: Option<usize>) -> usize {
    target_samples.map_or(1000, |target| (target - processed).min(1000))
}

fn next_timestamp(processed: usize, use_timestamps: bool) -> Option<NonZeroI64> {
    use_timestamps.then(|| NonZeroI64::new((processed as i64) + 1).expect("timestamp"))
}

fn next_timestamp_ns(processed: usize, use_timestamps: bool) -> i64 {
    if use_timestamps {
        (processed as i64) + 1
    } else {
        0
    }
}

fn build_profiler_workload(seed: u64) -> (Vec<Frame>, Vec<SampleTemplate>) {
    let frames = profiler_frames();
    let templates = profiler_sample_templates();
    let mut workload = Vec::new();
    workload.reserve(templates.iter().map(|template| template.weight).sum());
    for template in templates {
        for _ in 0..template.weight {
            workload.push(template.clone());
        }
    }
    let mut rng = StdRng::seed_from_u64(seed);
    workload.shuffle(&mut rng);
    (frames, workload)
}

fn run_add_sample(target_samples: Option<usize>, use_timestamps: bool, seed: u64) {
    let sample_types = sample_types();
    let thread_id = get_current_thread_id();
    let (frames, workload) = build_profiler_workload(seed);
    let mut profile = profiling::internal::Profile::try_new(&sample_types, None).unwrap();
    let stacks: Vec<_> = workload
        .iter()
        .map(|template| make_stack_api(&frames[template.frame_range.clone()]))
        .collect();
    let mut processed = 0usize;
    while should_continue(processed, target_samples) {
        for _ in 0..batch_size(processed, target_samples) {
            let template = &workload[processed % workload.len()];
            let (locations, values) = &stacks[processed % stacks.len()];
            let sample = api::Sample {
                locations: locations.clone(),
                values,
                labels: vec![
                    api::Label {
                        key: "thread id",
                        str: "",
                        num: thread_id + (processed % 7) as i64,
                        num_unit: "",
                    },
                    api::Label {
                        key: "thread name",
                        str: THREAD_NAME,
                        num: 0,
                        num_unit: "",
                    },
                    api::Label {
                        key: "class name",
                        str: template.class_name,
                        num: 0,
                        num_unit: "",
                    },
                    api::Label {
                        key: "category",
                        str: template.category,
                        num: 0,
                        num_unit: "",
                    },
                    api::Label {
                        key: "runtime id",
                        str: RUNTIME_ID,
                        num: 0,
                        num_unit: "",
                    },
                    api::Label {
                        key: "seed",
                        str: "",
                        num: seed as i64,
                        num_unit: "",
                    },
                ],
            };
            let timestamp = next_timestamp(processed, use_timestamps);
            profile.try_add_sample(sample, timestamp).unwrap();
            processed += 1;
        }
    }
    eprintln!(
        "stopped add_sample with {} aggregated samples after {} samples timestamps={} seed={} peak_rss_bytes={}",
        profile.only_for_testing_num_aggregated_samples(),
        processed,
        use_timestamps,
        seed,
        peak_rss_bytes()
    );
}

fn run_add_sample2(target_samples: Option<usize>, use_timestamps: bool, seed: u64) {
    let sample_types = sample_types();
    let (frames, workload) = build_profiler_workload(seed);
    let dict = profiling::profiles::datatypes::ProfilesDictionary::try_new().unwrap();
    let strings = dict.strings();
    let functions = dict.functions();
    let thread_id = get_current_thread_id();
    let thread_id_key: StringId2 = strings.try_insert("thread id").unwrap().into();
    let thread_name_key: StringId2 = strings.try_insert("thread name").unwrap().into();
    let class_name_key: StringId2 = strings.try_insert("class name").unwrap().into();
    let category_key: StringId2 = strings.try_insert("category").unwrap().into();
    let runtime_id_key: StringId2 = strings.try_insert("runtime id").unwrap().into();
    let seed_key: StringId2 = strings.try_insert("seed").unwrap().into();
    let frames2: Vec<_> = frames
        .iter()
        .map(|frame| {
            let set_id = functions
                .try_insert(Function {
                    name: strings.try_insert(frame.function_name).unwrap(),
                    system_name: Default::default(),
                    file_name: strings.try_insert(frame.file_name).unwrap(),
                })
                .unwrap();
            Frame2 {
                function: FunctionId2::from(set_id),
                line_number: frame.line_number,
            }
        })
        .collect();
    let dict = profiling::profiles::collections::Arc::try_new(dict).unwrap();
    let mut profile =
        profiling::internal::Profile::try_new_with_dictionary(&sample_types, None, dict).unwrap();
    let stacks: Vec<_> = workload
        .iter()
        .map(|template| make_stack_api2(&frames2[template.frame_range.clone()]))
        .collect();
    let mut processed = 0usize;
    while should_continue(processed, target_samples) {
        for _ in 0..batch_size(processed, target_samples) {
            let template = &workload[processed % workload.len()];
            let (locations, values) = &stacks[processed % stacks.len()];
            let labels_iter = [
                Ok(api2::Label::num(
                    thread_id_key,
                    thread_id + (processed % 7) as i64,
                    "",
                )),
                Ok(api2::Label::str(thread_name_key, THREAD_NAME)),
                Ok(api2::Label::str(class_name_key, template.class_name)),
                Ok(api2::Label::str(category_key, template.category)),
                Ok(api2::Label::str(runtime_id_key, RUNTIME_ID)),
                Ok(api2::Label::num(seed_key, seed as i64, "")),
            ]
            .into_iter();
            let timestamp = next_timestamp(processed, use_timestamps);
            unsafe {
                profile
                    .try_add_sample2(&locations, &values, labels_iter, timestamp)
                    .unwrap();
            }
            processed += 1;
        }
    }
    eprintln!(
        "stopped add_sample2 with {} aggregated samples after {} samples timestamps={} seed={} peak_rss_bytes={}",
        profile.only_for_testing_num_aggregated_samples(),
        processed,
        use_timestamps,
        seed,
        peak_rss_bytes()
    );
}

#[cfg(feature = "dynamic_profile")]
fn run_dynamic(target_samples: Option<usize>, use_timestamps: bool, seed: u64) {
    let sample_types = sample_types();
    let (frames, workload) = build_profiler_workload(seed);
    let thread_id = get_current_thread_id();
    let mut profile = DynamicProfile::try_new(&sample_types, None, None).unwrap();
    let thread_id_key = profile.intern_string("thread id").unwrap();
    let thread_name_key = profile.intern_string("thread name").unwrap();
    let class_name_key = profile.intern_string("class name").unwrap();
    let category_key = profile.intern_string("category").unwrap();
    let runtime_id_key = profile.intern_string("runtime id").unwrap();
    let seed_key = profile.intern_string("seed").unwrap();
    let frames_dynamic: Vec<_> = frames
        .iter()
        .map(|frame| {
            let name = profile.intern_string(frame.function_name).unwrap();
            let file = profile.intern_string(frame.file_name).unwrap();
            let function = profile.intern_function(name, file).unwrap();
            DynamicFrame {
                function,
                line_number: frame.line_number,
            }
        })
        .collect();
    let stacks: Vec<_> = workload
        .iter()
        .map(|template| make_stack_dynamic(&frames_dynamic[template.frame_range.clone()]))
        .collect();
    let mut processed = 0usize;
    while should_continue(processed, target_samples) {
        for _ in 0..batch_size(processed, target_samples) {
            let template = &workload[processed % workload.len()];
            let (locations, values) = &stacks[processed % stacks.len()];
            let labels = [
                DynamicLabel {
                    key: thread_id_key,
                    str: "",
                    num: thread_id + (processed % 7) as i64,
                },
                DynamicLabel {
                    key: thread_name_key,
                    str: THREAD_NAME,
                    num: 0,
                },
                DynamicLabel {
                    key: class_name_key,
                    str: template.class_name,
                    num: 0,
                },
                DynamicLabel {
                    key: category_key,
                    str: template.category,
                    num: 0,
                },
                DynamicLabel {
                    key: runtime_id_key,
                    str: RUNTIME_ID,
                    num: 0,
                },
                DynamicLabel {
                    key: seed_key,
                    str: "",
                    num: seed as i64,
                },
            ];
            let sample = DynamicSample {
                values,
                labels: &labels,
            };
            let timestamp_ns = next_timestamp_ns(processed, use_timestamps);
            profile
                .add_sample_by_locations(&locations, sample, timestamp_ns)
                .unwrap();
            processed += 1;
        }
    }
    eprintln!(
        "stopped dynamic mode after {} samples timestamps={} seed={} peak_rss_bytes={}",
        processed,
        use_timestamps,
        seed,
        peak_rss_bytes()
    );
}

fn usage(program: &str) -> ! {
    eprintln!(
        "usage: {program} <add_sample|add_sample2|dynamic> [samples] [--timestamps] [--seed=N]"
    );
    process::exit(2);
}

fn main() {
    install_signal_handlers();
    let mut args = env::args();
    let program = args.next().unwrap_or_else(|| "ingest_loop".to_string());
    let mode = args.next().unwrap_or_else(|| usage(&program));
    let mut target_samples = None;
    let mut use_timestamps = false;
    let mut seed = DEFAULT_WORKLOAD_SEED;
    for arg in args {
        if arg == "--timestamps" {
            use_timestamps = true;
        } else if let Some(value) = arg.strip_prefix("--seed=") {
            seed = value
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("invalid seed: {value}"));
        } else if target_samples.is_none() {
            target_samples = Some(
                arg.parse::<usize>()
                    .unwrap_or_else(|_| panic!("invalid sample count: {arg}")),
            );
        } else {
            usage(&program);
        }
    }

    eprintln!(
        "pid={} mode={} target_samples={:?} timestamps={} seed={} send SIGUSR1, SIGUSR2, SIGINT, or SIGTERM to stop",
        process::id(),
        mode,
        target_samples,
        use_timestamps,
        seed
    );

    match mode.as_str() {
        "add_sample" => run_add_sample(target_samples, use_timestamps, seed),
        "add_sample2" => run_add_sample2(target_samples, use_timestamps, seed),
        #[cfg(feature = "dynamic_profile")]
        "dynamic" => run_dynamic(target_samples, use_timestamps, seed),
        #[cfg(not(feature = "dynamic_profile"))]
        "dynamic" => {
            eprintln!("dynamic mode requires --features dynamic_profile");
            process::exit(2);
        }
        _ => usage(&program),
    }
}
