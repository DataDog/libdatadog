#![allow(unused_imports)]
use criterion::{Bencher, Criterion, black_box, criterion_group, criterion_main};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, sync::Arc, time::SystemTime};

use ffe_evaluation::{
    instruction_based::{
        Environment, PrecomputedConfiguration, parse_flags_configuration,
        subject::{Attribute, Subject},
    },
    rules_based::{
        Configuration, SdkMetadata, Str,
        attributes::{AttributeValue, Attributes},
        eval::{get_assignment, get_precomputed_configuration},
        ufc::UniversalFlagConfig,
    },
};

fn load_test_data() -> (Vec<u8>, Vec<u8>) {
    let json_path = if std::path::Path::new("tests/data/flags-v1.json").exists() {
        "tests/data/flags-v1.json"
    } else {
        "../tests/data/flags-v1.json"
    };

    let fb_path = if std::path::Path::new("tests/data/flags-v1.fb").exists() {
        "tests/data/flags-v1.fb"
    } else {
        "../tests/data/flags-v1.fb"
    };

    let json_bytes = fs::read(json_path).expect("Failed to read flags-v1.json");
    let fb_bytes = fs::read(fb_path).expect("Failed to read flags-v1.fb");

    (json_bytes, fb_bytes)
}

fn create_test_subject() -> (Str, Arc<Attributes>) {
    let subject_key = "test-user-123".into();

    let mut attributes = HashMap::new();
    attributes.insert("country".into(), AttributeValue::from("US"));
    attributes.insert("age".into(), AttributeValue::from(25.0));
    attributes.insert("premium".into(), AttributeValue::from(true));
    attributes.insert("device".into(), AttributeValue::from("mobile"));

    let context_attributes = Arc::new(Attributes::from_iter(attributes));

    (subject_key, context_attributes)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestCase {
    flag: String,
    variation_type: String,
    default_value: serde_json::Value,
    targeting_key: String,
    attributes: HashMap<String, serde_json::Value>,
    result: TestResult,
}

#[derive(Debug, Serialize, Deserialize)]
struct TestResult {
    value: serde_json::Value,
}

fn load_test_cases() -> Vec<TestCase> {
    let test_dir = if std::path::Path::new("src/rules_based/test_data/tests/").exists() {
        "src/rules_based/test_data/tests/"
    } else {
        "../src/rules_based/test_data/tests/"
    };

    let mut test_cases = Vec::new();

    if let Ok(entries) = fs::read_dir(test_dir) {
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

fn benchmark_ufc_v1(b: &mut Bencher) {
    let (json_bytes, _) = load_test_data();
    let (subject_key, subject_attributes) = create_test_subject();

    b.iter(|| {
        let now = chrono::Utc::now();

        // Parse configuration
        let config = UniversalFlagConfig::from_json(
            SdkMetadata {
                name: "benchmark",
                version: "0.1.0",
            },
            black_box(json_bytes.clone()),
        )
        .expect("Failed to parse UFC v1 configuration");
        let configuration = Configuration::from_server_response(config);

        // Run precompute function
        let precomputed = get_precomputed_configuration(
            Some(&configuration),
            &subject_key,
            &subject_attributes,
            now,
        );

        // Serialize result to bytes
        let serialized = serde_json::to_vec(&precomputed)
            .expect("Failed to serialize precomputed configuration");

        black_box(serialized)
    })
}

fn benchmark_instruction_based(b: &mut Bencher) {
    let (_, fb_bytes) = load_test_data();
    let (subject_key, _subject_attributes) = create_test_subject();

    b.iter(|| {
        let now = SystemTime::now();

        // Parse configuration
        let ufc = parse_flags_configuration(black_box(&fb_bytes))
            .expect("Failed to parse UFC v2 configuration");
        let _ = ufc.precompile_regexes();

        // Create subject for v2
        let subject = Subject::new(
            Attribute::String(std::borrow::Cow::Borrowed(subject_key.as_str())),
            {
                let mut attrs = HashMap::new();
                attrs.insert("country".into(), Attribute::String("US".into()));
                attrs.insert("age".into(), Attribute::Number(25.0));
                attrs.insert("premium".into(), Attribute::Bool(true));
                attrs.insert("device".into(), Attribute::String("mobile".into()));
                attrs.into_iter().collect()
            },
        );

        // Run precompute function
        let flags = ufc.evaluate_flags(&subject, now);

        // Convert to PrecomputedConfiguration structure
        let precomputed = PrecomputedConfiguration {
            created_at: now
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            environment: Environment { name: "test" },
            flags: flags.into_iter().collect(),
        };

        // Serialize result to bytes
        let serialized = serde_json::to_vec(&precomputed)
            .expect("Failed to serialize precomputed configuration");

        black_box(serialized)
    })
}

fn benchmark_individual_v1(b: &mut Bencher) {
    let (json_bytes, _) = load_test_data();
    let test_cases = load_test_cases();
    let now = chrono::Utc::now();

    // Prepare configuration outside the benchmark
    let config = UniversalFlagConfig::from_json(
        SdkMetadata {
            name: "benchmark",
            version: "0.1.0",
        },
        json_bytes,
    )
    .expect("Failed to parse UFC v1 configuration");
    let configuration = Configuration::from_server_response(config);

    let test_cases = test_cases
        .into_iter()
        .map(|test_case| {
            let flag_key: Str = test_case.flag.clone().into();
            let subject_key: Str = test_case.targeting_key.clone().into();

            // Convert test case attributes to Attributes
            let mut attributes = HashMap::new();
            for (key, value) in &test_case.attributes {
                match value {
                    serde_json::Value::Bool(b) => {
                        attributes.insert(key.clone().into(), AttributeValue::from(*b));
                    }
                    serde_json::Value::Number(n) => {
                        if let Some(f) = n.as_f64() {
                            attributes.insert(key.clone().into(), AttributeValue::from(f));
                        }
                    }
                    serde_json::Value::String(s) => {
                        attributes.insert(key.clone().into(), AttributeValue::from(s.clone()));
                    }
                    _ => {} // Skip other types for now
                }
            }
            let attributes = Arc::new(Attributes::from_iter(attributes));
            (flag_key, subject_key, attributes)
        })
        .collect::<Vec<_>>();

    b.iter(|| {
        for (flag_key, subject_key, attributes) in &test_cases {
            // Evaluate assignment
            let _assignment = get_assignment(
                Some(&configuration),
                flag_key,
                subject_key,
                attributes,
                None,
                now,
            );

            let _ = black_box(_assignment);
        }
    })
}

fn benchmark_individual_v2(b: &mut Bencher) {
    let (_, fb_bytes) = load_test_data();
    let test_cases = load_test_cases();
    let now = SystemTime::now();

    // Prepare configuration outside the benchmark
    let ufc = parse_flags_configuration(&fb_bytes).expect("Failed to parse UFC v2 configuration");

    // Precompile all regexes for better performance
    ufc.precompile_regexes()
        .expect("Failed to precompile regexes");

    let test_cases = test_cases
        .into_iter()
        .map(|test_case| {
            // Convert test case attributes to UFC v2 Subject
            let mut attrs = HashMap::new();
            for (key, value) in &test_case.attributes {
                match value {
                    serde_json::Value::Bool(b) => {
                        attrs.insert(key.clone().into(), Attribute::Bool(*b));
                    }
                    serde_json::Value::Number(n) => {
                        if let Some(f) = n.as_f64() {
                            attrs.insert(key.clone().into(), Attribute::Number(f));
                        }
                    }
                    serde_json::Value::String(s) => {
                        attrs.insert(key.clone().into(), Attribute::String(s.clone().into()));
                    }
                    _ => {} // Skip other types for now
                }
            }

            let subject = Subject::new(
                Attribute::String(test_case.targeting_key.clone().into()),
                attrs.into_iter().collect(),
            );

            (test_case.flag, subject)
        })
        .collect::<Vec<_>>();

    b.iter(|| {
        for (flag_key, subject) in &test_cases {
            // Evaluate single flag
            let _assignment = ufc.evaluate_flag(&flag_key, subject, now);

            let _ = black_box(_assignment);
        }
    })
}

fn benchmark_single_flag_v1(b: &mut Bencher) {
    let (json_bytes, _) = load_test_data();
    let now = chrono::Utc::now();

    // Prepare configuration outside the benchmark
    let config = UniversalFlagConfig::from_json(
        SdkMetadata {
            name: "benchmark",
            version: "0.1.0",
        },
        json_bytes,
    )
    .expect("Failed to parse UFC v1 configuration");
    let configuration = Configuration::from_server_response(config);

    // Use a realistic test case - kill-switch flag with country and age targeting
    let flag_key: Str = "kill-switch".into();
    let subject_key: Str = "french_user".into();

    let mut attributes = HashMap::new();
    attributes.insert("country".into(), AttributeValue::from("France"));
    attributes.insert("age".into(), AttributeValue::from(32.0));
    let attributes = Arc::new(Attributes::from_iter(attributes));

    b.iter(|| {
        let _assignment = get_assignment(
            black_box(Some(&configuration)),
            black_box(&flag_key),
            black_box(&subject_key),
            black_box(&attributes),
            None,
            now,
        );
        let _ = black_box(_assignment);
    })
}

fn benchmark_single_flag_v2(b: &mut Bencher) {
    let (_, fb_bytes) = load_test_data();
    let now = SystemTime::now();

    // Prepare configuration outside the benchmark
    let ufc = parse_flags_configuration(&fb_bytes).expect("Failed to parse UFC v2 configuration");

    // Precompile all regexes for better performance
    ufc.precompile_regexes()
        .expect("Failed to precompile regexes");

    // Use the same realistic test case - kill-switch flag
    let flag_name = "kill-switch";
    let mut attrs = HashMap::new();
    attrs.insert("country".into(), Attribute::String("France".into()));
    attrs.insert("age".into(), Attribute::Number(32.0));

    let subject = Subject::new(
        Attribute::String("french_user".into()),
        attrs.into_iter().collect(),
    );

    b.iter(|| {
        let _assignment =
            ufc.evaluate_flag(black_box(flag_name), black_box(&subject), black_box(now));
        let _ = black_box(_assignment);
    })
}

fn bench_ufc(c: &mut Criterion) {
    let mut group = c.benchmark_group("ufc_precomputed");
    group.bench_function("rules-based", benchmark_ufc_v1);
    group.bench_function("instruction-based", benchmark_instruction_based);
    group.finish();
}

fn bench_individual(c: &mut Criterion) {
    let mut group = c.benchmark_group("individual_evaluations");
    group.bench_function("rules-based", benchmark_individual_v1);
    group.bench_function("instruction-based", benchmark_individual_v2);
    group.finish();
}

fn bench_single_flag(c: &mut Criterion) {
    let mut group = c.benchmark_group("single_flag_killswitch");
    group.bench_function("rules-based", benchmark_single_flag_v1);
    group.bench_function("instruction-based", benchmark_single_flag_v2);
    group.finish();
}

// criterion_group!(benches, benchmark_ufc_v1, benchmark_instruction_based);
criterion_group!(benches, bench_ufc, bench_individual, bench_single_flag);
criterion_main!(benches);
