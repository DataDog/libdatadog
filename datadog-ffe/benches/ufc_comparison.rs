#![allow(unused_imports)]
use criterion::{black_box, criterion_group, criterion_main, Bencher, Criterion};
use datadog_ffe::rules_based::{EvaluationContext, UniversalFlagConfig};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, sync::Arc, time::SystemTime};

use datadog_ffe::rules_based::{get_assignment, Attribute, Configuration, Str};

fn load_configuration_bytes() -> Vec<u8> {
    fs::read("tests/data/flags-v1.json").expect("Failed to read flags-v1.json")
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestCase {
    flag: String,
    variation_type: String,
    default_value: serde_json::Value,
    targeting_key: Str,
    attributes: HashMap<Str, Attribute>,
    result: TestResult,
}

#[derive(Debug, Serialize, Deserialize)]
struct TestResult {
    value: serde_json::Value,
}

fn load_test_cases() -> Vec<TestCase> {
    let mut test_cases = Vec::new();

    if let Ok(entries) = fs::read_dir("tests/data/tests") {
        for entry in entries.flatten() {
            if let Some(path_str) = entry.path().to_str() {
                if path_str.ends_with(".json") {
                    if let Ok(content) = fs::read_to_string(&entry.path()) {
                        if let Ok(cases) = serde_json::from_str::<Vec<TestCase>>(&content) {
                            test_cases.extend(cases);
                        }
                    }
                }
            }
        }
    }

    test_cases
}

fn bench_sdk_test_data_rules_based(b: &mut Bencher) {
    let json_bytes = load_configuration_bytes();
    let test_cases = load_test_cases();
    let now = chrono::Utc::now();

    // Prepare configuration outside the benchmark
    let config =
        UniversalFlagConfig::from_json(json_bytes).expect("Failed to parse UFC v1 configuration");
    let configuration = Configuration::from_server_response(config);

    let test_cases = test_cases
        .into_iter()
        .map(|test_case| {
            (
                test_case.flag,
                EvaluationContext::new(test_case.targeting_key, Arc::new(test_case.attributes)),
            )
        })
        .collect::<Vec<_>>();

    b.iter(|| {
        for (flag_key, context) in black_box(&test_cases) {
            // Evaluate assignment
            let _assignment = get_assignment(Some(&configuration), flag_key, context, None, now);

            let _ = black_box(_assignment);
        }
    })
}

fn bench_single_flag_rules_based(b: &mut Bencher) {
    let json_bytes = load_configuration_bytes();
    let now = chrono::Utc::now();

    // Prepare configuration outside the benchmark
    let config =
        UniversalFlagConfig::from_json(json_bytes).expect("Failed to parse UFC v1 configuration");
    let configuration = Configuration::from_server_response(config);

    let context = EvaluationContext::new(
        "french_user".into(),
        Arc::new(
            [
                ("country".into(), "France".into()),
                ("age".into(), 32.0.into()),
            ]
            .into_iter()
            .collect(),
        ),
    );

    b.iter(|| {
        let _assignment = get_assignment(
            black_box(Some(&configuration)),
            black_box(&"kill-switch"),
            black_box(&context),
            None,
            now,
        );
        let _ = black_box(_assignment);
    })
}

fn bench_sdk_test_data(c: &mut Criterion) {
    let mut group = c.benchmark_group("sdk_test_data");
    group.bench_function("rules-based", bench_sdk_test_data_rules_based);
    group.finish();
}

fn bench_single_flag(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_flag_killswitch");
    group.bench_function("rules-based", bench_single_flag_rules_based);
    group.finish();
}

criterion_group!(benches, bench_sdk_test_data, bench_single_flag);
criterion_main!(benches);
