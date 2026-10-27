// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Micro benchmarks for writing to the Data Streams Monitoring (DSM) shared memory tables.
//!
//! PHP workers record every checkpoint and Kafka offset by writing it into a table the sidecar
//! created ([`ShmDsmCheckpointTable`], [`ShmDsmBacklogTable`]), on the request's own thread. Each
//! benchmark creates its table as the sidecar does and writes through a handle opened by name, as
//! a worker does, so the check for a replaced table that a worker makes on every write is timed
//! too. Nothing drains the tables, so every write after the first for a key finds the key already
//! in the bucket, as nearly every checkpoint does in production.

use std::ffi::CString;
use std::hint::black_box;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use libdd_ipc::shm_dsm_backlog::{
    DEFAULT_DSM_BACKLOG_SLOT_COUNT, DEFAULT_DSM_BACKLOG_STRING_POOL_BYTES, DsmBacklogInput,
    ShmDsmBacklogTable,
};
use libdd_ipc::shm_dsm_checkpoint::{
    DEFAULT_DSM_CHECKPOINT_SLOT_COUNT, DEFAULT_DSM_CHECKPOINT_STRING_POOL_BYTES, DSM_BUCKET_NANOS,
    DsmCheckpointInput, ShmDsmCheckpointTable,
};

/// Edge tags of a Kafka produce, sorted as the PHP tracer passes them.
const PRODUCE_TAGS: &[&str] = &["direction:out", "topic:orders", "type:kafka"];

/// Pathway hash of the checkpoint written; writer `n` of the concurrent benchmark writes
/// `PATHWAY_HASH + n`, which puts each writer's pathway in a slot of its own.
const PATHWAY_HASH: u64 = 0x8f2a_c1d4_6b3e_9a75;
const PARENT_HASH: u64 = 0x1d6e_03b7_52f9_c48a;

/// Message size reported with each checkpoint, as in the PHP tracer's `DataStreamsBench`.
const PAYLOAD_SIZE: i64 = 512;

/// Latencies cycled through, so that writes spread over as many histogram bins as a busy
/// pathway's do, rather than all landing in one.
const LATENCIES: i64 = 64;

/// Writers in the concurrent benchmark, each standing for a PHP worker process.
const WRITERS: usize = 4;

/// Partitions the backlog benchmark records produce offsets for, as in the PHP tracer's
/// `DataStreamsBench`.
const PARTITIONS: usize = 16;

/// A segment name no other benchmark (or concurrent run) uses.
fn shm_path(prefix: &str) -> CString {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    // Short enough for macOS, which allows 31 bytes.
    CString::new(format!(
        "/ddbench-{prefix}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
    .expect("segment names have no NUL")
}

/// The sidecar's table, and a worker's handle to it.
fn checkpoint_table() -> (ShmDsmCheckpointTable, CString) {
    let path = shm_path("dsmcp");
    let owner = ShmDsmCheckpointTable::create(
        path.clone(),
        DSM_BUCKET_NANOS,
        DEFAULT_DSM_CHECKPOINT_SLOT_COUNT,
        DEFAULT_DSM_CHECKPOINT_STRING_POOL_BYTES,
    )
    .expect("create the checkpoint table");
    (owner, path)
}

fn open_checkpoint_table(path: &CString) -> ShmDsmCheckpointTable {
    ShmDsmCheckpointTable::open(path).expect("open the checkpoint table")
}

/// The produce checkpoints written for `hash`, one per latency.
fn checkpoints(hash: u64) -> Vec<DsmCheckpointInput<'static>> {
    (0..LATENCIES)
        .map(|i| DsmCheckpointInput {
            hash,
            parent_hash: PARENT_HASH,
            edge_tags: PRODUCE_TAGS,
            // 1 ms to about 0.5 s since the pathway started, and 0.1 ms to about 50 ms since
            // the previous checkpoint.
            pathway_latency_ns: 1_000_000 + i * 8_000_000,
            edge_latency_ns: 100_000 + i * 800_000,
            payload_size_bytes: PAYLOAD_SIZE,
        })
        .collect()
}

fn bench_checkpoint(c: &mut Criterion) {
    let mut group = c.benchmark_group("dsm_checkpoint_table");

    let (_owner, path) = checkpoint_table();
    let worker = open_checkpoint_table(&path);
    let inputs = checkpoints(PATHWAY_HASH);
    assert!(worker.add_checkpoint(&inputs[0]));
    let mut inputs = inputs.iter().cycle();
    group.bench_function("add_existing_pathway", |b| {
        b.iter(|| worker.add_checkpoint(black_box(inputs.next().expect("cycle never ends"))))
    });

    // Each writer has its own mapping of the segment, as each worker process does, and its own
    // pathway, so the writers only share what every write to a bucket touches. The time
    // reported is a write's as each writer sees it while all of them are writing.
    let (_owner, path) = checkpoint_table();
    let writers: Vec<_> = (0..WRITERS as u64)
        .map(|n| {
            let worker = open_checkpoint_table(&path);
            let inputs = checkpoints(PATHWAY_HASH + n);
            assert!(worker.add_checkpoint(&inputs[0]));
            Arc::new((worker, inputs))
        })
        .collect();
    group.bench_with_input(
        BenchmarkId::new("add_concurrent_writers", WRITERS),
        &writers,
        |b, writers| {
            b.iter_custom(|iters| {
                let start = Arc::new(Barrier::new(writers.len() + 1));
                let threads: Vec<_> = writers
                    .iter()
                    .map(|writer| {
                        let (writer, start) = (writer.clone(), start.clone());
                        thread::spawn(move || {
                            let (worker, inputs) = &*writer;
                            let mut inputs = inputs.iter().cycle();
                            start.wait();
                            for _ in 0..iters {
                                worker.add_checkpoint(black_box(
                                    inputs.next().expect("cycle never ends"),
                                ));
                            }
                        })
                    })
                    .collect();
                start.wait();
                let started = Instant::now();
                for thread in threads {
                    thread.join().expect("writer thread");
                }
                started.elapsed()
            })
        },
    );

    group.finish();
}

fn bench_backlog(c: &mut Criterion) {
    let mut group = c.benchmark_group("dsm_backlog_table");

    let path = shm_path("dsmbl");
    let _owner = ShmDsmBacklogTable::create(
        path.clone(),
        DSM_BUCKET_NANOS,
        DEFAULT_DSM_BACKLOG_SLOT_COUNT,
        DEFAULT_DSM_BACKLOG_STRING_POOL_BYTES,
    )
    .expect("create the backlog table");
    let worker = ShmDsmBacklogTable::open(&path).expect("open the backlog table");

    // Tags as the PHP tracer passes them: `key:value` strings, parsed on every write.
    let partitions: Vec<String> = (0..PARTITIONS).map(|p| format!("partition:{p}")).collect();
    let tags: Vec<[&str; 3]> = partitions
        .iter()
        .map(|partition| ["topic:orders", "type:kafka_produce", partition])
        .collect();
    for partition_tags in &tags {
        let backlog = DsmBacklogInput::parse(partition_tags, 0).expect("known tags");
        assert!(worker.add_backlog(&backlog));
    }

    // Offsets keep rising, as a producer's do, so every write raises its partition's maximum.
    let mut partition_tags = tags.iter().cycle();
    let mut offset = 0i64;
    group.bench_function(
        BenchmarkId::new("add_produce_offset_partitions", PARTITIONS),
        |b| {
            b.iter(|| {
                offset += 1;
                let tags = partition_tags.next().expect("cycle never ends");
                if let Some(backlog) = DsmBacklogInput::parse(black_box(tags), offset) {
                    worker.add_backlog(&backlog);
                }
            })
        },
    );

    group.finish();
}

criterion_group!(benches, bench_checkpoint, bench_backlog);
criterion_main!(benches);
