// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![allow(clippy::unwrap_used)]

use core::hint::black_box;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use libdd_api_security::{extract_schema, extract_schema_json};
use serde_json::{Map, Value, json};

fn record(size: usize) -> Value {
    Value::Object(
        (0..size)
            .map(|i| (format!("field-{i}"), json!("some value")))
            .collect(),
    )
}

fn rows(size: usize) -> Value {
    Value::Array(
        (0..size)
            .map(|i| json!({"sku": "item", "count": i, "active": true}))
            .collect(),
    )
}

fn deep(depth: usize) -> Value {
    (0..depth).fold(json!(true), |value, _| json!({"child": value}))
}

fn realistic(size: usize) -> Value {
    json!({
        "user": {"id": 42, "email": "test@example.com", "active": true},
        "items": rows(size),
        "shipping": {"city": "Paris", "country": "FR"}
    })
}

fn fixtures() -> Vec<(String, Value)> {
    let mut inputs = Vec::new();
    for size in [10, 255, 1000] {
        inputs.push((format!("record-{size}"), record(size)));
    }
    for size in [10, 1000] {
        inputs.push((format!("array-{size}"), rows(size)));
    }
    for depth in [18, 100] {
        inputs.push((format!("depth-{depth}"), deep(depth)));
    }
    for size in [20, 500] {
        inputs.push((format!("realistic-{size}"), realistic(size)));
    }
    let heterogeneous = (0..10)
        .map(|i| {
            let fields: Map<_, _> =
                core::iter::once((format!("field-{i}"), json!([i, "value", true]))).collect();
            Value::Object(fields)
        })
        .collect();
    inputs.push(("heterogeneous-10".into(), Value::Array(heterogeneous)));
    inputs
}

fn benchmarks(c: &mut Criterion) {
    for (name, input) in fixtures() {
        let encoded = serde_json::to_string(&input).unwrap();
        let schema = extract_schema(&input);
        c.bench_with_input(BenchmarkId::new("extract", &name), &input, |b, value| {
            b.iter(|| black_box(extract_schema(black_box(value))));
        });
        c.bench_with_input(BenchmarkId::new("parse", &name), &encoded, |b, value| {
            b.iter(|| black_box(serde_json::from_str::<Value>(black_box(value)).unwrap()));
        });
        c.bench_with_input(BenchmarkId::new("serialize", &name), &schema, |b, value| {
            b.iter(|| black_box(serde_json::to_string(black_box(value)).unwrap()));
        });
        c.bench_with_input(
            BenchmarkId::new("dom_json_api", &name),
            &encoded,
            |b, value| {
                b.iter(|| {
                    let input: Value = serde_json::from_str(black_box(value)).unwrap();
                    black_box(serde_json::to_string(&extract_schema(&input)).unwrap())
                });
            },
        );
        c.bench_with_input(BenchmarkId::new("json_api", &name), &encoded, |b, value| {
            b.iter(|| black_box(extract_schema_json(black_box(value)).unwrap()));
        });
    }
}

criterion_group!(benches, benchmarks);
criterion_main!(benches);
