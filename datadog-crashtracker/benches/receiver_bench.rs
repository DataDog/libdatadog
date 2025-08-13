// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, BenchmarkId, Criterion, Throughput};
#[cfg(feature = "benchmarking")]
use datadog_crashtracker::receiver_entry_point_bench;
use datadog_crashtracker::{CrashtrackerConfiguration, CrashtrackerReceiverConfig};
use std::time::Duration;
use tokio::io::BufReader;

fn create_mock_crash_report() -> String {
    r#"DD_CRASHTRACK_BEGIN_CONFIG
{"resolve_frames": "Disabled","demangle_names": true,"path_replacements": [], "additional_files": []}
DD_CRASHTRACK_END_CONFIG
DD_CRASHTRACK_BEGIN_PROC_INFO
{"pid": 12345, "signal": 6}
DD_CRASHTRACK_END_PROC_INFO
DD_CRASHTRACK_BEGIN_SIG_INFO
{"sig": 6, "errno": 0, "addr": "0x0"}
DD_CRASHTRACK_END_SIG_INFO
DD_CRASHTRACK_BEGIN_METADATA
["log1", "log2", "log3"]
DD_CRASHTRACK_END_METADATA
DD_CRASHTRACK_BEGIN_STACKTRACE
[{"ip": "0x7f8b8c0d1234", "sp": "0x7f8b8c0d5678", "symbol_address": "0x7f8b8c0d0000", "module_base_address": "0x7f8b8c0d0000", "module": "test_module"}]
DD_CRASHTRACK_END_STACKTRACE
DD_CRASHTRACK_DONE"#.to_string()
}

fn create_mock_simple_crash_report() -> String {
    r#"DD_CRASHTRACK_BEGIN_STACKTRACE
{ "ip": "0x42", "module_address": "0x21", "sp": "0x11", "symbol_address": "0x73" }
DD_CRASHTRACK_END_STACKTRACE
DD_CRASHTRACK_DONE"#.to_string()
}

fn create_empty_report() -> String {
    "DD_CRASHTRACK_BEGIN_CONFIG\nDD_CRASHTRACK_FINISHED\n".to_string()
}

async fn bench_receiver_entry_point_valid(data: &str) {
    let cursor = std::io::Cursor::new(data.as_bytes());
    let reader = BufReader::new(cursor);
    let timeout = Duration::from_millis(5000);
    
    let _ = receiver_entry_point_bench(timeout, reader).await;
}

async fn bench_receiver_entry_point_invalid(data: &str) {
    let cursor = std::io::Cursor::new(data.as_bytes());
    let reader = BufReader::new(cursor);
    let timeout = Duration::from_millis(5000);
    
    let _ = receiver_entry_point_bench(timeout, reader).await;
}

fn receiver_entry_point_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("receiver_entry_point");
    
    let valid_report = create_mock_crash_report();
    group.throughput(Throughput::Bytes(valid_report.len() as u64));
    group.bench_with_input(
        BenchmarkId::new("valid_report", valid_report.len()),
        &valid_report,
        |b, data| {
            b.iter(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(bench_receiver_entry_point_valid(black_box(data)))
            });
        },
    );

    group.finish();
}

fn receiver_entry_point_memory_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("receiver_entry_point_memory");
    
    // Benchmark with varying sizes of crash reports
    for size_multiplier in [1, 10, 100].iter() {
        let mut large_report = create_mock_crash_report();
        
        // Create a larger stacktrace section
        let mut large_stacktrace = String::new();
        large_stacktrace.push_str("DD_CRASHTRACK_BEGIN_STACKTRACE\n");
        large_stacktrace.push('[');
        
        for i in 0..*size_multiplier {
            if i > 0 {
                large_stacktrace.push(',');
            }
            large_stacktrace.push_str(&format!(
                r#"{{"ip": "0x7f8b8c0d{:04x}", "sp": "0x7f8b8c0d{:04x}", "symbol_address": "0x7f8b8c0d0000", "module_base_address": "0x7f8b8c0d0000", "module": "test_module_{}"}}"#,
                i, i + 1000, i
            ));
        }
        
        large_stacktrace.push_str("]\nDD_CRASHTRACK_END_STACKTRACE\nDD_CRASHTRACK_FINISHED\n");
        
        // Replace the stacktrace section in the report
        let begin_idx = large_report.find("DD_CRASHTRACK_BEGIN_STACKTRACE").unwrap();
        let end_idx = large_report.find("DD_CRASHTRACK_FINISHED").unwrap() + "DD_CRASHTRACK_FINISHED".len();
        large_report.replace_range(begin_idx..end_idx, &large_stacktrace);
        
        group.throughput(Throughput::Bytes(large_report.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("large_report", size_multiplier),
            &large_report,
            |b, data| {
                b.iter(|| {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    rt.block_on(async {
                        let cursor = std::io::Cursor::new(data.as_bytes());
                        let reader = BufReader::new(cursor);
                        let timeout = Duration::from_millis(5000);
                        
                        let _ = receiver_entry_point_bench(black_box(timeout), black_box(reader)).await;
                    })
                });
            },
        );
    }
    
    group.finish();
}

criterion_group!(
    benches,
    receiver_entry_point_benchmarks,
    // receiver_entry_point_memory_benchmarks
);

criterion::criterion_main!(benches);
