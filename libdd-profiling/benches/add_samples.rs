// Copyright 2025-Present Datadog, Inc.
// SPDX-License-Identifier: Apache-2.0

use criterion::*;
use libdd_profiling::api2::Location2;
use libdd_profiling::profiles::datatypes::{Function, FunctionId2, MappingId2};
use libdd_profiling::{self as profiling, api, api2};

fn make_sample_types() -> Vec<api::ValueType<'static>> {
    vec![api::ValueType::new("samples", "count")]
}

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

    let frames2 = frames.map(|f| {
        let set_id = functions
            .try_insert(Function {
                name: strings.try_insert(f.file_name).unwrap(),
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

    c.bench_function("profile_add_sample_frames_x1000", |b| {
        b.iter(|| {
            let mut profile = profiling::internal::Profile::try_new(&sample_types, None).unwrap();
            let (locations, values) = make_stack_api(frames.as_slice());
            for _ in 0..1000 {
                let sample = api::Sample {
                    locations: locations.clone(),
                    values: &values,
                    labels: vec![],
                };
                black_box(profile.try_add_sample(sample, None)).unwrap();
            }
            black_box(profile.only_for_testing_num_aggregated_samples())
        })
    });

    c.bench_function("profile_add_sample2_frames_x1000", |b| {
        b.iter(|| {
            let mut profile = profiling::internal::Profile::try_new_with_dictionary(
                &sample_types,
                None,
                dict.try_clone().unwrap(),
            )
            .unwrap();
            let (locations, values) = make_stack_api2(frames2.as_slice());
            for _ in 0..1000 {
                // Provide an empty iterator for labels conversion path
                let labels_iter = std::iter::empty::<anyhow::Result<api2::Label>>();
                // SAFETY: all ids come from the profile's dictionary.
                black_box(unsafe {
                    profile.try_add_sample2(&locations, &values, labels_iter, None)
                })
                .unwrap();
            }
            black_box(profile.only_for_testing_num_aggregated_samples())
        })
    });
}

criterion_group!(benches, bench_add_sample_vs_add2);
