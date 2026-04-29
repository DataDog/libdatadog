// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::alloc::System;
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BatchSize, Criterion};
use libdd_common::bench_utils::{
    memory_allocated_measurement, AllocatedBytesMeasurement, ReportingAllocator,
};
use libdd_sampling::{v04_span::V04SamplingData, DatadogSampler, SamplingRule};
use libdd_trace_utils::span::{v04::Span, SliceData};

#[global_allocator]
static GLOBAL: ReportingAllocator<System> = ReportingAllocator::new(System);

// ---------------------------------------------------------------------------
// Benchmark scenario
// ---------------------------------------------------------------------------

struct BenchConfig {
    name: &'static str,
    sampler: DatadogSampler,
    is_parent_sampled: Option<bool>,
    span: Span<SliceData<'static>>,
}

fn make_span(
    name: &'static str,
    service: &'static str,
    resource: &'static str,
) -> Span<SliceData<'static>> {
    Span {
        name,
        service,
        resource,
        trace_id: 0x1234_5678_9012_3456_7890_1234_5678_9012_u128,
        ..Default::default()
    }
}

fn make_configs() -> Vec<BenchConfig> {
    vec![
        // 1. Root span, no rules — falls back to agent/default sampling
        BenchConfig {
            name: "root_span_no_rules",
            sampler: DatadogSampler::new(vec![], 100),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // 2. Parent sampled — short-circuits before any rule evaluation
        BenchConfig {
            name: "parent_sampled_short_circuit",
            sampler: DatadogSampler::new(
                vec![SamplingRule::new(
                    1.0,
                    Some("my-service".into()),
                    Some("http.*".into()),
                    None,
                    None,
                    None,
                )],
                100,
            ),
            is_parent_sampled: Some(true),
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // 3. Matching service rule
        BenchConfig {
            name: "service_rule_matching",
            sampler: DatadogSampler::new(
                vec![SamplingRule::new(
                    1.0,
                    Some("my-service".into()),
                    None,
                    None,
                    None,
                    None,
                )],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // 4. Non-matching service rule — falls through to default
        BenchConfig {
            name: "service_rule_not_matching",
            sampler: DatadogSampler::new(
                vec![SamplingRule::new(
                    1.0,
                    Some("other-service".into()),
                    None,
                    None,
                    None,
                    None,
                )],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // 5. Name pattern rule — matching
        BenchConfig {
            name: "name_pattern_rule_matching",
            sampler: DatadogSampler::new(
                vec![SamplingRule::new(
                    1.0,
                    None,
                    Some("http.*".into()),
                    None,
                    None,
                    None,
                )],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
        // 6. Name pattern rule — not matching
        BenchConfig {
            name: "name_pattern_rule_not_matching",
            sampler: DatadogSampler::new(
                vec![SamplingRule::new(
                    1.0,
                    None,
                    Some("http.*".into()),
                    None,
                    None,
                    None,
                )],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("grpc.request", "my-service", "GetUser"),
        },
        // 7. Resource pattern rule — matching
        BenchConfig {
            name: "resource_pattern_rule_matching",
            sampler: DatadogSampler::new(
                vec![SamplingRule::new(
                    1.0,
                    None,
                    None,
                    Some("/api/*".into()),
                    None,
                    None,
                )],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "/api/users"),
        },
        // 8. Resource pattern rule — not matching
        BenchConfig {
            name: "resource_pattern_rule_not_matching",
            sampler: DatadogSampler::new(
                vec![SamplingRule::new(
                    1.0,
                    None,
                    None,
                    Some("/api/*".into()),
                    None,
                    None,
                )],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("http.request", "my-service", "/health"),
        },
        // 9. Tag rule — matching
        {
            let mut span = make_span("test-operation", "my-service", "test");
            span.meta.insert("environment", "production");
            BenchConfig {
                name: "tag_rule_matching",
                sampler: DatadogSampler::new(
                    vec![SamplingRule::new(
                        1.0,
                        None,
                        None,
                        None,
                        Some(std::collections::HashMap::from([(
                            "environment".to_string(),
                            "production".to_string(),
                        )])),
                        None,
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // 10. Tag rule — not matching
        {
            let mut span = make_span("test-operation", "my-service", "test");
            span.meta.insert("environment", "staging");
            BenchConfig {
                name: "tag_rule_not_matching",
                sampler: DatadogSampler::new(
                    vec![SamplingRule::new(
                        1.0,
                        None,
                        None,
                        None,
                        Some(std::collections::HashMap::from([(
                            "environment".to_string(),
                            "production".to_string(),
                        )])),
                        None,
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // 11. Complex rule — all fields matching
        {
            let mut span = make_span("http.request", "api-service", "/api/v1/users");
            span.meta.insert("environment", "production");
            span.meta.insert("http.method", "POST");
            span.meta.insert("http.route", "/api/v1/users");
            BenchConfig {
                name: "complex_rule_matching",
                sampler: DatadogSampler::new(
                    vec![SamplingRule::new(
                        0.5,
                        Some("api-service".into()),
                        Some("http.*".into()),
                        Some("/api/v1/*".into()),
                        Some(std::collections::HashMap::from([(
                            "environment".to_string(),
                            "production".to_string(),
                        )])),
                        None,
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // 12. Complex rule — partial match (resource doesn't match)
        {
            let mut span = make_span("http.request", "api-service", "/health");
            span.meta.insert("environment", "staging");
            span.meta.insert("http.method", "POST");
            span.meta.insert("http.route", "/health");
            BenchConfig {
                name: "complex_rule_partial_match",
                sampler: DatadogSampler::new(
                    vec![SamplingRule::new(
                        0.5,
                        Some("api-service".into()),
                        Some("http.*".into()),
                        Some("/api/v1/*".into()),
                        Some(std::collections::HashMap::from([(
                            "environment".to_string(),
                            "production".to_string(),
                        )])),
                        None,
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // 13. Multiple rules — first one matches
        BenchConfig {
            name: "multiple_rules_first_match",
            sampler: DatadogSampler::new(
                vec![
                    SamplingRule::new(0.1, Some("api-service".into()), None, None, None, None),
                    SamplingRule::new(0.5, Some("web-service".into()), None, None, None, None),
                    SamplingRule::new(1.0, None, None, None, None, None),
                ],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("test-operation", "api-service", "test"),
        },
        // 14. Multiple rules — last one matches (all prior rules evaluated)
        BenchConfig {
            name: "multiple_rules_last_match",
            sampler: DatadogSampler::new(
                vec![
                    SamplingRule::new(0.1, Some("api-service".into()), None, None, None, None),
                    SamplingRule::new(0.5, Some("web-service".into()), None, None, None, None),
                    SamplingRule::new(1.0, None, None, None, None, None),
                ],
                100,
            ),
            is_parent_sampled: None,
            span: make_span("grpc.request", "other-service", "GetUser"),
        },
        // 15. Many meta entries with a tag rule — matching entry is near the end
        {
            let mut span = make_span("test-operation", "my-service", "test");
            for (k, v) in MANY_ATTR_PAIRS {
                span.meta.insert(k, v);
            }
            BenchConfig {
                name: "many_attributes_tag_rule",
                sampler: DatadogSampler::new(
                    vec![SamplingRule::new(
                        1.0,
                        None,
                        None,
                        None,
                        Some(std::collections::HashMap::from([(
                            "key10".to_string(),
                            "value10".to_string(),
                        )])),
                        None,
                    )],
                    100,
                ),
                is_parent_sampled: None,
                span,
            }
        },
        // 16. Parent not sampled — short-circuits before rule evaluation
        BenchConfig {
            name: "parent_not_sampled_short_circuit",
            sampler: DatadogSampler::new(
                vec![SamplingRule::new(
                    1.0,
                    Some("my-service".into()),
                    Some("http.*".into()),
                    None,
                    None,
                    None,
                )],
                100,
            ),
            is_parent_sampled: Some(false),
            span: make_span("http.request", "my-service", "GET /api/v1/users"),
        },
    ]
}

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

pub fn criterion_benchmark(c: &mut Criterion) {
    let configs = make_configs();

    for config in &configs {
        c.bench_function(
            &format!("datadog_sample_span/{}/wall_time", config.name),
            |b| {
                b.iter_batched(
                    || (),
                    |_| {
                        let data = V04SamplingData {
                            is_parent_sampled: config.is_parent_sampled,
                            span: &config.span,
                        };
                        black_box(config.sampler.sample(black_box(&data)));
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    }
}

fn criterion_benchmark_allocs(c: &mut Criterion<AllocatedBytesMeasurement<System>>) {
    let configs = make_configs();

    for config in &configs {
        c.bench_function(
            &format!("datadog_sample_span/{}/allocated_bytes", config.name),
            |b| {
                b.iter_batched(
                    || (),
                    |_| {
                        let data = V04SamplingData {
                            is_parent_sampled: config.is_parent_sampled,
                            span: &config.span,
                        };
                        black_box(config.sampler.sample(black_box(&data)));
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    }
}

criterion_group!(benches, criterion_benchmark);
criterion_group!(
    name = alloc_benches;
    config = memory_allocated_measurement(&GLOBAL);
    targets = criterion_benchmark_allocs
);
criterion_main!(benches, alloc_benches);
