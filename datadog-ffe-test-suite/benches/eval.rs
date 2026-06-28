// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, Bencher, Criterion, Throughput,
};
use datadog_ffe::telemetry::flagevaluation::{
    encode_flag_evaluation_payloads, AllocationKey, ContextDD, FfeFlagEvaluationBatch,
    FfeFlagEvaluationEvent, FlagEvalEventContext, FlagEvaluationEvpCoalescer, FlagKey, VariantKey,
    EVP_PAYLOAD_SIZE_LIMIT,
};
use datadog_ffe::telemetry::FfeTelemetryContext;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    sync::Arc,
};

use datadog_ffe::rules_based::{
    get_assignment, Attribute, Configuration, EvaluationContext, ExpectedFlagType, FlagType, Str,
    UniversalFlagConfig,
};

const UFC_CONFIG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/ffe-system-test-data/ufc-config.json"
);
const EVALUATION_CASES_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/ffe-system-test-data/evaluation-cases"
);

fn load_configuration_bytes() -> Vec<u8> {
    fs::read(UFC_CONFIG_PATH).expect("Failed to read ufc-config.json")
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestCase {
    flag: String,
    variation_type: FlagType,
    default_value: serde_json::Value,
    targeting_key: Option<Str>,
    attributes: HashMap<Str, Attribute>,
    result: TestResult,
}

#[derive(Debug, Serialize, Deserialize)]
struct TestResult {
    value: serde_json::Value,
}

fn load_test_cases() -> Vec<TestCase> {
    let mut test_cases = Vec::new();

    if let Ok(entries) = fs::read_dir(EVALUATION_CASES_DIR) {
        for entry in entries.flatten() {
            if let Some(path_str) = entry.path().to_str() {
                if path_str.ends_with(".json") {
                    if let Ok(content) = fs::read_to_string(entry.path()) {
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
            let _assignment = get_assignment(
                Some(&configuration),
                flag_key,
                context,
                ExpectedFlagType::Any,
                now,
            );

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
        Some("french_user".into()),
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
            black_box("kill-switch"),
            black_box(&context),
            ExpectedFlagType::Any,
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

#[derive(Clone, Copy)]
struct FlagEvalBenchProfile {
    name: &'static str,
    num_flags: usize,
    num_users: usize,
    num_fields: usize,
}

const FLAG_EVAL_BENCH_PROFILES: [FlagEvalBenchProfile; 3] = [
    FlagEvalBenchProfile {
        name: "typical/100flags_50users_10fields",
        num_flags: 100,
        num_users: 50,
        num_fields: 10,
    },
    FlagEvalBenchProfile {
        name: "stress/10flags_1000users_250fields",
        num_flags: 10,
        num_users: 1_000,
        num_fields: 250,
    },
    FlagEvalBenchProfile {
        name: "scale/2500flags_500users_20fields",
        num_flags: 2_500,
        num_users: 500,
        num_fields: 20,
    },
];

fn flag_eval_context() -> FfeTelemetryContext {
    FfeTelemetryContext {
        service: "bench-service".to_string(),
        env: "ci".to_string(),
        version: "1.0.0".to_string(),
    }
}

fn flag_eval_attrs(num_fields: usize) -> String {
    let attrs = (0..num_fields)
        .map(|i| {
            (
                format!("field{i}"),
                serde_json::Value::String("value".to_string()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    serde_json::to_string(&attrs).expect("benchmark attrs must encode")
}

fn flag_eval_events(profile: FlagEvalBenchProfile) -> Vec<FfeFlagEvaluationEvent> {
    let attrs = flag_eval_attrs(profile.num_fields);
    let cycle_count = profile.num_flags.max(profile.num_users);
    (0..cycle_count)
        .map(|i| FfeFlagEvaluationEvent {
            timestamp: 1_760_000_000_000,
            flag: FlagKey {
                key: format!("bench-flag-{}", i % profile.num_flags),
            },
            first_evaluation: 1_760_000_000_000 + i as i64,
            last_evaluation: 1_760_000_000_000 + i as i64,
            evaluation_count: 1,
            variant: Some(VariantKey {
                key: format!("variant-{}", i % 4),
            }),
            allocation: Some(AllocationKey {
                key: format!("alloc-{}", i % profile.num_flags),
            }),
            targeting_rule: None,
            targeting_key: Some(format!("bench-user-{}", i % profile.num_users)),
            context: Some(FlagEvalEventContext {
                evaluation: Some(attrs.clone()),
                dd: Some(ContextDD {
                    service: "bench-service".to_string(),
                }),
            }),
            error: None,
            runtime_default_used: false,
        })
        .collect()
}

fn flag_eval_batch(profile: FlagEvalBenchProfile) -> FfeFlagEvaluationBatch {
    FfeFlagEvaluationBatch {
        context: flag_eval_context(),
        flag_evaluations: flag_eval_events(profile),
    }
}

fn bench_flagevaluation_evp_coalescer(c: &mut Criterion) {
    let mut group = c.benchmark_group("flagevaluation_evp/coalescer");
    for profile in FLAG_EVAL_BENCH_PROFILES {
        let batch = flag_eval_batch(profile);
        group.throughput(Throughput::Elements(batch.flag_evaluations.len() as u64));
        group.bench_function(profile.name, |b| {
            b.iter_batched(
                || batch.clone(),
                |batch| {
                    let coalescer = FlagEvaluationEvpCoalescer::<String>::default();
                    coalescer.enqueue("agent".to_string(), black_box(batch));
                    let batches = coalescer.take_batches();
                    coalescer.finish_flush_cycle();
                    black_box(batches);
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn bench_flagevaluation_evp_payloads(c: &mut Criterion) {
    let mut group = c.benchmark_group("flagevaluation_evp/payloads");
    for profile in FLAG_EVAL_BENCH_PROFILES {
        let batch = flag_eval_batch(profile);
        group.throughput(Throughput::Elements(batch.flag_evaluations.len() as u64));
        group.bench_function(profile.name, |b| {
            b.iter_batched(
                || batch.clone(),
                |batch| {
                    let payloads =
                        encode_flag_evaluation_payloads(black_box(batch), EVP_PAYLOAD_SIZE_LIMIT)
                            .expect("benchmark payload should encode");
                    black_box(payloads);
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_sdk_test_data,
    bench_single_flag,
    bench_flagevaluation_evp_coalescer,
    bench_flagevaluation_evp_payloads
);
criterion_main!(benches);
