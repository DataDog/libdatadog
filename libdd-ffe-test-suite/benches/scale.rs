// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Evaluates the `scale` and `scale-json` suites of ffe-system-test-data.
//!
//! Each case file is one benchmark that cycles through its cases, and each `scale-json` case is
//! also benchmarked on its own. Before it measures, the benchmark checks each case's value and
//! reason (`tests/scale_suites.rs` checks the rest). A counting allocator reports bytes and
//! allocations per evaluation on stdout, in lines that start with `ALLOC`.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use libdd_ffe::rules_based::{
    AssignmentReason, Attribute, Configuration, EvaluationContext, EvaluationError, FlagType, Str,
    UniversalFlagConfig,
};
use serde::Deserialize;

struct CountingAllocator;

static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards every call to the system allocator and only adds relaxed counters.
// `usize as u64` is lossless on every supported target.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATED_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

const ALLOC_SAMPLE_EVALUATIONS: usize = 1_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestCase {
    flag: String,
    variation_type: FlagType,
    default_value: serde_json::Value,
    targeting_key: Option<Str>,
    attributes: HashMap<Str, serde_json::Value>,
    result: TestResult,
}

#[derive(Debug, Deserialize)]
struct TestResult {
    value: serde_json::Value,
    reason: Option<String>,
}

struct Case {
    flag: String,
    flag_type: FlagType,
    subject: EvaluationContext,
}

fn suite_root(suite: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ffe-system-test-data/evaluation-suites")
        .join(suite)
}

fn reason_from_assignment(reason: AssignmentReason) -> &'static str {
    match reason {
        AssignmentReason::TargetingMatch => "TARGETING_MATCH",
        AssignmentReason::Split => "SPLIT",
        AssignmentReason::Default => "DEFAULT",
        AssignmentReason::Static => "STATIC",
    }
}

fn reason_from_error(err: &EvaluationError) -> &'static str {
    match err {
        EvaluationError::FlagDisabled => "DISABLED",
        EvaluationError::DefaultAllocationNull => "DEFAULT",
        _ => "ERROR",
    }
}

fn load_configuration(root: &Path) -> Configuration {
    let bytes = fs::read(root.join("ufc-config.json")).expect("failed to read ufc-config.json");
    Configuration::from_server_response(
        UniversalFlagConfig::from_json(bytes).expect("failed to parse ufc-config.json"),
    )
}

/// Loads a case file and checks every case's value and reason against `config`.
fn load_cases(config: &Configuration, path: &Path) -> Vec<Case> {
    let cases: Vec<TestCase> =
        serde_json::from_reader(fs::File::open(path).expect("failed to open case file"))
            .expect("failed to parse case file");
    let now = chrono::Utc::now();
    cases
        .into_iter()
        .enumerate()
        .map(|(index, case)| {
            let attributes = case
                .attributes
                .into_iter()
                .map(|(key, value)| {
                    let value = serde_json::from_value::<Attribute>(value)
                        .expect("failed to parse attribute");
                    (key, value)
                })
                .collect();
            let subject = EvaluationContext::new(case.targeting_key, Arc::new(attributes));
            let (value_ok, reason) =
                match config.eval_flag(&case.flag, &subject, case.variation_type.into(), now) {
                    Ok(assignment) => (
                        match assignment.value.as_json() {
                            Some(value) => *value == case.result.value,
                            None => assignment.value.variation_value() == case.result.value,
                        },
                        reason_from_assignment(assignment.reason),
                    ),
                    Err(err) => (
                        case.default_value == case.result.value,
                        reason_from_error(&err),
                    ),
                };
            let at = format!("{} case {index} ({})", path.display(), case.flag);
            assert!(value_ok, "unexpected value for {at}");
            if let Some(expected) = case.result.reason.as_deref() {
                assert_eq!(reason, expected, "unexpected reason for {at}");
            }
            Case {
                flag: case.flag,
                flag_type: case.variation_type,
                subject,
            }
        })
        .collect()
}

fn evaluate(config: &Configuration, case: &Case, now: chrono::DateTime<chrono::Utc>) {
    let _ = black_box(config.eval_flag(
        black_box(&case.flag),
        black_box(&case.subject),
        case.flag_type.into(),
        now,
    ));
}

/// Prints bytes and allocations per evaluation over a fixed number of evaluations.
fn report_allocations(name: &str, config: &Configuration, cases: &[&Case]) {
    let now = chrono::Utc::now();
    let bytes_before = ALLOCATED_BYTES.load(Ordering::Relaxed);
    let allocations_before = ALLOCATIONS.load(Ordering::Relaxed);
    for n in 0..ALLOC_SAMPLE_EVALUATIONS {
        evaluate(config, cases[n % cases.len()], now);
    }
    let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes_before;
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations_before;
    // Lossless for the counts this benchmark reaches; only used for display.
    let per_evaluation = |total: u64| total as f64 / ALLOC_SAMPLE_EVALUATIONS as f64;
    println!(
        "ALLOC {name} bytes/eval={:.0} allocs/eval={:.1}",
        per_evaluation(bytes),
        per_evaluation(allocations)
    );
}

fn bench_suite(c: &mut Criterion, suite: &str, per_case: bool) {
    let root = suite_root(suite);
    let config = load_configuration(&root);
    let mut paths: Vec<_> = fs::read_dir(root.join("cases"))
        .expect("failed to list cases")
        .map(|entry| entry.expect("failed to read case entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    paths.sort();

    let mut group = c.benchmark_group(suite);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    for path in paths {
        let file = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("case file name")
            .to_string();
        let cases = load_cases(&config, &path);
        let all: Vec<&Case> = cases.iter().collect();
        report_allocations(&format!("{suite}/{file}"), &config, &all);
        group.bench_function(&file, |b| {
            let now = chrono::Utc::now();
            let mut n = 0usize;
            b.iter(|| {
                evaluate(&config, all[n % all.len()], now);
                n += 1;
            })
        });
        if per_case {
            for (index, case) in cases.iter().enumerate() {
                let name = format!("{file}/{index}-{}", case.flag);
                report_allocations(&format!("{suite}/{name}"), &config, &[case]);
                group.bench_function(&name, |b| {
                    let now = chrono::Utc::now();
                    b.iter(|| evaluate(&config, case, now))
                });
            }
        }
    }
    group.finish();
}

fn bench_scale(c: &mut Criterion) {
    bench_suite(c, "scale", false);
}

fn bench_scale_json(c: &mut Criterion) {
    bench_suite(c, "scale-json", true);
}

criterion_group!(benches, bench_scale, bench_scale_json);
criterion_main!(benches);
