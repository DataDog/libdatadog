// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use std::{
    collections::HashMap,
    time::{self, Duration, SystemTime},
};

use criterion::{criterion_group, Criterion};
use libdd_trace_stats::span_concentrator::SpanConcentrator;
use libdd_trace_utils::span::SpanBytes;

fn get_bucket_start(now: SystemTime, n: u64) -> i64 {
    let start = now.duration_since(time::UNIX_EPOCH).unwrap() + Duration::from_secs(10 * n);
    start.as_nanos() as i64
}

fn get_span(now: SystemTime, trace_id: u64, span_id: u64) -> SpanBytes {
    let mut metrics = HashMap::from([("_dd.measured".into(), 1.0)]);
    if span_id == 1 {
        metrics.insert("_dd.top_level".into(), 1.0);
    }
    let mut meta = HashMap::from([("db_name".into(), "postgres".into())]);
    if span_id % 3 == 0 {
        meta.insert("bucket_s3".into(), "aws_bucket".into());
    }
    SpanBytes {
        trace_id,
        span_id,
        service: "test-service".into(),
        name: "test-name".into(),
        resource: format!("test-{trace_id}").into(),
        error: (span_id % 2) as i32,
        metrics,
        meta,
        parent_id: span_id - 1,
        start: get_bucket_start(now, trace_id),
        duration: span_id as i64 % Duration::from_secs(10).as_nanos() as i64,
        ..Default::default()
    }
}

pub fn criterion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("concentrator");
    let now = SystemTime::now() - Duration::from_secs(10 * 100);
    let concentrator = SpanConcentrator::new(
        Duration::from_secs(10),
        now,
        vec![],
        vec!["db_name".into(), "bucket_s3".into()],
    );
    let mut spans = vec![];
    for trace_id in 1..100 {
        for span_id in 1..100 {
            spans.push(get_span(now, trace_id, span_id));
        }
    }
    group.bench_function("add_spans_to_concentrator", |b| {
        b.iter_batched_ref(
            || (concentrator.clone(), spans.clone()),
            |data| {
                let concentrator = &mut data.0;
                let spans = &data.1;
                for span in spans {
                    concentrator.add_span(span);
                }
            },
            criterion::BatchSize::LargeInput,
        );
    });
}
criterion_group!(benches, criterion_benchmark);
