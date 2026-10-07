// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Benchmarks for sampling through the trace-model traits on v1 spans.
//!
//! Each scenario runs `DatadogSampler::sample` on a `v1::Span<T>` via
//! `ModelSamplingData`, for both text representations (`SliceData` borrowing
//! static strings, `BytesData` owning refcounted strings). The
//! `apply_sampling_tags` benchmarks additionally measure producing the sampling
//! tags and writing them back to the span.

use std::alloc::System;
use std::collections::HashMap;
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use libdd_common::bench_utils::{
    AllocatedBytesMeasurement, ReportingAllocator, memory_allocated_criterion,
};
use libdd_sampling::DatadogSampler;
use libdd_sampling::SamplingRule;
use libdd_sampling::trace_model_span::{ModelAttributeFactory, ModelSamplingData};
use libdd_trace_model::{AttributeValue, Attributes};
use libdd_trace_utils::span::v1::Span;
use libdd_trace_utils::span::{BytesData, SliceData, SpanText, TraceData};

#[global_allocator]
static GLOBAL: ReportingAllocator<System> = ReportingAllocator::new(System);

const TRACE_ID: u128 = 0x1234_5678_9012_3456_7890_1234_5678_9012;

static MANY_ATTR_PAIRS: &[(&str, &str)] = &[
    ("key0", "value0"),
    ("key1", "value1"),
    ("key2", "value2"),
    ("key3", "value3"),
    ("key4", "value4"),
    ("key5", "value5"),
    ("key6", "value6"),
    ("key7", "value7"),
    ("key8", "value8"),
    ("key9", "value9"),
    ("key10", "value10"),
    ("key11", "value11"),
    ("key12", "value12"),
    ("key13", "value13"),
    ("key14", "value14"),
    ("key15", "value15"),
    ("key16", "value16"),
    ("key17", "value17"),
    ("key18", "value18"),
    ("key19", "value19"),
];

struct BenchConfig<T: TraceData> {
    name: &'static str,
    sampler: DatadogSampler,
    is_parent_sampled: Option<bool>,
    span: Span<T>,
}

fn make_span<T: TraceData>(
    name: &'static str,
    service: &'static str,
    resource: &'static str,
) -> Span<T> {
    Span {
        name: T::Text::from_static_str(name),
        service: T::Text::from_static_str(service),
        resource: T::Text::from_static_str(resource),
        ..Default::default()
    }
}

fn set_str<T: TraceData>(span: &mut Span<T>, key: &'static str, value: &'static str) {
    span.set_attribute(
        T::Text::from_static_str(key),
        AttributeValue::String(T::Text::from_static_str(value)),
    );
}

fn set_int<T: TraceData>(span: &mut Span<T>, key: &'static str, value: i64) {
    span.set_attribute(T::Text::from_static_str(key), AttributeValue::Int(value));
}

fn rule(
    sample_rate: f64,
    service: Option<&str>,
    name: Option<&str>,
    resource: Option<&str>,
    tags: Option<HashMap<String, String>>,
) -> SamplingRule {
    SamplingRule::new(
        sample_rate,
        service.map(Into::into),
        name.map(Into::into),
        resource.map(Into::into),
        tags,
        None,
    )
}

fn make_configs<T: TraceData>() -> Vec<BenchConfig<T>> {
    vec![
        // Root span, no rules. Falls through to rate limiting and default keep.
        BenchConfig {
            name: "root_span_no_rules",
            sampler: DatadogSampler::new(vec![], 100),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // Parent sampled. Short-circuits before any rule evaluation.
        BenchConfig {
            name: "parent_sampled_short_circuit",
            sampler: DatadogSampler::new(
                vec![rule(1.0, Some("my-service"), Some("http.*"), None, None)],
                100,
            ),
            is_parent_sampled: Some(true),
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // Service rule, matching.
        BenchConfig {
            name: "service_rule_matching",
            sampler: DatadogSampler::new(
                vec![rule(1.0, Some("my-service"), None, None, None)],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // Service rule, not matching.
        BenchConfig {
            name: "service_rule_not_matching",
            sampler: DatadogSampler::new(
                vec![rule(1.0, Some("other-service"), None, None, None)],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // Name pattern rule, matching.
        BenchConfig {
            name: "name_pattern_rule_matching",
            sampler: DatadogSampler::new(vec![rule(1.0, None, Some("http.*"), None, None)], 100),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // Name pattern rule, not matching.
        BenchConfig {
            name: "name_pattern_rule_not_matching",
            sampler: DatadogSampler::new(vec![rule(1.0, None, Some("http.*"), None, None)], 100),
            is_parent_sampled: None,
            span: make_span("grpc.request", "my-service", "GetUser"),
        },
        // Tag rule against a String attribute, matching.
        {
            let mut span = make_span::<T>("test-operation", "my-service", "test");
            set_str(&mut span, "environment", "production");
            BenchConfig {
                name: "tag_rule_matching",
                sampler: DatadogSampler::new(
                    vec![rule(
                        1.0,
                        None,
                        None,
                        None,
                        Some(HashMap::from([(
                            "environment".to_string(),
                            "production".to_string(),
                        )])),
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // Tag rule, not matching.
        {
            let mut span = make_span::<T>("test-operation", "my-service", "test");
            set_str(&mut span, "environment", "staging");
            BenchConfig {
                name: "tag_rule_not_matching",
                sampler: DatadogSampler::new(
                    vec![rule(
                        1.0,
                        None,
                        None,
                        None,
                        Some(HashMap::from([(
                            "environment".to_string(),
                            "production".to_string(),
                        )])),
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // HTTP status code rule, matching via the Int attribute path. The rule uses
        // the Datadog key; the span carries the OTel key, so status code extraction
        // goes through `ModelSpanProperties::status_code`.
        {
            let mut span = make_span::<T>("http.request", "my-service", "GET /api/v1/users");
            set_int(&mut span, "http.response.status_code", 200);
            BenchConfig {
                name: "http_status_code_rule_matching",
                sampler: DatadogSampler::new(
                    vec![rule(
                        1.0,
                        None,
                        None,
                        None,
                        Some(HashMap::from([(
                            "http.status_code".to_string(),
                            "2*".to_string(),
                        )])),
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // Rule combining service, name, resource and a tag, all matching.
        {
            let mut span = make_span::<T>("http.request", "api-service", "/api/v1/users");
            set_str(&mut span, "environment", "production");
            BenchConfig {
                name: "complex_rule_matching",
                sampler: DatadogSampler::new(
                    vec![rule(
                        0.5,
                        Some("api-service"),
                        Some("http.*"),
                        Some("/api/v1/*"),
                        Some(HashMap::from([(
                            "environment".to_string(),
                            "production".to_string(),
                        )])),
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // Tag rule where the matching attribute is near the end of a 21-entry map.
        {
            let mut span = make_span::<T>("test-operation", "my-service", "test");
            for (key, value) in MANY_ATTR_PAIRS {
                set_str(&mut span, key, value);
            }
            BenchConfig {
                name: "many_attributes_tag_rule",
                sampler: DatadogSampler::new(
                    vec![rule(
                        1.0,
                        None,
                        None,
                        None,
                        Some(HashMap::from([(
                            "key10".to_string(),
                            "value10".to_string(),
                        )])),
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // Multiple rules, only the last matches. All prior rules are evaluated.
        BenchConfig {
            name: "multiple_rules_last_match",
            sampler: DatadogSampler::new(
                vec![
                    rule(0.1, Some("api-service"), None, None, None),
                    rule(0.5, Some("web-service"), None, None, None),
                    rule(1.0, None, None, None, None),
                ],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("grpc.request", "other-service", "GetUser"),
        },
    ]
}

fn sample_configs<T: TraceData>(c: &mut Criterion, type_name: &str) {
    let configs = make_configs::<T>();
    for config in &configs {
        c.bench_function(
            &format!(
                "trace_model_sample_span/{}/{}/wall_time",
                config.name, type_name
            ),
            |b| {
                b.iter(|| {
                    let data = ModelSamplingData {
                        is_parent_sampled: config.is_parent_sampled,
                        trace_id: TRACE_ID,
                        span: &config.span,
                    };
                    black_box(config.sampler.sample(black_box(&data)));
                })
            },
        );
    }
}

fn apply_tags_configs<T: TraceData>(c: &mut Criterion, type_name: &str) {
    // `to_dd_sampling_tags` only produces tags for root spans, so this measures the
    // full root-path decision plus tag creation and write-back.
    let sampler = DatadogSampler::new(vec![], 100);
    let mut span = make_span::<T>("http.request", "my-service", "GET /api/v1/users");
    let factory = ModelAttributeFactory::<T>::new();

    c.bench_function(
        &format!("trace_model_apply_tags/{}/wall_time", type_name),
        |b| {
            b.iter(|| {
                let data = ModelSamplingData {
                    is_parent_sampled: None,
                    trace_id: TRACE_ID,
                    span: &span,
                };
                let result = sampler.sample(black_box(&data));
                if let Some(tags) = result.to_dd_sampling_tags(&factory) {
                    for tag in tags {
                        tag.apply_to(&mut span);
                    }
                }
            })
        },
    );
}

pub fn criterion_benchmark(c: &mut Criterion) {
    sample_configs::<SliceData<'static>>(c, "slice_data");
    sample_configs::<BytesData>(c, "bytes_data");
    apply_tags_configs::<SliceData<'static>>(c, "slice_data");
    apply_tags_configs::<BytesData>(c, "bytes_data");
}

fn criterion_benchmark_allocs(c: &mut Criterion<AllocatedBytesMeasurement<System>>) {
    let mut run = |configs: Vec<BenchConfig<BytesData>>| {
        for config in &configs {
            c.bench_function(
                &format!(
                    "trace_model_sample_span/{}/bytes_data/allocated_bytes",
                    config.name
                ),
                |b| {
                    b.iter(|| {
                        let data = ModelSamplingData {
                            is_parent_sampled: config.is_parent_sampled,
                            trace_id: TRACE_ID,
                            span: &config.span,
                        };
                        black_box(config.sampler.sample(black_box(&data)));
                    })
                },
            );
        }
    };
    run(make_configs::<BytesData>());
}

criterion_group!(benches, criterion_benchmark);

// Not `criterion_group!`: its `config =` would be overridden by the command-line flags.
fn alloc_benches() {
    criterion_benchmark_allocs(&mut memory_allocated_criterion(&GLOBAL));
}

criterion_main!(alloc_benches, benches);
