// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Benchmarks for the change_buffer module.

use criterion::{criterion_group, BatchSize, Criterion};
use libdd_trace_utils::change_buffer::{ChangeBuffer, ChangeBufferState};
use libdd_trace_utils::span::SliceData;

// -- Buffer builder helper --

trait ToLeBytes {
    fn extend_le_bytes(&self, buf: &mut Vec<u8>);
}

macro_rules! impl_to_le_bytes {
    ($($ty:ty),*) => {
        $(impl ToLeBytes for $ty {
            fn extend_le_bytes(&self, buf: &mut Vec<u8>) {
                buf.extend_from_slice(&self.to_le_bytes());
            }
        })*
    };
}

impl_to_le_bytes!(u32, u64, u128, i32, i64, f64);

struct BufBuilder {
    data: Vec<u8>,
    op_count: u64,
}

impl BufBuilder {
    fn with_capacity(cap: usize) -> Self {
        let mut data = Vec::with_capacity(cap + 8);
        data.extend_from_slice(&[0u8; 8]);
        Self { data, op_count: 0 }
    }

    fn push<T: ToLeBytes>(&mut self, val: T) {
        val.extend_le_bytes(&mut self.data);
    }

    fn push_op_header(&mut self, opcode: u64, slot: u32) {
        self.push(opcode);
        self.push(slot);
        self.op_count += 1;
    }

    fn push_create(&mut self, slot: u32, span_id: u64, trace_id: u128, parent_id: u64) {
        self.push_op_header(0, slot);
        self.push(span_id);
        self.push(trace_id);
        self.push(parent_id);
    }

    fn push_set_meta(&mut self, slot: u32, key_idx: u32, val_idx: u32) {
        self.push_op_header(1, slot);
        self.push(key_idx);
        self.push(val_idx);
    }

    fn push_set_metric(&mut self, slot: u32, key_idx: u32, val: f64) {
        self.push_op_header(2, slot);
        self.push(key_idx);
        self.push(val);
    }

    fn push_set_service(&mut self, slot: u32, val_idx: u32) {
        self.push_op_header(3, slot);
        self.push(val_idx);
    }

    fn push_set_resource(&mut self, slot: u32, val_idx: u32) {
        self.push_op_header(4, slot);
        self.push(val_idx);
    }

    fn push_set_name(&mut self, slot: u32, val_idx: u32) {
        self.push_op_header(9, slot);
        self.push(val_idx);
    }

    fn push_set_duration(&mut self, slot: u32, val: i64) {
        self.push_op_header(7, slot);
        self.push(val);
    }

    fn push_set_start(&mut self, slot: u32, val: i64) {
        self.push_op_header(6, slot);
        self.push(val);
    }

    fn push_set_trace_meta(&mut self, slot: u32, key_idx: u32, val_idx: u32) {
        self.push_op_header(10, slot);
        self.push(key_idx);
        self.push(val_idx);
    }

    fn finalize(&mut self) -> Vec<u8> {
        self.data[0..8].copy_from_slice(&self.op_count.to_le_bytes());
        self.data.clone()
    }
}

fn make_change_buffer(data: &mut Vec<u8>) -> ChangeBuffer {
    unsafe { ChangeBuffer::from_raw_parts(data.as_mut_ptr(), data.len()) }
}

fn make_state(data: &mut Vec<u8>) -> ChangeBufferState<SliceData<'static>> {
    let buf = make_change_buffer(data);
    ChangeBufferState::new(buf, "my-service", "javascript", 1234)
}

fn setup_string_table(state: &mut ChangeBufferState<SliceData<'static>>) {
    let strings: &[(u32, &str)] = &[
        (0, "http.request"),
        (1, "web"),
        (2, "my-service"),
        (3, "GET /api/users"),
        (4, "http.method"),
        (5, "GET"),
        (6, "http.url"),
        (7, "https://example.com/api/users"),
        (8, "http.status_code"),
        (9, "200"),
        (10, "component"),
        (11, "express"),
        (12, "span.kind"),
        (13, "server"),
        (14, "env"),
        (15, "production"),
    ];
    for &(k, v) in strings {
        state.string_table_insert_one(k, v);
    }
}

// -- Benchmarks --

/// Realistic single-span lifecycle: Create + set all common fields (single trace)
fn bench_flush_realistic_single_span(c: &mut Criterion) {
    c.bench_function("change_buffer/flush_realistic_single_span", |b| {
        b.iter_batched(
            || {
                let mut builder = BufBuilder::with_capacity(512);
                let slot: u32 = 0;
                let sid: u64 = 1;
                builder.push_create(slot, sid, 100, 0);
                builder.push_set_name(slot, 0);
                builder.push_set_service(slot, 2);
                builder.push_set_resource(slot, 3);
                builder.push_set_meta(slot, 4, 5);
                builder.push_set_meta(slot, 6, 7);
                builder.push_set_meta(slot, 8, 9);
                builder.push_set_meta(slot, 10, 11);
                builder.push_set_meta(slot, 12, 13);
                builder.push_set_metric(slot, 8, 200.0);
                builder.push_set_start(slot, 1_700_000_000_000_000_000);
                builder.push_set_duration(slot, 5_000_000);
                builder.push_set_trace_meta(slot, 14, 15);
                let mut data = builder.finalize();
                let mut state = make_state(&mut data);
                setup_string_table(&mut state);
                let op_count = builder.op_count;
                (data, state, op_count)
            },
            |(mut data, mut state, op_count)| {
                data[0..8].copy_from_slice(&op_count.to_le_bytes());
                state.flush_change_buffer().unwrap();
            },
            BatchSize::SmallInput,
        );
    });
}

/// 10 spans all on the same trace (common case)
fn bench_flush_10_spans_one_trace(c: &mut Criterion) {
    c.bench_function("change_buffer/flush_10_spans_one_trace", |b| {
        b.iter_batched(
            || {
                let mut builder = BufBuilder::with_capacity(4096);
                let trace_id: u128 = 12345;
                for i in 0..10u32 {
                    let slot = i;
                    let sid = i as u64 + 1;
                    let parent = if i == 0 { 0 } else { 1 };
                    builder.push_create(slot, sid, trace_id, parent);
                    builder.push_set_name(slot, 0);
                    builder.push_set_service(slot, 2);
                    builder.push_set_resource(slot, 3);
                    builder.push_set_meta(slot, 4, 5);
                    builder.push_set_meta(slot, 6, 7);
                    builder.push_set_meta(slot, 8, 9);
                    builder.push_set_meta(slot, 10, 11);
                    builder.push_set_meta(slot, 12, 13);
                    builder.push_set_start(slot, 1_700_000_000_000_000_000 + (i as i64) * 1_000_000);
                    builder.push_set_duration(slot, 5_000_000);
                }
                let mut data = builder.finalize();
                let mut state = make_state(&mut data);
                setup_string_table(&mut state);
                let op_count = builder.op_count;
                (data, state, op_count)
            },
            |(mut data, mut state, op_count)| {
                data[0..8].copy_from_slice(&op_count.to_le_bytes());
                state.flush_change_buffer().unwrap();
            },
            BatchSize::SmallInput,
        );
    });
}

/// 10 spans across 10 different traces (stress test for traces map)
fn bench_flush_10_spans_10_traces(c: &mut Criterion) {
    c.bench_function("change_buffer/flush_10_spans_10_traces", |b| {
        b.iter_batched(
            || {
                let mut builder = BufBuilder::with_capacity(4096);
                for i in 0..10u32 {
                    let slot = i;
                    let sid = i as u64 + 1;
                    let trace_id = (i as u128 + 1) * 1000;
                    builder.push_create(slot, sid, trace_id, 0);
                    builder.push_set_name(slot, 0);
                    builder.push_set_service(slot, 2);
                    builder.push_set_resource(slot, 3);
                    builder.push_set_meta(slot, 4, 5);
                    builder.push_set_meta(slot, 6, 7);
                    builder.push_set_meta(slot, 8, 9);
                    builder.push_set_meta(slot, 10, 11);
                    builder.push_set_meta(slot, 12, 13);
                    builder.push_set_start(slot, 1_700_000_000_000_000_000 + (i as i64) * 1_000_000);
                    builder.push_set_duration(slot, 5_000_000);
                }
                let mut data = builder.finalize();
                let mut state = make_state(&mut data);
                setup_string_table(&mut state);
                let op_count = builder.op_count;
                (data, state, op_count)
            },
            |(mut data, mut state, op_count)| {
                data[0..8].copy_from_slice(&op_count.to_le_bytes());
                state.flush_change_buffer().unwrap();
            },
            BatchSize::SmallInput,
        );
    });
}

/// flush_chunk: create 10 spans via buffer, flush buffer, then flush_chunk
fn bench_flush_chunk_10_spans(c: &mut Criterion) {
    c.bench_function("change_buffer/flush_chunk_10_spans", |b| {
        b.iter_batched(
            || {
                // Build a change buffer that creates 10 spans with tags
                let mut builder = BufBuilder::with_capacity(4096);
                let trace_id: u128 = 100;
                for i in 0..10u32 {
                    let slot = i;
                    let sid = i as u64 + 1;
                    let parent = if i == 0 { 0 } else { 1 };
                    builder.push_create(slot, sid, trace_id, parent);
                    builder.push_set_name(slot, 0);
                    builder.push_set_service(slot, 2);
                    builder.push_set_resource(slot, 3);
                    builder.push_set_meta(slot, 4, 5); // http.method: GET
                    builder.push_set_meta(slot, 6, 7); // http.url: ...
                    builder.push_set_meta(slot, 10, 11); // component: express
                    builder.push_set_meta(slot, 12, 13); // span.kind: server
                    builder.push_set_start(slot, 1_700_000_000_000_000_000 + (i as i64) * 1_000_000);
                    builder.push_set_duration(slot, 5_000_000);
                }
                // Add trace-level tags
                builder.push_set_trace_meta(0, 14, 15); // env: production
                let mut data = builder.finalize();
                let mut state = make_state(&mut data);
                setup_string_table(&mut state);
                // Flush the change buffer to populate spans/traces
                state.flush_change_buffer().unwrap();
                let slot_indices: Vec<u32> = (0..10).collect();
                (data, state, slot_indices)
            },
            |(_data, mut state, slot_indices)| {
                state.flush_chunk(slot_indices, true).unwrap();
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    change_buffer_benches,
    bench_flush_realistic_single_span,
    bench_flush_10_spans_one_trace,
    bench_flush_10_spans_10_traces,
    bench_flush_chunk_10_spans,
);
