// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::hint::spin_loop;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};
use libdd_data_pipeline::trace_buffer::{
    BufferSize, Export, TraceBuffer, TraceBufferConfig, TraceChunk,
};
use libdd_data_pipeline::trace_exporter::{
    agent_response::AgentResponse, error::TraceExporterError,
};
use libdd_shared_runtime::{ForkSafeRuntime, SharedRuntime};
use libdd_tinybytes::BytesString;
use libdd_trace_utils::span::v04::SpanBytes;
use libdd_trace_utils::span::vec_map::VecMap;

// Number of chunks each sender thread sends per benchmark iteration.
const CHUNKS_PER_SENDER: usize = 900;

struct CompletionGuard<'a>(&'a AtomicUsize);

impl Drop for CompletionGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Release);
    }
}

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

#[derive(Debug)]
struct NoopExport;

impl Export<SpanBytes> for NoopExport {
    fn export_trace_chunks(
        &mut self,
        _trace_chunks: Vec<TraceChunk<SpanBytes>>,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send + '_,
        >,
    > {
        Box::pin(async { Ok(AgentResponse::Unchanged) })
    }
}

fn setup_buffer(max_buffered_bytes: usize) -> (Arc<ForkSafeRuntime>, Arc<TraceBuffer<SpanBytes>>) {
    let rt = Arc::new(ForkSafeRuntime::new().expect("ForkSafeRuntime::new"));
    let cfg = TraceBufferConfig::new()
        .max_buffered_bytes(max_buffered_bytes)
        .flush_threshold_bytes(max_buffered_bytes)
        .max_flush_interval(Duration::from_secs(2));
    let (buf, worker) = TraceBuffer::new(cfg, Box::new(|_| {}), Box::new(NoopExport));
    let _ = rt.spawn_worker(worker, true).expect("spawn_worker");
    (rt, Arc::new(buf))
}

fn bench_trace_buffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("trace_buffer_enqueue");
    group.sampling_mode(SamplingMode::Flat);

    // (label, inter-send delay)
    let workloads: &[(&str, Option<Duration>)] = &[
        ("no_delay", None),
        ("1us_delay", Some(Duration::from_micros(1))),
        ("10us_delay", Some(Duration::from_micros(10))),
    ];

    for &(delay_label, delay) in workloads {
        for num_senders in [1_usize, 2, 4, 8] {
            let max_buffered_bytes = make_span().byte_size() * num_senders * CHUNKS_PER_SENDER;
            let (rt, sender) = setup_buffer(max_buffered_bytes);

            group.throughput(Throughput::Elements(
                (num_senders * CHUNKS_PER_SENDER) as u64,
            ));

            group.bench_function(
                BenchmarkId::new(format!("{}_senders", num_senders), delay_label),
                |b| {
                    b.iter_custom(|iterations| {
                        let mut elapsed = Duration::ZERO;

                        for _ in 0..iterations {
                            let input = Vec::from_iter(
                                (0..num_senders)
                                    .map(|_| (0..CHUNKS_PER_SENDER).map(|_| vec![make_span()]))
                                    .map(Vec::from_iter),
                            );
                            let ready = AtomicUsize::new(0);
                            let start = AtomicBool::new(false);
                            let done = AtomicUsize::new(0);

                            std::thread::scope(|s| {
                                for sender_spans in input {
                                    let sender = sender.clone();
                                    let ready = &ready;
                                    let start = &start;
                                    let done = &done;
                                    s.spawn(move || {
                                        let _completion = CompletionGuard(done);
                                        ready.fetch_add(1, Ordering::Release);
                                        while !start.load(Ordering::Acquire) {
                                            spin_loop();
                                        }

                                        for spans in sender_spans {
                                            sender
                                                .send_chunk(spans)
                                                .expect("iteration fits in the configured buffer");
                                            if let Some(delay) = delay {
                                                std::thread::sleep(delay);
                                            }
                                        }
                                    });
                                }

                                while ready.load(Ordering::Acquire) != num_senders {
                                    spin_loop();
                                }
                                // Thread scheduling otherwise dominates the short no-delay case.
                                let start_time = Instant::now();
                                start.store(true, Ordering::Release);
                                while done.load(Ordering::Acquire) != num_senders {
                                    spin_loop();
                                }
                                elapsed += start_time.elapsed();
                            });

                            sender
                                .flush_and_wait(Some(Duration::from_secs(1)))
                                .expect("flush after benchmark iteration");
                        }

                        elapsed
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
