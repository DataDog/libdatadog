// Copyright 2025-Present Datadog, Inc.
// SPDX-License-Identifier: Apache-2.0

use criterion::*;
use libdd_common::threading::get_current_thread_id;
use libdd_profiling::api2::Location2;
use libdd_profiling::profiles::datatypes::{Function, FunctionId2, MappingId2, StringId2};
use libdd_profiling::{self as profiling, api, api2};

#[cfg(feature = "dynamic_profile")]
use libdd_profiling::dynamic::{DynamicLabel, DynamicLocation, DynamicProfile, DynamicSample};

fn make_sample_types() -> Vec<api::SampleType> {
    vec![api::SampleType::CpuSamples]
}

const THREAD_NAME: &str = "this thread";

fn make_stack_api(frames: &[Frame]) -> (Vec<api::Location<'static>>, Vec<i64>) {
    // No mappings in Ruby, but the v1 API requires it.
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
    let values = vec![1i64];
    (locations, values)
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

    let values = vec![1i64];
    (locations, values)
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

    let values = vec![1i64];
    (locations, values)
}

fn make_timestamped_profile(
    sample_types: &[api::SampleType],
    frames: &[Frame],
    labels: &[api::Label<'static>],
) -> profiling::internal::Profile {
    let mut profile = profiling::internal::Profile::try_new(sample_types, None).unwrap();
    let (locations, values) = make_stack_api(frames);

    for i in 0..1000 {
        let sample = api::Sample {
            locations: locations.clone(),
            values: &values,
            labels: labels.to_vec(),
        };
        let ts = std::num::NonZeroI64::new(i + 1);
        black_box(profile.try_add_sample(sample, ts)).unwrap();
    }

    profile
}

#[derive(Clone, Copy)]
struct Frame {
    file_name: &'static str,
    line_number: u32,
    function_name: &'static str,
}

impl Frame {
    pub const fn new(
        file_name: &'static str,
        line_number: u32,
        function_name: &'static str,
    ) -> Self {
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

pub fn bench_add_sample_vs_add2(c: &mut Criterion) {
    let sample_types = make_sample_types();
    let dict = profiling::profiles::datatypes::ProfilesDictionary::try_new().unwrap();

    // This is root-to-leaf, instead of leaf-to-root. We'll reverse it below.
    // Taken from a Ruby app, everything here is source-available.
    let mut frames = [
        Frame::new("/usr/local/bundle/gems/logging-2.4.0/lib/logging/diagnostic_context.rb", 474, "create_with_logging_context"),
        Frame::new("/usr/local/bundle/gems/puma-6.4.3/lib/puma/thread_pool.rb", 155, "spawn_thread"),
        Frame::new("/usr/local/bundle/gems/puma-6.4.3/lib/puma/server.rb", 245, "run"),
        Frame::new("/usr/local/bundle/gems/puma-6.4.3/lib/puma/server.rb", 464, "process_client"),
        Frame::new("/usr/local/bundle/gems/puma-6.4.3/lib/puma/request.rb", 99, "handle_request"),
        Frame::new("/usr/local/bundle/gems/puma-6.4.3/lib/puma/thread_pool.rb", 378, "with_force_shutdown"),
        Frame::new("/usr/local/bundle/gems/puma-6.4.3/lib/puma/request.rb", 100, "handle_request"),
        Frame::new("/usr/local/bundle/gems/puma-6.4.3/lib/puma/configuration.rb", 272, "call"),
        Frame::new("/usr/local/bundle/gems/railties-7.0.8.7/lib/rails/engine.rb", 530, "call"),
        Frame::new("/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/tracing/contrib/rack/middlewares.rb", 474, "call"),
        Frame::new("/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/tracing/contrib/rack/trace_proxy_middleware.rb", 17, "call"),
        Frame::new("/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/tracing/contrib/rack/middlewares.rb", 70, "call"),
        Frame::new("/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/contrib/rack/request_middleware.rb", 82, "call"),
        Frame::new("/usr/local/lib/libruby.so.3.3", 0, "catch"),
        Frame::new("/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/contrib/rack/request_middleware.rb", 85, "catch"),
        Frame::new("/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/instrumentation/gateway.rb", 41, "push"),
        Frame::new("/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/instrumentation/gateway.rb", 37, "push"),
        Frame::new("/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/instrumentation/gateway/middleware.rb", 18, "call"),
    ];
    frames.reverse();

    let strings = dict.strings();
    let functions = dict.functions();
    let thread_id = get_current_thread_id();
    let thread_id_key: StringId2 = strings.try_insert("thread id").unwrap().into();
    let labels_api = vec![
        api::Label {
            key: "thread id",
            str: "",
            num: thread_id,
            num_unit: "",
        },
        api::Label {
            key: "thread name",
            str: THREAD_NAME,
            num: 0,
            num_unit: "",
        },
    ];

    let thread_name_key: StringId2 = strings.try_insert("thread name").unwrap().into();
    let frames2 = frames.map(|f| {
        let set_id = functions
            .try_insert(Function {
                name: strings.try_insert(f.function_name).unwrap(),
                system_name: Default::default(),
                file_name: strings.try_insert(f.file_name).unwrap(),
            })
            .unwrap();
        Frame2 {
            function: FunctionId2::from(set_id),
            line_number: f.line_number,
        }
    });
    let dict = profiling::profiles::collections::Arc::try_new(dict).unwrap();

    c.bench_function("profile_add_sample_timestamped_x1000", |b| {
        b.iter(|| {
            let mut profile = profiling::internal::Profile::try_new(&sample_types, None).unwrap();
            let (locations, values) = make_stack_api(frames.as_slice());
            for i in 0..1000 {
                let sample = api::Sample {
                    locations: locations.clone(),
                    values: &values,
                    labels: labels_api.clone(),
                };
                let ts = std::num::NonZeroI64::new(i + 1);
                black_box(profile.try_add_sample(sample, ts)).unwrap();
            }
            black_box(profile.only_for_testing_num_aggregated_samples())
        })
    });

    c.bench_function("profile_add_sample_frames_x1000_same_input", |b| {
        b.iter_batched(
            || {
                let profile = profiling::internal::Profile::try_new(&sample_types, None).unwrap();
                let (locations, values) = make_stack_api(frames.as_slice());
                (profile, locations, values, labels_api.clone())
            },
            |(mut profile, locations, values, labels)| {
                for _ in 0..1000 {
                    let sample = api::Sample {
                        locations: locations.clone(),
                        values: &values,
                        labels: labels.clone(),
                    };
                    black_box(profile.try_add_sample(sample, None)).unwrap();
                }
                black_box(profile.only_for_testing_num_aggregated_samples())
            },
            BatchSize::SmallInput,
        )
    });

    c.bench_function("profile_add_sample2_frames_x1000_same_input", |b| {
        b.iter_batched(
            || {
                let profile = profiling::internal::Profile::try_new_with_dictionary(
                    &sample_types,
                    None,
                    dict.try_clone().unwrap(),
                )
                .unwrap();
                let (locations, values) = make_stack_api2(frames2.as_slice());
                (profile, locations, values)
            },
            |(mut profile, locations, values)| {
                for _ in 0..1000 {
                    let labels_iter = [
                        Ok(api2::Label::num(thread_id_key, thread_id, "")),
                        Ok(api2::Label::str(thread_name_key, THREAD_NAME)),
                    ]
                    .into_iter();
                    // SAFETY: all ids come from the profile's dictionary.
                    black_box(unsafe {
                        profile.try_add_sample2(&locations, &values, labels_iter, None)
                    })
                    .unwrap();
                }
                black_box(profile.only_for_testing_num_aggregated_samples())
            },
            BatchSize::SmallInput,
        )
    });

    #[cfg(feature = "dynamic_profile")]
    c.bench_function(
        "dynamic_profile_add_sample_by_locations_frames_x1000_same_input",
        |b| {
            b.iter_batched(
                || {
                    let mut profile = DynamicProfile::try_new(&sample_types, None, None).unwrap();
                    let thread_id_key = profile.intern_string("thread id").unwrap();
                    let thread_name_key = profile.intern_string("thread name").unwrap();
                    let frames_dynamic = frames.map(|f| {
                        let name = profile.intern_string(f.function_name).unwrap();
                        let file = profile.intern_string(f.file_name).unwrap();
                        let function = profile.intern_function(name, file).unwrap();
                        DynamicFrame {
                            function,
                            line_number: f.line_number,
                        }
                    });
                    let (locations, values) = make_stack_dynamic(frames_dynamic.as_slice());
                    let labels = [
                        DynamicLabel {
                            key: thread_id_key,
                            str: "",
                            num: thread_id,
                        },
                        DynamicLabel {
                            key: thread_name_key,
                            str: THREAD_NAME,
                            num: 0,
                        },
                    ];
                    (profile, locations, values, labels)
                },
                |(mut profile, locations, values, labels)| {
                    for _ in 0..1000 {
                        let sample = DynamicSample {
                            values: &values,
                            labels: &labels,
                        };
                        black_box(profile.add_sample_by_locations(&locations, sample, 0)).unwrap();
                    }
                    black_box(profile)
                },
                BatchSize::SmallInput,
            )
        },
    );

    c.bench_function("profile_add_sample_frames_x1000_steady_state", |b| {
        let mut profile = profiling::internal::Profile::try_new(&sample_types, None).unwrap();
        let (locations, values) = make_stack_api(frames.as_slice());
        let labels = labels_api.clone();
        b.iter(|| {
            for _ in 0..1000 {
                let sample = api::Sample {
                    locations: locations.clone(),
                    values: &values,
                    labels: labels.clone(),
                };
                black_box(profile.try_add_sample(sample, None)).unwrap();
            }
            black_box(profile.only_for_testing_num_aggregated_samples())
        })
    });

    c.bench_function("profile_add_sample2_frames_x1000_steady_state", |b| {
        let mut profile = profiling::internal::Profile::try_new_with_dictionary(
            &sample_types,
            None,
            dict.try_clone().unwrap(),
        )
        .unwrap();
        let (locations, values) = make_stack_api2(frames2.as_slice());
        b.iter(|| {
            for _ in 0..1000 {
                let labels_iter = [
                    Ok(api2::Label::num(thread_id_key, thread_id, "")),
                    Ok(api2::Label::str(thread_name_key, THREAD_NAME)),
                ]
                .into_iter();
                // SAFETY: all ids come from the profile's dictionary.
                black_box(unsafe {
                    profile.try_add_sample2(&locations, &values, labels_iter, None)
                })
                .unwrap();
            }
            black_box(profile.only_for_testing_num_aggregated_samples())
        })
    });

    #[cfg(feature = "dynamic_profile")]
    c.bench_function(
        "dynamic_profile_add_sample_by_locations_frames_x1000_steady_state",
        |b| {
            let mut profile = DynamicProfile::try_new(&sample_types, None, None).unwrap();
            let thread_id_key = profile.intern_string("thread id").unwrap();
            let thread_name_key = profile.intern_string("thread name").unwrap();
            let frames_dynamic = frames.map(|f| {
                let name = profile.intern_string(f.function_name).unwrap();
                let file = profile.intern_string(f.file_name).unwrap();
                let function = profile.intern_function(name, file).unwrap();
                DynamicFrame {
                    function,
                    line_number: f.line_number,
                }
            });
            let (locations, values) = make_stack_dynamic(frames_dynamic.as_slice());
            let labels = [
                DynamicLabel {
                    key: thread_id_key,
                    str: "",
                    num: thread_id,
                },
                DynamicLabel {
                    key: thread_name_key,
                    str: THREAD_NAME,
                    num: 0,
                },
            ];
            b.iter(|| {
                for _ in 0..1000 {
                    let sample = DynamicSample {
                        values: &values,
                        labels: &labels,
                    };
                    black_box(profile.add_sample_by_locations(&locations, sample, 0)).unwrap();
                }
                black_box(())
            })
        },
    );

    c.bench_function(
        "profile_serialize_compressed_pprof_timestamped_x1000",
        |b| {
            b.iter_batched(
                || {
                    make_timestamped_profile(
                        &sample_types,
                        frames.as_slice(),
                        labels_api.as_slice(),
                    )
                },
                |profile| black_box(profile.serialize_into_compressed_pprof(None, None)).unwrap(),
                BatchSize::SmallInput,
            )
        },
    );
}

criterion_group!(benches, bench_add_sample_vs_add2);
