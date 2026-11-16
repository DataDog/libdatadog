// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{Criterion, criterion_group, criterion_main};
use libdd_ffe::rules_based::{
    Configuration, EvaluationContext, ExpectedFlagType, UniversalFlagConfig,
};
use serde_json::json;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    hint::black_box,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct CountingAllocator;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

// This standalone benchmark counts allocation requests during one evaluation,
// excluding fixture compilation and Criterion's own bookkeeping.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(size, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn bench_dependencies(c: &mut Criterion) {
    let parent = json!({"key":"parent","enabled":true,"variationType":"BOOLEAN",
        "variations":{"on":{"key":"on","value":true}},
        "allocations":[{"key":"default","splits":[{"shards":[],"variationKey":"on"}]}]});
    for (name, repeated) in [
        ("legacy", 0),
        ("one_prerequisite", 1),
        ("memoized_16_references", 16),
    ] {
        let mut root = parent.clone();
        root["key"] = json!("root");
        if repeated > 0 {
            root["allocations"][0]["rules"] = json!([{"conditions": (0..repeated).map(|_|
                json!({"flagEvaluation":{"key":"parent"},"operator":"ONE_OF","value":["on"]})
            ).collect::<Vec<_>>()}]);
        }
        let config = Configuration::from_server_response(
            UniversalFlagConfig::from_json(
                serde_json::to_vec(
                    &json!({"createdAt":"2026-10-28T00:00:00Z","environment":{"name":"test"},
                "flags":{"root":root,"parent":parent}}),
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let context = EvaluationContext::new(Some("subject".into()), Default::default());
        let now = chrono::Utc::now();
        c.bench_function(&format!("dependent_flags/{name}"), |b| {
            b.iter(|| {
                black_box(config.eval_flag(
                    black_box("root"),
                    black_box(&context),
                    ExpectedFlagType::Boolean,
                    now,
                ))
                .unwrap()
            })
        });
    }
    for edges in [127, 255] {
        for failure in [false, true] {
            let mut flags = serde_json::Map::new();
            for n in 0..=edges {
                let key = format!("f{n}");
                let mut flag = parent.clone();
                flag["key"] = json!(key);
                if n < edges {
                    flag["allocations"][0]["rules"] = json!([{"conditions":[{
                        "flagEvaluation":{"key":format!("f{}", n + 1)},
                        "operator":"ONE_OF", "value":["on"]
                    }]}]);
                } else if failure {
                    flag["enabled"] = json!(false);
                }
                flags.insert(key, flag);
            }
            let config = Configuration::from_server_response(
                UniversalFlagConfig::from_json(
                    serde_json::to_vec(&json!({
                        "createdAt":"2026-10-28T00:00:00Z", "environment":{"name":"test"},
                        "evaluatorParams":{"maxDependencyDepth":255}, "flags":flags
                    }))
                    .unwrap(),
                )
                .unwrap(),
            );
            let context = EvaluationContext::new(Some("subject".into()), Default::default());
            let now = chrono::Utc::now();
            let evaluate = || {
                config.eval_flag_with_trace(
                    "f0",
                    &context,
                    ExpectedFlagType::Boolean,
                    now,
                    Default::default(),
                )
            };
            let warmup = evaluate();
            assert_eq!(warmup.result.is_err(), failure);
            assert_eq!(warmup.evaluations.len(), edges);
            drop(warmup);
            ALLOCATIONS.store(0, Ordering::Relaxed);
            BYTES.store(0, Ordering::Relaxed);
            COUNTING.store(true, Ordering::Relaxed);
            let trace = evaluate();
            COUNTING.store(false, Ordering::Relaxed);
            let name = format!(
                "dependent_flags/chain_{edges}_{}",
                if failure { "failure" } else { "success" }
            );
            eprintln!(
                "{name}: {} allocation requests, {} bytes requested per evaluation",
                ALLOCATIONS.load(Ordering::Relaxed),
                BYTES.load(Ordering::Relaxed)
            );
            drop(trace);
            c.bench_function(&name, |b| b.iter(|| black_box(evaluate())));
        }
    }
}
criterion_group!(benches, bench_dependencies);
criterion_main!(benches);
