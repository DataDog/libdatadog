// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use libdd_data_pipeline::trace_buffer::{Export, TraceBuffer, TraceBufferConfig, TraceChunk};
use libdd_data_pipeline::trace_exporter::{
    agent_response::AgentResponse, error::TraceExporterError, TraceExporter,
};
use libdd_shared_runtime::SharedRuntime;

type Span = [u8; 100];

// Number of chunks each sender thread sends per benchmark iteration.
const CHUNKS_PER_SENDER: usize = 90_000;

// Simulates async IO by sleeping 2ms per export batch.
#[derive(Debug)]
struct SleepExport;

impl Export<Span> for SleepExport {
    fn export_trace_chunks<'a: 'c, 'b: 'c, 'c>(
        &'a mut self,
        _trace_chunks: Vec<TraceChunk<Span>>,
        _trace_exporter: &'b TraceExporter,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send + 'c,
        >,
    > {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(2)).await;
            Ok(AgentResponse::Unchanged)
        })
    }
}

fn setup_buffer() -> (Arc<SharedRuntime>, Arc<TraceBuffer<Span>>) {
    let rt = Arc::new(SharedRuntime::new().expect("SharedRuntime::new"));
    let mut builder = TraceExporter::builder();
    builder.set_shared_runtime(rt.clone());
    let cfg = TraceBufferConfig::new()
        .max_buffered_spans(400_000)
        .span_flush_threshold(50_000)
        .max_flush_interval(Duration::from_secs(2));
    let (buf, worker) = TraceBuffer::new(
        cfg,
        Box::new(|_| {}),
        Box::new(SleepExport),
        builder.build().expect("TraceExporter::build"),
    );
    rt.spawn_worker(worker).expect("spawn_worker");
    (rt, Arc::new(buf))
}

fn bench_trace_buffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("trace_buffer");

    // (label, inter-send delay)
    let workloads: &[(&str, Option<Duration>)] = &[
        ("no_delay", None),
        ("1us_delay", Some(Duration::from_micros(1))),
        ("10us_delay", Some(Duration::from_micros(100))),
    ];

    for &(delay_label, delay) in workloads {
        for num_senders in [1_usize, 2, 4, 8] {
            let (rt, sender) = setup_buffer();

            group.throughput(Throughput::Elements(
                (num_senders * CHUNKS_PER_SENDER) as u64,
            ));

            group.bench_function(
                BenchmarkId::new(format!("{}_senders", num_senders), delay_label),
                |b| {
                    b.iter(|| {
                        std::thread::scope(|s| {
                            for _ in 0..num_senders {
                                let sender = sender.clone();
                                s.spawn(move || {
                                    for _ in 0..CHUNKS_PER_SENDER {
                                        // BatchFull errors are expected under high load.
                                        let _ = sender.send_chunk(vec![[0u8; 100]]);
                                        if let Some(d) = delay {
                                            std::thread::sleep(d);
                                        }
                                    }
                                });
                            }
                        });
                    });
                },
            );

            rt.shutdown(None).expect("runtime shutdown");
        }
    }

    group.finish();
}

criterion_group!(benches, bench_trace_buffer);
criterion_main!(benches);
