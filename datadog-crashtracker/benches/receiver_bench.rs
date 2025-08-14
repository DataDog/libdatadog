// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, BenchmarkId, Criterion};
use datadog_crashtracker::benchmark::receiver_entry_point;
use std::time::Duration;
use tokio::io::BufReader;

fn create_dummy_crash_report() -> String {
    r#"DD_CRASHTRACK_BEGIN_STACKTRACE
{ "ip": "0x42", "module_address": "0x21", "sp": "0x11", "symbol_address": "0x73" }
DD_CRASHTRACK_END_STACKTRACE
DD_CRASHTRACK_DONE"#
        .to_string()
}

async fn bench_receiver_entry_point_from_str(data: &str) {
    let cursor = std::io::Cursor::new(data.as_bytes());
    let reader = BufReader::new(cursor);
    let timeout = Duration::from_millis(5000);

    let _ = receiver_entry_point(timeout, reader).await;
}

pub fn receiver_entry_point_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("receiver_entry_point");

    let report = create_dummy_crash_report();
    group.bench_with_input(
        BenchmarkId::new("report", report.len()),
        &report,
        |b, data| {
            b.iter(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(bench_receiver_entry_point_from_str(black_box(data)))
            });
        },
    );
}

criterion_group!(benches, receiver_entry_point_benchmarks);
