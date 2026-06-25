// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use libdd_data_pipeline::trace_buffer::{Export, TraceBuffer, TraceBufferConfig, TraceChunk};
use libdd_data_pipeline::trace_exporter::{
    agent_response::AgentResponse, error::TraceExporterError,
};
use libdd_shared_runtime::{ForkSafeRuntime, SharedRuntime};
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::v04::SpanBytes;
use libdd_trace_utils::span::vec_map::VecMap;

// Number of chunks each sender thread sends per benchmark iteration.
const CHUNKS_PER_SENDER: usize = 900;

fn bs(s: &'static str) -> BytesString {
    BytesString::from_static(s)
}

fn make_span() -> SpanBytes {
    SpanBytes {
        service: bs("my-web-service"),
        name: bs("http.request"),
        resource: bs("GET /api/v1/users"),
        r#type: bs("web"),
        trace_id: 1_234_567_890_123_456_789_u128,
        span_id: 987_654_321_u64,
        parent_id: 0,
        start: 1_700_000_000_000_000_000_i64,
        duration: 5_000_000_i64,
        error: 0,
        meta: vec![
            (bs("env"), bs("prod")),
            (bs("version"), bs("1.0.0")),
            (bs("http.method"), bs("GET")),
            (bs("http.url"), bs("/api/v1/users")),
            (bs("peer.service"), bs("users-service")),
        ]
        .into(),
        metrics: vec![
            (bs("_sampling_priority_v1"), 1.0_f64),
            (bs("_dd.agent_psr"), 1.0_f64),
        ]
        .into(),
        meta_struct: VecMap::new(),
        span_links: vec![],
        span_events: vec![],
    }
}

// Simulates async IO by sleeping 2ms per export batch.
#[derive(Debug)]
struct SleepExport;

impl Export<SpanBytes> for SleepExport {
    fn export_trace_chunks(
        &mut self,
        _trace_chunks: Vec<TraceChunk<SpanBytes>>,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send + '_,
        >,
    > {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(2)).await;
            Ok(AgentResponse::Unchanged)
        })
    }
}

fn setup_buffer() -> (Arc<ForkSafeRuntime>, Arc<TraceBuffer<SpanBytes>>) {
    let rt = Arc::new(ForkSafeRuntime::new().expect("ForkSafeRuntime::new"));
    let cfg = TraceBufferConfig::new()
        .max_buffered_bytes(1_000_000)
        .flush_threshold_bytes(100_000)
        .max_flush_interval(Duration::from_secs(2));
    let (buf, worker) = TraceBuffer::new(cfg, Box::new(|_| {}), Box::new(SleepExport));
    let _ = rt.spawn_worker(worker, true).expect("spawn_worker");
    (rt, Arc::new(buf))
}

fn bench_trace_buffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("trace_buffer");

    // (label, inter-send delay)
    let workloads: &[(&str, Option<Duration>)] = &[
        ("no_delay", None),
        ("1us_delay", Some(Duration::from_micros(1))),
        ("10us_delay", Some(Duration::from_micros(10))),
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
                    b.iter_batched(
                        || {
                            Vec::from_iter(
                                (0..num_senders)
                                    .map(|_| (0..CHUNKS_PER_SENDER).map(|_| vec![make_span()]))
                                    .map(Vec::from_iter),
                            )
                        },
                        |input| {
                            std::thread::scope(|s| {
                                for sender_spans in input {
                                    let sender = sender.clone();
                                    s.spawn(move || {
                                        for s in sender_spans {
                                            // BatchFull errors are expected under high load.
                                            let _ = sender.send_chunk(s);
                                            if let Some(d) = delay {
                                                std::thread::sleep(d);
                                            }
                                        }
                                    });
                                }
                            });
                        },
                        BatchSize::PerIteration,
                    );
                },
            );

            rt.shutdown(None).expect("runtime shutdown");
        }
    }

    group.finish();
}

criterion_group!(benches, bench_trace_buffer);
criterion_main!(benches);
