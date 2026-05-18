// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashMap, fs, path::PathBuf, sync::Arc};

use chrono::Utc;
use datadog_ffe::rules_based::{
    get_assignment, Attribute, Configuration, EvaluationContext, FlagType, Str, UniversalFlagConfig,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestCase {
    flag: String,
    variation_type: FlagType,
    default_value: serde_json::Value,
    targeting_key: Option<Str>,
    attributes: Arc<HashMap<Str, Attribute>>,
    result: TestResult,
}

#[derive(Debug, Deserialize)]
struct TestResult {
    value: serde_json::Value,
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ffe-system-test-data")
}

#[test]
#[cfg_attr(miri, ignore)] // this test is too slow on miri
fn evaluates_canonical_json_fixtures() {
    let _ = env_logger::builder().is_test(true).try_init();

    let root = fixture_root();
    let config_path = root.join("ufc-config.json");
    let cases_dir = root.join("evaluation-cases");

    let config = UniversalFlagConfig::from_json(fs::read(&config_path).unwrap()).unwrap();
    let config = Configuration::from_server_response(config);
    let now = Utc::now();

    let mut fixture_count = 0;
    for entry in fs::read_dir(&cases_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        fixture_count += 1;
        let test_cases: Vec<TestCase> = serde_json::from_reader(fs::File::open(&path).unwrap())
            .unwrap_or_else(|err| panic!("failed to parse fixture {}: {err}", path.display()));

        for test_case in test_cases {
            let subject = EvaluationContext::new(test_case.targeting_key, test_case.attributes);
            let result = get_assignment(
                Some(&config),
                &test_case.flag,
                &subject,
                test_case.variation_type.into(),
                now,
            );

            let actual = result
                .map(|assignment| assignment.value.variation_value())
                .unwrap_or(test_case.default_value);

            assert_eq!(
                actual,
                test_case.result.value,
                "unexpected value for flag {} in {}",
                test_case.flag,
                path.display()
            );
        }
    }

    assert!(fixture_count > 0, "no canonical FFE fixtures loaded");
}
