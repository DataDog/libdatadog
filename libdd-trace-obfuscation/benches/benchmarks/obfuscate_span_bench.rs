// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Span-obfuscation benchmarks for the trace-model trait path: [`obfuscate_span`],
//! [`replace_span_tags_trait`] and [`obfuscate_chunk_attributes`].
//!
//! Unlike the primitive benches (redis, SQL, credit cards, IP, tags), these exercise the
//! per-attribute obfuscation pass that the agentless exporter runs for every span. Two
//! configuration knobs drive the pass's allocation behavior, and both are covered: the
//! credit-card scan enabled or disabled, and a wildcard replace rule present or absent.
//!
//! Each entry point is measured twice: in wall time and in allocated bytes.

use std::alloc::System;

use criterion::{BatchSize, Criterion, black_box, criterion_group};
use libdd_tinybytes::BytesString;
use libdd_trace_obfuscation::obfuscate::{obfuscate_chunk_attributes, obfuscate_span};
use libdd_trace_obfuscation::obfuscation_config::{CreditCardConfig, ObfuscationConfig};
use libdd_trace_obfuscation::replacer::{self, replace_span_tags_trait};
use libdd_trace_utils::span::v1::{AttributeValue, SpanBytes, SpanEventBytes, TraceChunkBytes};
use tools::bench_utils::{
    AllocatedBytesMeasurement, MeasurementName, ReportingAllocator, memory_allocated_criterion,
};

#[global_allocator]
static GLOBAL: ReportingAllocator<System> = ReportingAllocator::new(System);

/// A wildcard rule matching the `token/<value>` marker embedded in two of the span's
/// attributes, so the replace pass rewrites two values and leaves the rest untouched.
const WILDCARD_RULES: &str = r#"[{"name": "*", "pattern": "(token/)([^/]*)", "repl": "${1}?"}]"#;

fn bs(s: &str) -> BytesString {
    BytesString::from_string(s.to_string())
}

#[allow(clippy::unwrap_used)]
fn config(credit_cards: bool, wildcard_rules: bool) -> ObfuscationConfig {
    ObfuscationConfig {
        credit_cards: CreditCardConfig {
            enabled: credit_cards,
            ..CreditCardConfig::default()
        },
        tag_replace_rules: if wildcard_rules {
            replacer::parse_rules_from_string(WILDCARD_RULES).unwrap()
        } else {
            Vec::new()
        },
        ..ObfuscationConfig::default()
    }
}

/// The `obfuscate_span` matrix: credit-card scan on/off × wildcard rule on/off.
fn span_scenarios() -> Vec<(String, ObfuscationConfig)> {
    let mut scenarios = Vec::new();
    for credit_cards in [true, false] {
        for wildcard_rules in [true, false] {
            scenarios.push((
                format!("cc={credit_cards} rules={wildcard_rules}"),
                config(credit_cards, wildcard_rules),
            ));
        }
    }
    scenarios
}

/// A realistic web span: 13 string and 10 numeric attributes (split between `Int` and
/// `Float`, as the v1 model has no meta/metrics split), one nested map, and two span
/// events. One attribute holds a card-like number, one map entry holds another, and two
/// hold `token/<value>` markers, so the credit-card scan and the wildcard rule each find
/// matches.
fn web_span() -> SpanBytes {
    let mut span = SpanBytes {
        service: bs("bench-service"),
        name: bs("web.request"),
        resource: bs("GET /api/v1/users/12345/profile"),
        r#type: bs("web"),
        span_id: 424_242,
        parent_id: 111_111,
        start: 1_735_689_600_000_000_000,
        duration: 42_000_000,
        ..SpanBytes::default()
    };

    let strings: &[(&str, &str)] = &[
        ("http.url", "http://foo.com/id/123/page/q?search=bar&page=2"),
        ("http.method", "GET"),
        ("http.useragent", "Mozilla/5.0 token/abcdef (bench)"),
        ("http.status_code", "200"),
        ("operation.name", "users.show"),
        ("peer.hostname", "foo.com"),
        ("peer.ipv4", "10.0.0.12"),
        ("language", "go"),
        ("component", "http"),
        ("enduser.id", "user-42"),
        ("order.number", "4111111111111111"),
        ("runtime-id", "token/0123456789abcdef"),
        ("_dd.p.tid", "16a1c5a100000000"),
    ];
    let ints: &[(&str, i64)] = &[
        ("_dd.top_level", 1),
        ("_dd.measured", 1),
        ("system.pid", 12345),
        ("http.response.content_length", 2048),
        ("network.destination.port", 443),
        ("_dd.agent.psml", 1),
    ];
    let floats: &[(&str, f64)] = &[
        ("_dd.trace_span_attribute_schema", 0.1),
        ("_sampling_priority_v1", 1.0),
        ("_dd.span_sampling.rule_rate", 1.0),
        ("_dd.rule_prio", 1.0),
    ];

    for (key, value) in strings {
        span.attributes
            .insert(bs(key), AttributeValue::String(bs(value)));
    }
    for (key, value) in ints {
        span.attributes.insert(bs(key), AttributeValue::Int(*value));
    }
    for (key, value) in floats {
        span.attributes
            .insert(bs(key), AttributeValue::Float(*value));
    }

    // The nested map gives the credit-card scan's recursion a `KeyValueList` to walk. The
    // scan builds a child path for every card-capable entry (skipping non-candidates like
    // `verified`), and the first mutable iteration dedups the map, which allocates.
    span.attributes.insert(
        bs("payment"),
        AttributeValue::KeyValueList(
            vec![
                (bs("number"), AttributeValue::String(bs("4111111111111111"))),
                (bs("retries"), AttributeValue::Int(3)),
                (bs("verified"), AttributeValue::Bool(true)),
            ]
            .into(),
        ),
    );

    for (i, name) in ["event.one", "event.two"].into_iter().enumerate() {
        let mut event = SpanEventBytes {
            name: bs(name),
            ..SpanEventBytes::default()
        };
        event.attributes.insert(
            bs("message"),
            AttributeValue::String(bs(&format!("event {i}"))),
        );
        span.span_events.push(event);
    }

    span
}

/// A span taking the SQL arm. The arm writes the obfuscated query to both the resource and
/// the `sql.query` attribute; reference-counted text types share one allocation between
/// the two.
fn sql_span() -> SpanBytes {
    let mut span = SpanBytes {
        service: bs("bench-service"),
        name: bs("postgres.query"),
        resource: bs("SELECT * FROM users WHERE id = 42"),
        r#type: bs("sql"),
        ..SpanBytes::default()
    };
    span.attributes.insert(
        bs("sql.query"),
        AttributeValue::String(bs("SELECT * FROM users WHERE id = 42")),
    );
    span
}

/// A chunk with chunk-level attributes, holding two spans. `obfuscate_chunk_attributes`
/// only touches the chunk's own attributes, not the spans.
fn trace_chunk() -> TraceChunkBytes {
    TraceChunkBytes {
        trace_id: [0x42; 16],
        priority: Some(1),
        origin: bs("ciapp-test"),
        attributes: {
            let mut chunk = web_span().attributes;
            chunk.insert(bs("origin.version"), AttributeValue::Int(2));
            chunk
        },
        spans: vec![web_span(), web_span()],
        ..TraceChunkBytes::default()
    }
}

fn bench_obfuscate_span<M: criterion::measurement::Measurement + MeasurementName + 'static>(
    c: &mut Criterion<M>,
) {
    let measurement = M::name();
    for (label, config) in span_scenarios() {
        c.bench_function(&format!("obfuscate_span/{label}/{measurement}"), |b| {
            b.iter_batched(
                web_span,
                |mut span| obfuscate_span(black_box(&mut span), black_box(&config)),
                BatchSize::LargeInput,
            );
        });
    }

    // The SQL arm of the matrix, with the credit-card scan off and on. `sql.query` is on
    // the scan's skip list, so the scan's own cost is the only difference between the
    // two rows.
    for (label, config) in [
        ("cc=false rules=false", config(false, false)),
        ("cc=true rules=false", config(true, false)),
    ] {
        c.bench_function(&format!("obfuscate_span/sql/{label}/{measurement}"), |b| {
            b.iter_batched(
                sql_span,
                |mut span| obfuscate_span(black_box(&mut span), black_box(&config)),
                BatchSize::LargeInput,
            );
        });
    }

    // The replace pass alone (`obfuscate_span` runs it last), so its cost can be read
    // separately from the full obfuscation pass.
    #[allow(clippy::unwrap_used)]
    let rules = replacer::parse_rules_from_string(WILDCARD_RULES).unwrap();
    c.bench_function(
        &format!("obfuscate_span/replace_span_tags_trait/{measurement}"),
        |b| {
            b.iter_batched(
                web_span,
                |mut span| {
                    replace_span_tags_trait(black_box(&mut span), black_box(&rules));
                },
                BatchSize::LargeInput,
            );
        },
    );

    // Chunk-level attributes, benched with the default config (CC scan on, no rules) and
    // with the wildcard rule added.
    for (label, config) in [
        ("cc=true rules=false", config(true, false)),
        ("cc=true rules=true", config(true, true)),
    ] {
        c.bench_function(
            &format!("obfuscate_chunk_attributes/{label}/{measurement}"),
            |b| {
                b.iter_batched(
                    trace_chunk,
                    |mut chunk| {
                        obfuscate_chunk_attributes(black_box(&mut chunk), black_box(&config));
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
}

fn wall_time_benches(c: &mut Criterion) {
    bench_obfuscate_span(c);
}

fn allocated_bytes_benches(c: &mut Criterion<AllocatedBytesMeasurement<System>>) {
    bench_obfuscate_span(c);
}

criterion_group!(benches, wall_time_benches);

/// Wired into the bench binary's `main` in `benches/trace_obfuscation.rs`.
pub fn alloc_benches() {
    allocated_bytes_benches(&mut memory_allocated_criterion(&GLOBAL));
}
